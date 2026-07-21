mod common;
use common::*;

use std::{collections::BTreeMap, sync::Arc};

use cloudflare::framework::Environment;
use cloudflare_tunnels_operator::{
    ClusterTunnel, Context,
    cloudflare::TunnelConfig,
    controller::clustertunnel::{ClusterTunnelSpec, SecretRef},
};
use k8s_openapi::{
    api::{
        core::v1::{ConfigMap, Secret, Service, ServicePort, ServiceSpec},
        networking::v1::{
            HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
            IngressServiceBackend, IngressSpec, ServiceBackendPort,
        },
    },
    apimachinery::pkg::util::intstr::IntOrString,
};
use kube::{
    Api, ResourceExt,
    api::{DeleteParams, ListParams, ObjectMeta, PostParams},
};

#[tokio::test]
async fn test_ingress_controller() {
    env_logger::init();

    let account_id = "e2e-test-account";
    let zone_id = "e2e-test-zone";
    let tunnel_name = "e2e-tunnel";
    let hostname = "whoami.example.com";
    let cname_record = "e2e-test.cfargotunnel.com";
    let txt_record = "heritage=cloudflare-tunnels-operator,cloudflare-tunnels-operator/owner=default,cloudflare-tunnels-operator/resource=ingress/default/whoami";

    let mut server = mockito::Server::new_async().await;

    // Create a mock
    let list_tunnel_mock = setup_list_tunnels_mock(
        &mut server,
        account_id,
        tunnel_name,
        vec![tunnel_name.to_string()],
    )
    .await;

    let list_dns_empty_mock = setup_list_dns_mock(&mut server, zone_id, hostname, vec![]).await;
    let list_dns_existing_mock = setup_list_dns_mock(
        &mut server,
        zone_id,
        hostname,
        vec![
            (
                hostname.to_string(),
                "CNAME".to_string(),
                cname_record.to_string(),
            ),
            (
                hostname.to_string(),
                "TXT".to_string(),
                txt_record.to_string(),
            ),
        ],
    )
    .await;

    let create_txt_mock = setup_create_dns_mock(&mut server, zone_id, "TXT", txt_record).await;

    let create_cname_mock =
        setup_create_dns_mock(&mut server, zone_id, "CNAME", cname_record).await;

    let kube_cli = kube::Client::try_default().await.unwrap();

    let cloudflare_client = cloudflare_tunnels_operator::cloudflare::Client::new(
        account_id.to_string(),
        zone_id.to_string(),
        cloudflare_tunnels_operator::cloudflare::Credentials::UserAuthToken {
            token: "e2e-test-token".to_string(),
        },
        Environment::Custom(server.url()),
    )
    .unwrap();

    let ctx = Arc::new(Context {
        kube_cli: kube_cli.clone(),
        ingress_class: Some("cloudflare-tunnels".to_string()),
        disable_dns: None,
        owner: None,
        cloudflared_version: "latest".to_string(),
        cloudflare_client,
    });

    let sec_api: Api<Secret> = Api::namespaced(kube_cli.clone(), "default");
    let ct_api: Api<ClusterTunnel> = Api::all(kube_cli.clone());
    let svc_api: Api<Service> = Api::namespaced(kube_cli.clone(), "default");
    let ing_api: Api<Ingress> = Api::namespaced(kube_cli.clone(), "default");
    let cm_api: Api<ConfigMap> = Api::namespaced(kube_cli.clone(), "default");

    let secret = Secret {
        metadata: ObjectMeta {
            name: Some("cloudflared-secret".to_string()),
            labels: Some({
                let mut map = BTreeMap::new();
                map.insert("test-resource".to_string(), "true".to_string());
                map
            }),
            ..Default::default()
        },
        string_data: Some({
            let mut map = BTreeMap::new();
            map.insert("credentials.json".to_string(), r#"{"AccountTag":"e2e-account-tag","TunnelSecret":"e2e-tunnel-secret","TunnelID":"e2e-test"}"#.to_string());
            map.insert("cert.pem".to_string(), "cert pem".to_string());
            map
        }),
        ..Default::default()
    };

    if let Err(err) = sec_api.create(&PostParams::default(), &secret).await {
        assert!(false, "{err:?}");
    }

    let cluster_tunnel = ClusterTunnel {
        metadata: ObjectMeta {
            name: Some(tunnel_name.to_string()),
            labels: Some({
                let mut map = BTreeMap::new();
                map.insert("test-resource".to_string(), "true".to_string());
                map
            }),
            ..Default::default()
        },
        spec: ClusterTunnelSpec {
            name: Some(tunnel_name.to_string()),
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

    let receiver = run_contollers(ctx);

    let _ = receiver.await;

    if let Err(err) = ct_api.create(&PostParams::default(), &cluster_tunnel).await {
        assert!(false, "{err:?}");
    }

    if wait_for_resource(
        &cm_api,
        format!("metadata.name=cloudflared-{tunnel_name}-config").as_str(),
    )
    .await
    .is_none()
    {
        assert!(false, "config not created");
    }

    let service = Service {
        metadata: ObjectMeta {
            name: Some("whoami".to_string()),
            labels: Some({
                let mut map = BTreeMap::new();
                map.insert("test-resource".to_string(), "true".to_string());
                map
            }),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            ports: Some(vec![ServicePort {
                port: 80,
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
            labels: Some({
                let mut map = BTreeMap::new();
                map.insert("test-resource".to_string(), "true".to_string());
                map
            }),
            ..Default::default()
        },
        spec: Some(IngressSpec {
            ingress_class_name: Some("cloudflare-tunnels".to_string()),
            rules: Some(vec![IngressRule {
                host: Some(hostname.to_string()),
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
    match wait_for_resource_status(&ing_api, &format!("metadata.name={}", ingress.name_any())).await
    {
        Some(ing) => {
            if let Some(hostname) = ing
                .status
                .and_then(|status| status.load_balancer)
                .and_then(|lb| lb.ingress)
                .and_then(|ing| ing.first().cloned())
                .and_then(|lb| lb.hostname)
            {
                if hostname != cname_record {
                    assert!(false, "expected {cname_record} got {hostname}");
                }
            }
        }
        None => {
            assert!(false, "ingress status not updated");
        }
    }

    if let Some(config) = cm_api
        .get(format!("loudflared-{tunnel_name}-config").as_str())
        .await
        .ok()
        .and_then(|cfg| cfg.data)
        .and_then(|cfg| cfg.get("config.yaml").cloned())
        .and_then(|cfg| serde_yaml::from_str::<TunnelConfig>(&cfg).ok())
    {
        if config
            .ingress
            .iter()
            .find(|ing| {
                ing.hostname == Some(hostname.to_string())
                    && ing.service == "http://whoami.default.svc:80"
            })
            .is_none()
        {
            assert!(false, "ingress not updated in cloudflared config.yaml")
        }
    } else {
        assert!(false, "no config found");
    }

    list_tunnel_mock.expect_at_least(1).assert_async().await;
    list_dns_empty_mock.assert_async().await;
    list_dns_existing_mock.assert_async().await;
    create_cname_mock.assert_async().await;
    create_txt_mock.assert_async().await;

    let _ = ing_api
        .delete_collection(
            &DeleteParams::default(),
            &ListParams::default().labels("test-resource=true"),
        )
        .await;

    let _ = svc_api
        .delete_collection(
            &DeleteParams::default(),
            &ListParams::default().labels("test-resource=true"),
        )
        .await;

    let _ = ct_api.delete(tunnel_name, &DeleteParams::default()).await;

    let _ = sec_api
        .delete("cloudflared-secret", &DeleteParams::default())
        .await;
}
