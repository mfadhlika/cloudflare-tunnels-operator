use std::{collections::BTreeMap, sync::Arc, time::Duration};

use cloudflare::framework::Environment;
use cloudflare_tunnels_operator::{
    ClusterTunnel, Context,
    controller::{
        self,
        clustertunnel::{ClusterTunnelSpec, SecretRef},
    },
};
use k8s_openapi::{
    api::{
        core::v1::{Secret, Service, ServicePort, ServiceSpec},
        networking::v1::{
            HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressClass,
            IngressClassSpec, IngressRule, IngressServiceBackend, IngressSpec, ServiceBackendPort,
        },
    },
    apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    apimachinery::pkg::util::intstr::IntOrString,
};
use kube::{
    Api, CustomResourceExt,
    api::{ObjectMeta, PostParams},
};
use mockito::{Matcher, Mock, ServerGuard};
use serde_json::json;

async fn setup_create_dns_mock(server: &mut ServerGuard, record_type: &str, content: &str) -> Mock {
    return server
        .mock("POST", "/zones/e2e-test-zone/dns_records")
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

#[tokio::test]
async fn test_ingress_controller() {
    let mut server = mockito::Server::new_async().await;

    // Create a mock
    let list_tunnel_mock = server
        .mock(
            "GET",
            "/accounts/e2e-test-account/cfd_tunnel?name=e2e-test&is_deleted=false",
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "errors": [],
                "messages": [],
                "success": true,
                "result": [{
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
                    "name": "e2e-test",
                    "remote_config": false,
                    "status": "healthy",
                    "tun_type": "cfd_tunnel"
                }],
            })
            .to_string(),
        )
        .create_async()
        .await;

    let list_dns_mock = server
        .mock(
            "GET",
            "/zones/e2e-test-zone/dns_records?name=whoami.example.com",
        )
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

    let create_txt_mock = setup_create_dns_mock(
        &mut server,
        "TXT",
        "heritage=cloudflare-tunnels-operator,cloudflare-tunnels-operator/owner=default,cloudflare-tunnels-operator/resource=ingress/default/whomai",
    )
    .await;

    let create_cname_mock =
        setup_create_dns_mock(&mut server, "CNAME", "1234.cfargotunnel.com").await;

    let kube_cli = kube::Client::try_default().await.unwrap();

    let cloudflare_client = cloudflare_tunnels_operator::cloudflare::Client::new(
        "e2e-test-account".to_string(),
        "e2e-test-zone".to_string(),
        cloudflare_tunnels_operator::cloudflare::Credentials::UserAuthToken {
            token: "e2e-test-token".to_string(),
        },
        Environment::Custom(server.url()),
    )
    .unwrap();

    let ctx = Arc::new(Context {
        kube_cli: kube_cli.clone(),
        ingress_class: None,
        disable_dns: None,
        owner: None,
        cloudflared_version: "latest".to_string(),
        cloudflare_client,
    });

    tokio::spawn(async move {
        let ct = controller::clustertunnel::run(ctx.clone());
        let ing = controller::ingress::run(ctx.clone());

        let _ = tokio::join!(ct, ing);
    });

    let crd_api: Api<CustomResourceDefinition> = Api::all(kube_cli.clone());
    let sec_api: Api<Secret> = Api::namespaced(kube_cli.clone(), "default");
    let ct_api: Api<ClusterTunnel> = Api::all(kube_cli.clone());
    let svc_api: Api<Service> = Api::namespaced(kube_cli.clone(), "default");
    let ingc_api: Api<IngressClass> = Api::all(kube_cli.clone());
    let ing_api: Api<Ingress> = Api::namespaced(kube_cli.clone(), "default");

    if let Err(err) = crd_api
        .create(
            &PostParams::default(),
            &cloudflare_tunnels_operator::ClusterTunnel::crd(),
        )
        .await
    {
        assert!(false, "{err:?}");
    }

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some("cloudflared-secret".to_string()),
            ..Default::default()
        },
        string_data: Some({
            let mut map = BTreeMap::new();
            map.insert("credentials.json".to_string(), "".to_string());
            map.insert("cert.pem".to_string(), "".to_string());
            map
        }),
        ..Default::default()
    };

    if let Err(err) = sec_api.create(&PostParams::default(), &secret).await {
        assert!(false, "{err:?}");
    }

    let cluster_tunnel = ClusterTunnel {
        metadata: ObjectMeta {
            name: Some("e2e-test".to_string()),
            ..Default::default()
        },
        spec: ClusterTunnelSpec {
            name: Some("e2e-test".to_string()),
            tunnel_secret_ref: Some(SecretRef {
                name: "cloudflared-secret".to_string(),
                key: "credentials.json".to_string(),
            }),
            origin_cert_secret_ref: Some(SecretRef {
                name: "cloudflared-secret".to_string(),
                key: "cert.pem".to_string(),
            }),
            cloudflared: None,
        },
    };

    if let Err(err) = ct_api.create(&PostParams::default(), &cluster_tunnel).await {
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

    let service = Service {
        metadata: ObjectMeta {
            name: Some("whoami".to_string()),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            ports: Some(vec![ServicePort {
                port: 8080,
                target_port: Some(IntOrString::Int(80)),
                protocol: Some("TCP".to_string()),
                name: Some("http".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };

    let ingress = Ingress {
        metadata: ObjectMeta {
            name: Some("whoami".to_string()),
            ..Default::default()
        },
        spec: Some(IngressSpec {
            ingress_class_name: Some("cloudflare-tunnels".to_string()),
            rules: Some(vec![IngressRule {
                host: Some("whoami.example.com".to_string()),
                http: Some(HTTPIngressRuleValue {
                    paths: vec![HTTPIngressPath {
                        path: Some("/".to_string()),
                        path_type: "Prefix".to_string(),
                        backend: IngressBackend {
                            service: Some(IngressServiceBackend {
                                name: "whoami".to_string(),
                                port: Some(ServiceBackendPort {
                                    name: Some("http".to_string()),
                                    ..Default::default()
                                }),
                            }),
                            ..Default::default()
                        },
                    }],
                }),
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };

    if let Err(err) = svc_api.create(&PostParams::default(), &service).await {
        assert!(false, "{err:?}");
    }

    if let Err(err) = ing_api.create(&PostParams::default(), &ingress).await {
        assert!(false, "{err:?}");
    }

    tokio::time::sleep(Duration::from_secs(30)).await;

    list_tunnel_mock.expect_at_least(1).assert_async().await;
    list_dns_mock.assert_async().await;
    create_cname_mock.assert_async().await;
    create_txt_mock.assert_async().await;
}
