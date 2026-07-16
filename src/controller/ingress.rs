use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use futures_util::StreamExt;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{ConfigMap, Service},
    networking::v1::{
        Ingress, IngressLoadBalancerIngress, IngressLoadBalancerStatus, IngressRule, IngressStatus,
    },
};
use kube::{
    Api, ResourceExt,
    api::{ListParams, ObjectMeta, Patch, PatchParams},
    runtime::{Controller, controller::Action, finalizer, watcher},
};
use log::{error, info, warn};

use crate::{
    ClusterTunnel,
    cloudflare::{Client as CloudflareClient, TunnelConfig, TunnelIngress, dns::dns::DnsContent},
    context::Context,
    controller::utils::*,
    error::Error,
};

use super::{OPERATOR_MANAGER, error_policy};

const INGRESS_FINALIZER: &str = "ingress.cloudflare-tunnels-operator.io/finalizer";

async fn patch_deployment(deploy_api: &Api<Deployment>, hash: String) -> Result<(), Error> {
    let patch: json_patch::Patch = serde_json::from_value(serde_json::json!([
        {
            "op": "replace", 
            "path": format!("/spec/template/metadata/annotations/{}", ANNOTATION_CONFIG_HASH.replace("/", "~1")), 
            "value": hash 
        },
      ])).map_err(|err|Error::Other(anyhow!("parse patch: {err}")))?;

    deploy_api
        .patch(
            "cloudflared",
            &PatchParams::apply(OPERATOR_MANAGER),
            &Patch::Json::<()>(patch),
        )
        .await?;

    Ok(())
}

async fn upsert_dns_record(
    rule: &IngressRule,
    cloudflare_client: &CloudflareClient,
    cname: &str,
    txt: &str,
) -> Result<(), Error> {
    let hostname = match &rule.host {
        Some(host) => host.to_string(),
        None => "@".to_string(),
    };

    // list all dns record with the hostname
    let dns_records = cloudflare_client.find_dns_record(&hostname).await?;

    // find cname and all txt records
    let mut cname_record = None;
    let mut txt_record_found = false;
    let mut other_txt_record_found = false;
    for record in dns_records {
        match record.content {
            DnsContent::CNAME { .. } => cname_record = Some(record),
            DnsContent::TXT { content } => {
                if content == txt {
                    txt_record_found = true;
                } else {
                    other_txt_record_found = true;
                }
            }
            _ => {}
        }
    }

    match cname_record {
        Some(record) => match record.content {
            DnsContent::CNAME { content } => {
                if content == cname && !other_txt_record_found {
                    if !txt_record_found {
                        cloudflare_client.create_txt_record(&hostname, &txt).await?;
                    }
                } else if content != cname && !other_txt_record_found {
                    cloudflare_client
                        .update_cname_record(&record.id, &hostname, cname)
                        .await?;
                } else {
                    return Err(Error::Other(anyhow!(
                        "CNAME record set to another tunnel, maybe set by another tunnel. If you think that's not the case, manually delete the record from Cloudflare dashboard"
                    )));
                }
            }
            _ => {}
        },
        None => {
            if !txt_record_found {
                cloudflare_client.create_txt_record(&hostname, &txt).await?;
            }

            cloudflare_client
                .create_cname_record(&hostname, cname)
                .await?;
        }
    }

    Ok(())
}

async fn cleanup_dns_records(
    rule: &IngressRule,
    cloudflare_client: &CloudflareClient,
    txt: &str,
) -> Result<(), Error> {
    let hostname = match &rule.host {
        Some(host) => host.to_string(),
        None => "@".to_string(),
    };

    // list all dns record with the hostname
    let dns_records = cloudflare_client.find_dns_record(&hostname).await?;

    // find cname and all txt records
    let mut cname_record = None;
    let mut txt_record = None;
    let mut txt_record_count = 0;
    for record in dns_records.iter() {
        match &record.content {
            DnsContent::CNAME { .. } => cname_record = Some(record),
            DnsContent::TXT { content } => {
                txt_record_count += 1;
                if content == txt {
                    txt_record = Some(record);
                }
            }
            _ => {}
        }
    }

    // if txt record found and match, delete the record
    if let Some(record) = txt_record {
        cloudflare_client.delete_dns_record(&record.id).await?;
        txt_record_count -= 1;
    }

    // if there's no txt record anymore, delete the cname record
    if txt_record_count == 0 {
        if let Some(record) = cname_record {
            cloudflare_client.delete_dns_record(&record.id).await?;
        }
    }

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

    let tunnel_name = if let Some(tunnel_name) = obj
        .metadata
        .annotations
        .as_ref()
        .and_then(|ann| ann.get(ANNOTATION_TUNNEL_NAME))
    {
        tunnel_name.to_owned()
    } else if let Some(tunnel) = ct_api.list(&ListParams::default()).await?.items.first() {
        tunnel
            .spec
            .name
            .clone()
            .unwrap_or_else(|| tunnel.name_any())
    } else {
        return Err(Error::Other(anyhow!("no clustertunnel found")));
    };
    let config_name = format!("cloudflared-{tunnel_name}-config");
    let config_map = cm_api.get(&config_name).await?;
    let mut config = config_map
        .data
        .as_ref()
        .and_then(|data| data.get("config.yaml"))
        .and_then(|cfg| serde_yaml::from_str::<TunnelConfig>(cfg).ok())
        .ok_or_else(|| anyhow!("no data"))?;

    let cloudflare_client = &ctx.cloudflare_client;

    let name = "cloudflare-tunnels-operator".to_string();
    let owner = ctx.owner.clone().unwrap_or("default".to_string());
    let txt_record_content = format!(
        "heritage={name},{name}/owner={owner},{name}/resource=ingress/{}/{}",
        obj.metadata.namespace.as_ref().unwrap(),
        obj.metadata.name.as_ref().unwrap()
    );
    let cname_record_content = format!("{}.cfargotunnel.com", config.tunnel);

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
                                (svc_port.name == Some(name.to_string())).then_some(svc_port.port)
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
                        } else if config.ingress.is_empty() {
                            config.ingress.push(ing);
                            config.ingress.push(TunnelIngress {
                                service: "http_status:404".to_string(),
                                ..TunnelIngress::default()
                            });
                        } else {
                            config.ingress.insert(config.ingress.len() - 1, ing);
                        }
                    }

                    if !ctx.disable_dns.unwrap_or_default() {
                        if let Err(err) = upsert_dns_record(
                            rule,
                            &cloudflare_client,
                            &cname_record_content,
                            &txt_record_content,
                        )
                        .await
                        {
                            error!("failed to create or update dns record: {err}");
                        }
                    }
                }

                let config_yaml = serde_yaml::to_string(&config).unwrap();
                let config_hash = sha256::digest(&config_yaml);

                /*
                name: Some(config_name.to_string()),
                namespace: Some(ns.to_owned()),
                owner_references: Some(oref.to_vec()),
                 */
                let config_map = ConfigMap {
                    metadata: ObjectMeta {
                        name: Some(config_map.name_any()),
                        namespace: config_map.namespace(),
                        owner_references: Some(config_map.owner_references().to_vec()),
                        ..ObjectMeta::default()
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

                        config
                            .ingress
                            .retain(|ing| !ing.service.contains(&svc.name));
                    }

                    if !ctx.disable_dns.unwrap_or_default() {
                        if let Err(err) =
                            cleanup_dns_records(rule, &cloudflare_client, &txt_record_content).await
                        {
                            error!("failed to clean up dns records: {err}");
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

#[cfg(test)]
mod tests {
    use k8s_openapi::api::networking::v1::{
        HTTPIngressPath, HTTPIngressRuleValue, IngressBackend, IngressServiceBackend,
        ServiceBackendPort,
    };
    use mockito::{Matcher, Mock, ServerGuard};
    use serde_json::json;

    use super::*;

    async fn setup_create_dns_mock(
        server: &mut ServerGuard,
        record_type: &str,
        content: &str,
    ) -> Mock {
        return server
            .mock("POST", "/zones/test-zone/dns_records")
            .match_body(Matcher::Json(json!({
                "proxied": true,
                "name": "test.example.com",
                "type": record_type,
                "content": content
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": {
                        "name": "test.example.com",
                        "ttl": 3600,
                        "type": record_type,
                        "comment": "Domain verification record",
                        "content": content,
                        "private_routing": true,
                        "proxied": true,
                        "settings": {
                          "ipv4_only": true,
                          "ipv6_only": true
                        },
                        "tags": [
                          "owner:dns-team"
                        ],
                        "id": "023e105f4ecef8ad9ca31a8372d0c353",
                        "created_on": "2014-01-01T05:20:00.12345Z",
                        "meta": {
                          "dead_glue": true,
                          "is_glue": true,
                          "shadowed_by": [
                            "372e67954025e0ba6aaa6d586b9e0b59"
                          ],
                          "shadowed_records_count": 42
                        },
                        "modified_on": "2014-01-01T05:20:00.12345Z",
                        "proxiable": true,
                        "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                        "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;
    }

    #[tokio::test]
    async fn test_upsert_dns_record_on_empty_record() {
        let cname = "test-cname";
        let txt = "test-txt";

        let mut server = mockito::Server::new_async().await;

        // Use one of these addresses to configure your client
        let url = server.url();

        // Create a mock
        let _ = server
            .mock("GET", "/zones/test-zone/dns_records?name=test.example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": []
                })
                .to_string(),
            )
            .create_async()
            .await;

        let create_txt_mock = setup_create_dns_mock(&mut server, "TXT", txt).await;

        let create_cname_mock = setup_create_dns_mock(&mut server, "CNAME", cname).await;

        let cloudflare_client = crate::cloudflare::Client::new(
            "test-account".to_string(),
            "test-zone".to_string(),
            crate::cloudflare::Credentials::UserAuthToken {
                token: "token".to_string(),
            },
            crate::cloudflare::Environment::Custom(url),
        )
        .unwrap();

        let rule = IngressRule {
            host: Some("test.example.com".to_string()),
            http: Some(HTTPIngressRuleValue {
                paths: vec![HTTPIngressPath {
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: "test".to_string(),
                            port: Some(ServiceBackendPort {
                                name: Some("http".to_string()),
                                ..Default::default()
                            }),
                        }),
                        ..Default::default()
                    },
                    path: Some("/".to_string()),
                    path_type: "Prefix".to_string(),
                }],
            }),
        };

        if let Err(err) = upsert_dns_record(&rule, &cloudflare_client, cname, txt).await {
            assert!(false, "failed to upsert dns record: {err:?}");
        }

        create_txt_mock.assert_async().await;
        create_cname_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_upsert_dns_record_on_missing_txt_record() {
        let cname = "test-cname";
        let txt = "test-txt";

        let mut server = mockito::Server::new_async().await;

        // Use one of these addresses to configure your client
        let url = server.url();

        // Create a mock
        let _ = server
            .mock("GET", "/zones/test-zone/dns_records?name=test.example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": [
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "CNAME",
                          "comment": "Domain verification record",
                          "content": cname,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        }
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let create_txt_mock = setup_create_dns_mock(&mut server, "TXT", txt).await;

        let cloudflare_client = crate::cloudflare::Client::new(
            "test-account".to_string(),
            "test-zone".to_string(),
            crate::cloudflare::Credentials::UserAuthToken {
                token: "token".to_string(),
            },
            crate::cloudflare::Environment::Custom(url),
        )
        .unwrap();

        let rule = IngressRule {
            host: Some("test.example.com".to_string()),
            http: Some(HTTPIngressRuleValue {
                paths: vec![HTTPIngressPath {
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: "test".to_string(),
                            port: Some(ServiceBackendPort {
                                name: Some("http".to_string()),
                                ..Default::default()
                            }),
                        }),
                        ..Default::default()
                    },
                    path: Some("/".to_string()),
                    path_type: "Prefix".to_string(),
                }],
            }),
        };

        if let Err(err) = upsert_dns_record(&rule, &cloudflare_client, cname, txt).await {
            assert!(false, "failed to upsert dns record: {err:?}");
        }

        create_txt_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_upsert_dns_record_on_missing_cname_record() {
        let cname = "test-cname";
        let txt = "test-txt";

        let mut server = mockito::Server::new_async().await;

        // Use one of these addresses to configure your client
        let url = server.url();

        // Create a mock
        let _ = server
            .mock("GET", "/zones/test-zone/dns_records?name=test.example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": [
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "TXT",
                          "comment": "Domain verification record",
                          "content": txt,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        }
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let create_cname_mock = setup_create_dns_mock(&mut server, "CNAME", cname).await;

        let cloudflare_client = crate::cloudflare::Client::new(
            "test-account".to_string(),
            "test-zone".to_string(),
            crate::cloudflare::Credentials::UserAuthToken {
                token: "token".to_string(),
            },
            crate::cloudflare::Environment::Custom(url),
        )
        .unwrap();

        let rule = IngressRule {
            host: Some("test.example.com".to_string()),
            http: Some(HTTPIngressRuleValue {
                paths: vec![HTTPIngressPath {
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: "test".to_string(),
                            port: Some(ServiceBackendPort {
                                name: Some("http".to_string()),
                                ..Default::default()
                            }),
                        }),
                        ..Default::default()
                    },
                    path: Some("/".to_string()),
                    path_type: "Prefix".to_string(),
                }],
            }),
        };

        if let Err(err) = upsert_dns_record(&rule, &cloudflare_client, cname, txt).await {
            assert!(false, "failed to upsert dns record: {err:?}");
        }

        create_cname_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_upsert_dns_record_on_existing_records() {
        let cname = "test-cname";
        let txt = "test-txt";

        let mut server = mockito::Server::new_async().await;

        // Use one of these addresses to configure your client
        let url = server.url();

        // Create a mock
        let _ = server
            .mock("GET", "/zones/test-zone/dns_records?name=test.example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": [
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "CNAME",
                          "comment": "Domain verification record",
                          "content": cname,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        },
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "TXT",
                          "comment": "Domain verification record",
                          "content": txt,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        }
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let cloudflare_client = crate::cloudflare::Client::new(
            "test-account".to_string(),
            "test-zone".to_string(),
            crate::cloudflare::Credentials::UserAuthToken {
                token: "token".to_string(),
            },
            crate::cloudflare::Environment::Custom(url),
        )
        .unwrap();

        let rule = IngressRule {
            host: Some("test.example.com".to_string()),
            http: Some(HTTPIngressRuleValue {
                paths: vec![HTTPIngressPath {
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: "test".to_string(),
                            port: Some(ServiceBackendPort {
                                name: Some("http".to_string()),
                                ..Default::default()
                            }),
                        }),
                        ..Default::default()
                    },
                    path: Some("/".to_string()),
                    path_type: "Prefix".to_string(),
                }],
            }),
        };

        if let Err(err) = upsert_dns_record(&rule, &cloudflare_client, cname, txt).await {
            assert!(false, "failed to upsert dns record: {err:?}");
        }
    }

    #[tokio::test]
    async fn test_upsert_dns_record_on_another_tunnel() {
        let cname = "test-cname";
        let txt = "test-txt";

        let mut server = mockito::Server::new_async().await;

        // Use one of these addresses to configure your client
        let url = server.url();

        // Create a mock
        let _ = server
            .mock("GET", "/zones/test-zone/dns_records?name=test.example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": [
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "CNAME",
                          "comment": "Domain verification record",
                          "content": "different-cname",
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        },
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "TXT",
                          "comment": "Domain verification record",
                          "content": "different-txt",
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        }
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let cloudflare_client = crate::cloudflare::Client::new(
            "test-account".to_string(),
            "test-zone".to_string(),
            crate::cloudflare::Credentials::UserAuthToken {
                token: "token".to_string(),
            },
            crate::cloudflare::Environment::Custom(url),
        )
        .unwrap();

        let rule = IngressRule {
            host: Some("test.example.com".to_string()),
            http: Some(HTTPIngressRuleValue {
                paths: vec![HTTPIngressPath {
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: "test".to_string(),
                            port: Some(ServiceBackendPort {
                                name: Some("http".to_string()),
                                ..Default::default()
                            }),
                        }),
                        ..Default::default()
                    },
                    path: Some("/".to_string()),
                    path_type: "Prefix".to_string(),
                }],
            }),
        };

        let res = upsert_dns_record(&rule, &cloudflare_client, cname, txt).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_delete_dns_record() {
        let cname = "test-cname";
        let txt = "test-txt";

        let mut server = mockito::Server::new_async().await;

        // Use one of these addresses to configure your client
        let url = server.url();

        // Create a mock
        let _ = server
            .mock("GET", "/zones/test-zone/dns_records?name=test.example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": [
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "CNAME",
                          "comment": "Domain verification record",
                          "content": cname,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        },
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "TXT",
                          "comment": "Domain verification record",
                          "content": txt,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c354",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        }
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let delete_cname_mock = server
            .mock(
                "DELETE",
                "/zones/test-zone/dns_records/023e105f4ecef8ad9ca31a8372d0c353",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "result": {
                      "id": "023e105f4ecef8ad9ca31a8372d0c353"
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let delete_txt_mock = server
            .mock(
                "DELETE",
                "/zones/test-zone/dns_records/023e105f4ecef8ad9ca31a8372d0c354",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "result": {
                      "id": "023e105f4ecef8ad9ca31a8372d0c353"
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let cloudflare_client = crate::cloudflare::Client::new(
            "test-account".to_string(),
            "test-zone".to_string(),
            crate::cloudflare::Credentials::UserAuthToken {
                token: "token".to_string(),
            },
            crate::cloudflare::Environment::Custom(url),
        )
        .unwrap();

        let rule = IngressRule {
            host: Some("test.example.com".to_string()),
            http: Some(HTTPIngressRuleValue {
                paths: vec![HTTPIngressPath {
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: "test".to_string(),
                            port: Some(ServiceBackendPort {
                                name: Some("http".to_string()),
                                ..Default::default()
                            }),
                        }),
                        ..Default::default()
                    },
                    path: Some("/".to_string()),
                    path_type: "Prefix".to_string(),
                }],
            }),
        };

        if let Err(err) = cleanup_dns_records(&rule, &cloudflare_client, txt).await {
            assert!(false, "failed to cleanup dns record: {err:?}");
        }

        delete_cname_mock.assert_async().await;
        delete_txt_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_delete_dns_record_on_multiple_txt_records() {
        let cname = "test-cname";
        let txt = "test-txt";

        let mut server = mockito::Server::new_async().await;

        // Use one of these addresses to configure your client
        let url = server.url();

        // Create a mock
        let _ = server
            .mock("GET", "/zones/test-zone/dns_records?name=test.example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "errors": [],
                    "messages": [],
                    "success": true,
                    "result": [
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "CNAME",
                          "comment": "Domain verification record",
                          "content": cname,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c353",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        },
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "TXT",
                          "comment": "Domain verification record",
                          "content": txt,
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c354",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        },
                        {
                          "name": "text.example.com",
                          "ttl": 3600,
                          "type": "TXT",
                          "comment": "Domain verification record",
                          "content": "different-txt",
                          "private_routing": true,
                          "proxied": true,
                          "settings": {
                            "ipv4_only": true,
                            "ipv6_only": true
                          },
                          "tags": [
                            "owner:dns-team"
                          ],
                          "id": "023e105f4ecef8ad9ca31a8372d0c355",
                          "created_on": "2014-01-01T05:20:00.12345Z",
                          "meta": {
                            "dead_glue": true,
                            "is_glue": true,
                            "shadowed_by": [
                              "372e67954025e0ba6aaa6d586b9e0b59"
                            ],
                            "shadowed_records_count": 42
                          },
                          "modified_on": "2014-01-01T05:20:00.12345Z",
                          "proxiable": true,
                          "comment_modified_on": "2024-01-01T05:20:00.12345Z",
                          "tags_modified_on": "2025-01-01T05:20:00.12345Z"
                        }
                    ]
                })
                .to_string(),
            )
            .create_async()
            .await;

        let delete_txt_mock = server
            .mock(
                "DELETE",
                "/zones/test-zone/dns_records/023e105f4ecef8ad9ca31a8372d0c354",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "result": {
                      "id": "023e105f4ecef8ad9ca31a8372d0c354"
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let cloudflare_client = crate::cloudflare::Client::new(
            "test-account".to_string(),
            "test-zone".to_string(),
            crate::cloudflare::Credentials::UserAuthToken {
                token: "token".to_string(),
            },
            crate::cloudflare::Environment::Custom(url),
        )
        .unwrap();

        let rule = IngressRule {
            host: Some("test.example.com".to_string()),
            http: Some(HTTPIngressRuleValue {
                paths: vec![HTTPIngressPath {
                    backend: IngressBackend {
                        service: Some(IngressServiceBackend {
                            name: "test".to_string(),
                            port: Some(ServiceBackendPort {
                                name: Some("http".to_string()),
                                ..Default::default()
                            }),
                        }),
                        ..Default::default()
                    },
                    path: Some("/".to_string()),
                    path_type: "Prefix".to_string(),
                }],
            }),
        };

        if let Err(err) = cleanup_dns_records(&rule, &cloudflare_client, txt).await {
            assert!(false, "failed to cleanup dns record: {err:?}");
        }

        delete_txt_mock.assert_async().await;
    }
}
