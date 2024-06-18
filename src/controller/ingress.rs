use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use futures_util::StreamExt;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{ConfigMap, Service},
    networking::v1::{
        Ingress, IngressLoadBalancerIngress, IngressLoadBalancerStatus, IngressStatus,
    },
};
use kube::{
    api::{ListParams, ObjectMeta, Patch, PatchParams},
    runtime::{controller::Action, finalizer, watcher, Controller},
    Api, ResourceExt,
};
use log::{info, warn};

use crate::{
    cloudflare::{dns::DnsContent, Client as CloudflareClient, TunnelConfig, TunnelIngress},
    context::Context,
    controller::utils::*,
    error::Error,
    ClusterTunnel,
};

use super::{error_policy, OPERATOR_MANAGER};

const INGRESS_FINALIZER: &'static str = "ingress.cloudflare-tunnels-operator.io/finalizer";

async fn patch_deployment(deploy_api: &Api<Deployment>, hash: String) -> Result<(), Error> {
    let annotations = serde_json::json!({
        "spec": {
            "template": {
                "metadata": {
                    "annotations" : {
                        ANNOTATION_CONFIG_HASH: hash
                    }
                }
            }
        }
    });

    deploy_api
        .patch(
            "cloudflared",
            &PatchParams::apply(OPERATOR_MANAGER),
            &Patch::Merge(&annotations),
        )
        .await?;

    Ok(())
}

pub async fn reconcile(obj: Arc<Ingress>, ctx: Arc<Context>) -> Result<Action, Error> {
    if obj
        .annotations()
        .get("kubernetes.io/ingress.class")
        .or(obj
            .spec
            .as_ref()
            .and_then(|spec| spec.ingress_class_name.as_ref()))
        .cloned()
        != ctx.ingress_class
    {
        return Ok(Action::await_change());
    }

    let ns = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".to_string());
    let client = ctx.kube_cli.clone();

    let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
    let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &ns);
    let ct_api: Api<ClusterTunnel> = Api::all(client.clone());

    let ing_ns = obj.namespace().unwrap_or_else(|| "default".to_string());
    let ing_api: Api<Ingress> = Api::namespaced(client.clone(), &ing_ns);
    let svc_api: Api<Service> = Api::namespaced(client.clone(), &ing_ns);

    let config_map = cm_api.get("cloudflared-config").await?;
    let mut config = config_map
        .data
        .as_ref()
        .and_then(|data| data.get("config.yaml"))
        .and_then(|cfg| serde_yaml::from_str::<TunnelConfig>(cfg).ok())
        .ok_or_else(|| anyhow!("no data"))?;

    let clustertunnels = ct_api.list(&ListParams::default()).await?;
    let Some(clustertunnel) = clustertunnels.items.first() else {
        return Err(anyhow!("no cluster tunnel available").into());
    };

    let cloudflare_creds =
        get_credentials(ctx.clone(), &ns, &clustertunnel.spec.cloudflare).await?;
    let cloudflare_client = CloudflareClient::new(
        clustertunnel.spec.cloudflare.account_id.clone(),
        cloudflare_creds,
    )?;

    finalizer(&ing_api, INGRESS_FINALIZER, obj, |event| async {
        match event {
            finalizer::Event::Apply(obj) => {
                let Some(spec) = obj.spec.as_ref() else {
                    return Ok(Action::requeue(Duration::from_secs(3600)));
                };

                for rule in spec.rules.iter().flatten() {
                    for ingress_path in rule
                        .http
                        .as_ref()
                        .map(|http| http.paths.clone())
                        .iter()
                        .flatten()
                    {
                        let path = if let Some(mut path) = ingress_path
                            .path
                            .as_ref()
                            .map(|p| format!("^{}", regex::escape(p)))
                        {
                            if ingress_path.path_type == "Exact" {
                                path = format!("{path}\\/?$");
                            }

                            Some(path)
                        } else {
                            None
                        };

                        let Some(svc) = ingress_path.backend.service.as_ref() else {
                            continue;
                        };

                        let Some(svc_port) = svc.port.as_ref() else {
                            continue;
                        };

                        let port = if let Some(port) = svc_port.number {
                            port
                        } else if let Some(name) = svc_port.name.as_ref() {
                            let svc = svc_api.get(&svc.name).await?;
                            let Some(svc_spec) = svc.spec.as_ref() else {
                                continue;
                            };
                            let Some(port) = svc_spec.ports.iter().flatten().find_map(|svc_port| {
                                (svc_port.name == Some(name.to_string())).then(|| svc_port.port)
                            }) else {
                                continue;
                            };

                            port
                        } else {
                            continue;
                        };

                        let service = format!(
                            "http://{}.{}.svc:{}",
                            svc.name,
                            obj.namespace().unwrap_or_else(|| "default".to_string()),
                            port
                        );

                        let ing = TunnelIngress {
                            hostname: rule.host.clone(),
                            path,
                            service: service.clone(),
                            origin_request: None,
                        };

                        if let Some(index) =
                            config.ingress.iter().position(|ing| ing.service == service)
                        {
                            config.ingress[index] = ing
                        } else if config.ingress.len() == 0 {
                            config.ingress.push(ing);
                            config.ingress.push(TunnelIngress {
                                service: "http_status:404".to_string(),
                                ..TunnelIngress::default()
                            });
                        } else {
                            config.ingress.insert(config.ingress.len() - 1, ing);
                        }
                    }

                    let hostname = match &rule.host {
                        Some(host) => host.clone(),
                        None => "@".to_string(),
                    };

                    let dns_record = cloudflare_client
                        .find_dns_record(&clustertunnel.spec.cloudflare.zone_id, &hostname)
                        .await?;

                    let cname = format!("{}.cfargotunnel.com", config.tunnel);
                    match dns_record {
                        Some(record) => match record.content {
                            DnsContent::CNAME { content } if content == cname => {
                                continue;
                            }
                            _ => {
                                cloudflare_client
                                    .update_dns_record(
                                        &clustertunnel.spec.cloudflare.zone_id,
                                        &record.id,
                                        &hostname,
                                        &config.tunnel,
                                    )
                                    .await?;
                            }
                        },
                        None => {
                            cloudflare_client
                                .create_dns_record(
                                    &clustertunnel.spec.cloudflare.zone_id,
                                    &hostname,
                                    &config.tunnel,
                                )
                                .await?;
                        }
                    }
                }

                let config_yaml = serde_yaml::to_string(&config).unwrap();
                let config_hash = sha256::digest(&config_yaml);

                let config_map = ConfigMap {
                    metadata: ObjectMeta {
                        managed_fields: None,
                        ..config_map.metadata.clone()
                    },
                    data: Some({
                        let mut map = BTreeMap::new();
                        map.insert("config.yaml".to_string(), config_yaml);
                        map
                    }),
                    ..config_map.clone()
                };

                cm_api
                    .patch(
                        &config_map.name_any(),
                        &PatchParams::apply(OPERATOR_MANAGER),
                        &Patch::Apply(&config_map),
                    )
                    .await?;

                patch_deployment(&deploy_api, config_hash).await?;

                let mut ing = ing_api.get_status(&obj.name_any()).await?;

                ing.status = Some(IngressStatus {
                    load_balancer: Some(IngressLoadBalancerStatus {
                        ingress: Some(vec![IngressLoadBalancerIngress {
                            hostname: Some(format!("{}.cfargotunnel.com", config.tunnel)),
                            ..IngressLoadBalancerIngress::default()
                        }]),
                    }),
                });

                ing_api
                    .patch_status(
                        &ing.name_any(),
                        &PatchParams::apply(OPERATOR_MANAGER),
                        &Patch::Merge(ing),
                    )
                    .await?;

                Ok(Action::requeue(Duration::from_secs(3600)))
            }
            finalizer::Event::Cleanup(obj) => {
                let Some(spec) = obj.spec.as_ref() else {
                    return Ok(Action::requeue(Duration::from_secs(3600)));
                };

                for rule in spec.rules.iter().flatten() {
                    for ingress_path in rule
                        .http
                        .as_ref()
                        .map(|http| http.paths.clone())
                        .iter()
                        .flatten()
                    {
                        let Some(svc) = ingress_path.backend.service.as_ref() else {
                            continue;
                        };

                        config.ingress = config
                            .ingress
                            .into_iter()
                            .filter(|ing| !ing.service.contains(&svc.name))
                            .collect();
                    }

                    let hostname = match &rule.host {
                        Some(host) => host.clone(),
                        None => "@".to_string(),
                    };

                    let Some(dns_record) = cloudflare_client
                        .find_dns_record(&clustertunnel.spec.cloudflare.zone_id, &hostname)
                        .await?
                    else {
                        continue;
                    };

                    cloudflare_client
                        .delete_dns_record(&clustertunnel.spec.cloudflare.zone_id, &dns_record.id)
                        .await?;
                }

                let config_yaml = serde_yaml::to_string(&config).unwrap();
                let config_hash = sha256::digest(&config_yaml);

                let config_map = ConfigMap {
                    metadata: ObjectMeta {
                        managed_fields: None,
                        ..config_map.metadata.clone()
                    },
                    data: Some({
                        let mut map = BTreeMap::new();
                        map.insert("config.yaml".to_string(), config_yaml);
                        map
                    }),
                    ..config_map.clone()
                };

                cm_api
                    .patch(
                        &config_map.name_any(),
                        &PatchParams::apply(OPERATOR_MANAGER),
                        &Patch::Apply(&config_map),
                    )
                    .await?;

                patch_deployment(&deploy_api, config_hash).await?;

                Ok(Action::requeue(Duration::from_secs(3600)))
            }
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

pub async fn run(ctx: Arc<Context>) -> anyhow::Result<()> {
    let client = ctx.kube_cli.clone();

    let cfg = watcher::Config::default();
    let ing_api: Api<Ingress> = Api::all(client.clone());

    Controller::new(ing_api, cfg)
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled ingress {o:?}"),
                Err(e) => warn!("reconcile ingress failed: {e:?}"),
            }
        })
        .await;

    Ok(())
}
