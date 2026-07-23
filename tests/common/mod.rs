use std::{collections::BTreeMap, fmt::Debug, sync::Arc, time::Duration};

use cloudflare_tunnels_operator::{Context, controller};
use k8s_openapi::{
    api::networking::v1::{IngressClass, IngressClassSpec},
    apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
};
use kube::{
    Api, CustomResourceExt,
    api::{ObjectMeta, PostParams},
};
use mockito::{Matcher, Mock, ServerGuard};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::oneshot::Receiver;

pub async fn setup_list_dns_mock(
    server: &mut ServerGuard,
    zone_id: &str,
    name: &str,
    records: Vec<(String, String, String)>,
) -> Mock {
    let results = records
        .iter()
        .map(|record| {
            json!({
                "name": record.0,
                "ttl": 3600,
                "type": record.1,
                "comment": "Domain verification record",
                "content": record.2,
                "private_routing": true,
                "proxied": true,
                "settings": {
                  "ipv4_only": true,
                  "ipv6_only": true
                },
                "tags": [
                  "owner:dns-team"
                ],
                "id": format!("{}-{}-{}", record.0, record.1, record.2),
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
            })
        })
        .collect::<serde_json::Value>();

    return server
        .mock(
            "GET",
            format!("/zones/{zone_id}/dns_records?name={name}").as_str(),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "errors": [],
                "messages": [],
                "success": true,
                "result": results
            })
            .to_string(),
        )
        .create_async()
        .await;
}

pub async fn setup_create_dns_mock(
    server: &mut ServerGuard,
    zone_id: &str,
    record_type: &str,
    content: &str,
) -> Mock {
    return server
        .mock("POST", format!("/zones/{zone_id}/dns_records").as_str())
        .match_body(Matcher::Json(json!({
            "proxied": true,
            "name": "whoami.example.com",
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
                    "name": "whoami.example.com",
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

pub async fn setup_list_tunnels_mock(
    server: &mut ServerGuard,
    account_id: &str,
    tunnel_name: &str,
    tunnels: Vec<String>,
) -> Mock {
    let results = tunnels
        .iter()
        .map(|tunnel| {
            json!({
                "id": "f70ff985-a4ef-4643-bbbc-4a0ed4fc8415",
                "account_tag": "699d98642c564d2e855e9661899b7252",
                "config_src": "local",
                "connections": [
                  {
                    "id": "1bedc50d-42b3-473c-b108-ff3d10c0d925",
                    "client_id": "1bedc50d-42b3-473c-b108-ff3d10c0d925",
                    "client_version": "2022.7.1",
                    "colo_name": "DFW",
                    "is_pending_reconnect": false,
                    "opened_at": "2021-01-25T18:22:34.317854Z",
                    "origin_ip": "10.1.0.137",
                    "uuid": "1bedc50d-42b3-473c-b108-ff3d10c0d925"
                  }
                ],
                "conns_active_at": "2009-11-10T23:00:00Z",
                "conns_inactive_at": "2009-11-10T23:00:00Z",
                "created_at": "2021-01-25T18:22:34.317854Z",
                "deleted_at": "2009-11-10T23:00:00.000000Z",
                "metadata": {},
                "name": tunnel,
                "remote_config": false,
                "status": "healthy",
                "tun_type": "cfd_tunnel"
            })
        })
        .collect::<Vec<serde_json::Value>>();

    return server
        .mock(
            "GET",
            format!("/accounts/{account_id}/cfd_tunnel?name={tunnel_name}&is_deleted=false")
                .as_str(),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "errors": [],
                "messages": [],
                "success": true,
                "result": results,
            })
            .to_string(),
        )
        .create_async()
        .await;
}

pub fn run_contollers(ctx: Arc<Context>) -> Receiver<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let kube_cli = ctx.kube_cli.clone();
        let crd_api: Api<CustomResourceDefinition> = Api::all(kube_cli.clone());
        let ingc_api: Api<IngressClass> = Api::all(kube_cli.clone());

        if let Err(err) = crd_api
            .create(&PostParams::default(), &crate::ClusterTunnel::crd())
            .await
        {
            assert!(false, "{err:?}");
        }

        let ingress_class = IngressClass {
            metadata: ObjectMeta {
                name: Some("cloudflare-tunnels".to_string()),
                annotations: Some({
                    let mut map = BTreeMap::new();
                    map.insert(
                        "ingressclass.kubernetes.io/is-default-class".to_string(),
                        "true".to_string(),
                    );
                    map
                }),
                labels: Some({
                    let mut map = BTreeMap::new();
                    map.insert("test-resource".to_string(), "true".to_string());
                    map
                }),
                ..Default::default()
            },
            spec: Some(IngressClassSpec {
                controller: Some("cloudflare-tunnels-operator.io/ingress-controller".to_string()),
                ..Default::default()
            }),
        };

        if let Err(err) = ingc_api
            .create(&PostParams::default(), &ingress_class)
            .await
        {
            assert!(false, "{err:?}");
        }

        let ct = controller::clustertunnel::run(ctx.clone());
        let ing = controller::ingress::run(ctx.clone());

        let _ = sender.send(());
        let _ = tokio::join!(ct, ing);
    });

    return receiver;
}

pub async fn wait_for_resource<K: DeserializeOwned + Clone + Debug>(
    api: &Api<K>,
    name: &str,
) -> Option<K> {
    let mut retry = 0;
    loop {
        retry += 1;

        if let Ok(res) = api.get(name).await {
            return Some(res);
        }

        tokio::time::sleep(Duration::from_secs(5)).await;

        if retry >= 5 {
            return None;
        }
    }
}

pub async fn wait_for_resource_status<K: DeserializeOwned + Clone + Debug>(
    api: &Api<K>,
    name: &str,
) -> Option<K> {
    let mut retry = 0;
    loop {
        retry += 1;

        if let Ok(res) = api.get_status(name).await {
            return Some(res);
        }

        tokio::time::sleep(Duration::from_secs(5)).await;

        if retry >= 5 {
            return None;
        }
    }
}
