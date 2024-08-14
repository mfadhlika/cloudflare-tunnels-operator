use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use futures_util::StreamExt;
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            ConfigMap, ConfigMapVolumeSource, Container, HTTPGetAction, PodSpec, PodTemplateSpec,
            Probe, Secret, SecretVolumeSource, Volume, VolumeMount,
        },
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{
    api::{ObjectMeta, Patch, PatchParams},
    runtime::{controller::Action, finalizer, watcher, Controller},
    Api, CustomResource, ResourceExt,
};
use log::{info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    cloudflare::{self, TunnelConfig, TunnelCredentials, TunnelIngress},
    context::Context,
    error::Error,
};

use super::{error_policy, utils::*, OPERATOR_MANAGER};

const CLUSTER_TUNNEL_FINALIZER: &'static str = "cluster-tunnel.cloudflare-tunnels.io/finalizer";

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub name: String,
    pub key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CloudflareSecretRef {
    #[serde(rename = "apiKeySecretRef")]
    ApiKey(SecretRef),
    #[serde(rename = "apiTokenSecretRef")]
    ApiToken(SecretRef),
}

impl CloudflareSecretRef {
    pub fn secret_ref(&self) -> &SecretRef {
        match self {
            CloudflareSecretRef::ApiKey(secret_ref) => secret_ref,
            CloudflareSecretRef::ApiToken(secret_ref) => secret_ref,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloudflareCredentials {
    pub account_id: String,
    pub zone_id: String,
    pub email: Option<String>,
    #[serde(flatten)]
    pub secret_ref: CloudflareSecretRef,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    kind = "ClusterTunnel",
    group = "cloudflare-tunnels-operator.io",
    version = "v1alpha1"
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterTunnelSpec {
    pub name: Option<String>,
    pub tunnel_secret_ref: Option<SecretRef>,
    pub cloudflare: CloudflareCredentials,
}

impl ClusterTunnel {
    async fn deploy_cloudflared(
        &self,
        ctx: Arc<Context>,
        creds: &TunnelCredentials,
    ) -> Result<(), Error> {
        let oref = self.owner_references();
        let ns = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".to_string());
        let client = ctx.kube_cli.clone();

        let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
        let secret_api: Api<Secret> = Api::namespaced(client.clone(), &ns);
        let deploy_api: Api<Deployment> = Api::namespaced(client.clone(), &ns);

        let tunnel_name = self.spec.name.clone().unwrap_or_else(|| self.name_any());

        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/part-of".to_string(),
            "cloudflare-tunnels-operator".to_string(),
        );
        labels.insert(
            "app.kubernetes.io/name".to_string(),
            "cloudflared".to_string(),
        );

        let creds_json = serde_json::to_string(creds).unwrap();

        let (secret_name, secret_key) = if let Some(secret_ref) = self.spec.tunnel_secret_ref.as_ref() {
            (secret_ref.name.clone(), Some(secret_ref.key.clone()))
        } else {
            let secret_name = format!("cloudflared-{tunnel_name}-credentials");
            let secret = Secret {
                metadata: ObjectMeta {
                    name: Some(secret_name.clone()),
                    namespace: Some(ns.to_owned()),
                    owner_references: Some(oref.to_vec()),
                    ..ObjectMeta::default()
                },
                string_data: Some({
                    let mut map = BTreeMap::new();
                    map.insert("credentials.json".to_string(), creds_json.clone());
                    map
                }),
                ..Default::default()
            };

            secret_api
            .patch(
                &secret.name_any(),
                &PatchParams::apply(OPERATOR_MANAGER),
                &Patch::Apply(&secret),
            )
            .await?;

            (secret_name, Some("credentials.json".to_string()))
        };

        let config_name = format!("cloudflared-{tunnel_name}-config");
        let config = cm_api
            .get_opt(&config_name)
            .await?
            .and_then(|cm| cm.data)
            .and_then(|data| data.get("config.yaml").cloned())
            .map(|config| serde_yaml::from_str(&config).unwrap())
            .unwrap_or_else(|| TunnelConfig {
                tunnel: creds.tunnel_id.clone(),
                credentials_file: "/credentials/credentials.json".to_string(),
                ingress: vec![TunnelIngress {
                    service: "http_status:404".to_string(),
                    ..TunnelIngress::default()
                }],
                ..TunnelConfig::default()
            });

        let config_yaml = serde_yaml::to_string(&config).unwrap();
        let config_hash = sha256::digest(&config_yaml);

        let config_map = ConfigMap {
            metadata: ObjectMeta {
                name: Some(config_name.to_string()),
                namespace: Some(ns.to_owned()),
                owner_references: Some(oref.to_vec()),
                ..ObjectMeta::default()
            },
            data: Some({
                let mut map = BTreeMap::new();
                map.insert("config.yaml".to_string(), config_yaml);
                map
            }),
            ..ConfigMap::default()
        };

        cm_api
            .patch(
                &config_map.name_any(),
                &PatchParams::apply(OPERATOR_MANAGER),
                &Patch::Apply(&config_map),
            )
            .await?;

        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some("cloudflared".to_string()),
                namespace: Some(ns.to_owned()),
                owner_references: Some(oref.to_vec()),
                labels: Some(labels.clone()),
                ..ObjectMeta::default()
            },
            spec: Some(DeploymentSpec {
                selector: LabelSelector {
                    match_labels: Some(labels.clone()),
                    ..LabelSelector::default()
                },
                template: PodTemplateSpec {
                    metadata: Some(ObjectMeta {
                        labels: Some(labels.clone()),
                        annotations: Some({
                            let mut map = BTreeMap::new();
                            map.insert(ANNOTATION_CONFIG_HASH.to_string(), config_hash);
                            map
                        }),
                        ..ObjectMeta::default()
                    }),
                    spec: Some(PodSpec {
                        volumes: Some(vec![
                            Volume {
                                name: "config".to_string(),
                                config_map: Some(ConfigMapVolumeSource {
                                    name: Some(config_name.to_string()),
                                    ..ConfigMapVolumeSource::default()
                                }),
                                ..Volume::default()
                            },
                            Volume {
                                name: "credentials".to_string(),
                                secret: Some(SecretVolumeSource {
                                    secret_name: Some(secret_name),
                                    ..SecretVolumeSource::default()
                                }),
                                ..Volume::default()
                            },
                        ]),
                        containers: vec![Container {
                            name: "cloudflared".to_string(),
                            image: Some("cloudflare/cloudflared:2024.8.2".to_string()),
                            args: Some(vec![
                                "tunnel".to_string(),
                                "--no-autoupdate".to_string(),
                                "--metrics".to_string(),
                                "0.0.0.0:2000".to_string(),
                                "--config".to_string(),
                                "/config/config.yaml".to_string(),
                                "run".to_string(),
                                config.tunnel.clone(),
                            ]),
                            volume_mounts: Some(vec![
                                VolumeMount {
                                    name: "config".to_string(),
                                    mount_path: "/config".to_string(),
                                    ..VolumeMount::default()
                                },
                                VolumeMount {
                                    name: "credentials".to_string(),
                                    mount_path: "/credentials/credentials.json".to_string(),
                                    sub_path: secret_key,
                                    ..VolumeMount::default()
                                },
                            ]),
                            liveness_probe: Some(Probe {
                                http_get: Some(HTTPGetAction {
                                    path: Some("/ready".to_string()),
                                    port: IntOrString::Int(2000),
                                    ..HTTPGetAction::default()
                                }),
                                failure_threshold: Some(1),
                                initial_delay_seconds: Some(10),
                                period_seconds: Some(10),
                                ..Probe::default()
                            }),
                            ..Container::default()
                        }],
                        ..PodSpec::default()
                    }),
                    ..PodTemplateSpec::default()
                },
                ..DeploymentSpec::default()
            }),
            ..Deployment::default()
        };

        deploy_api
            .patch(
                &deployment.name_any(),
                &PatchParams::apply(OPERATOR_MANAGER),
                &Patch::Apply(&deployment),
            )
            .await?;

        Ok(())
    }

    pub async fn get_credentials(
        &self,
        ctx: Arc<Context>,
    ) -> Result<cloudflare::Credentials, Error> {
        let ns = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".to_string());
        let kube_cli = ctx.kube_cli.clone();

        let secret_api: Api<Secret> = Api::namespaced(kube_cli.clone(), &ns);

        let secret_ref = match &self.spec.cloudflare.secret_ref {
            CloudflareSecretRef::ApiKey(secret_ref) => secret_ref,
            CloudflareSecretRef::ApiToken(secret_ref) => secret_ref,
        };

        let secret = secret_api.get(&secret_ref.name).await?;
        let data = secret.data.ok_or_else(|| anyhow!("no data"))?;
        let value = data.get(&secret_ref.key).ok_or_else(|| {
            anyhow!(
                "key {} not found or invalid in {}",
                secret_ref.key,
                secret_ref.name
            )
        })?;

        let value = String::from_utf8(value.clone().0)
            .map_err(|err| anyhow!("value not a string: {err:?}"))?;

        let creds = match &self.spec.cloudflare.secret_ref {
            CloudflareSecretRef::ApiKey(_) => {
                let Some(email) = &self.spec.cloudflare.email else {
                    return Err(anyhow!("api key requires email").into());
                };

                cloudflare::Credentials::UserAuthKey {
                    email: email.to_owned(),
                    key: value,
                }
            }
            CloudflareSecretRef::ApiToken(_) => {
                cloudflare::Credentials::UserAuthToken { token: value }
            }
        };

        Ok(creds)
    }

    pub async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action, Error> {
        let credentials = self.get_credentials(ctx.clone()).await?;

        let cf_cli = cloudflare::Client::new(self.spec.cloudflare.account_id.clone(), credentials)?;

        let tunnel_name = self.spec.name.clone().unwrap_or_else(|| self.name_any());
        let tunnel_credentials = if let Some(tunnel_id) = cf_cli.find_tunnel(&tunnel_name).await? {
            info!("tunnel found: {tunnel_id}");

            let client = ctx.kube_cli.clone();
            let ns = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "default".to_string());
            let secret_api: Api<Secret> = Api::namespaced(client.clone(), &ns);

            let secret_ref = self
                .spec
                .tunnel_secret_ref
                .clone()
                .unwrap_or_else(|| SecretRef {
                    name: format!("cloudflared-{tunnel_name}-credentials"),
                    key: "credentials.json".to_string(),
                });

            let secret = secret_api.get(&secret_ref.name).await?;
            let data = secret.data.ok_or_else(|| anyhow!("no data"))?;
            let creds = data
                .get(&secret_ref.key)
                .ok_or_else(|| anyhow!("no credentials"))?;
            serde_json::from_slice(&creds.0)
                .map_err(|err| anyhow!("failed to deserialize credentials: {err:?}"))?
        } else {
            info!("tunnel not found, creating...");

            let tunnel_name = self.spec.name.clone().unwrap_or_else(|| self.name_any());
            cf_cli.create_tunnel(&tunnel_name).await?
        };

        self.deploy_cloudflared(ctx.clone(), &tunnel_credentials)
            .await?;

        Ok(Action::requeue(Duration::from_secs(3600)))
    }

    pub async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action, Error> {
        let credentials = self.get_credentials(ctx.clone()).await?;

        let cf_cli = cloudflare::Client::new(self.spec.cloudflare.account_id.clone(), credentials)?;

        let tunnel_name = self.spec.name.clone().unwrap_or_else(|| self.name_any());
        let Some(tunnel_id) = cf_cli.find_tunnel(&tunnel_name).await? else {
            return Ok(Action::requeue(Duration::from_secs(3600)));
        };

        cf_cli.delete_tunnel(&tunnel_id).await?;

        Ok(Action::requeue(Duration::from_secs(3600)))
    }
}

pub async fn reconcile(obj: Arc<ClusterTunnel>, ctx: Arc<Context>) -> Result<Action, Error> {
    let client = ctx.kube_cli.clone();

    let ct_api: Api<ClusterTunnel> = Api::all(client);
    finalizer(&ct_api, CLUSTER_TUNNEL_FINALIZER, obj, |event| async {
        match event {
            finalizer::Event::Apply(obj) => obj.reconcile(ctx.clone()).await,
            finalizer::Event::Cleanup(obj) => obj.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

pub async fn run(ctx: Arc<Context>) -> anyhow::Result<()> {
    let client = ctx.kube_cli.clone();

    let cfg = watcher::Config::default();
    let ct_api: Api<ClusterTunnel> = Api::all(client.clone());

    Controller::new(ct_api, cfg)
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled cluster tunnel {o:?}"),
                Err(e) => warn!("reconcile cluster tunnel failed: {e:?}"),
            }
        })
        .await;

    Ok(())
}
