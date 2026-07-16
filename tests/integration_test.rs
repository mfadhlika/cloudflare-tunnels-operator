use std::sync::Arc;

use cloudflare::framework::Environment;
use cloudflare_tunnels_operator::{Context, controller};
use k8s_openapi::{
    api::{
        core::v1::{Service, ServicePort, ServiceSpec},
        networking::v1::{
            HTTPIngressPath, HTTPIngressRuleValue, Ingress, IngressBackend, IngressRule,
            IngressServiceBackend, IngressSpec, ServiceBackendPort,
        },
    },
    apimachinery::pkg::util::intstr::IntOrString,
};
use kube::{
    Api,
    api::{ObjectMeta, PostParams},
};

#[tokio::test]
async fn test() -> Result<(), anyhow::Error> {
    let mut server = mockito::Server::new_async().await;

    // Use one of these addresses to configure your client
    let url = server.url();

    // Create a mock
    let _ = server
        .mock("GET", "/zones/{zone_id}/dns_records")
        .with_status(201)
        .with_header("content-type", "text/plain")
        .with_header("x-api-key", "1234")
        .with_body("world")
        .create();

    let kube_cli = kube::Client::try_default().await?;

    let cloudflare_client = cloudflare_tunnels_operator::cloudflare::Client::new(
        "test".to_string(),
        "test".to_string(),
        cloudflare_tunnels_operator::cloudflare::Credentials::UserAuthToken {
            token: "token".to_string(),
        },
        Environment::Custom(url),
    )?;

    let ctx = Arc::new(Context {
        kube_cli: kube_cli.clone(),
        ingress_class: None,
        disable_dns: None,
        owner: None,
        cloudflared_version: "latest".to_string(),
        cloudflare_client,
    });

    let _ = controller::clustertunnel::run(ctx.clone());
    let _ = controller::ingress::run(ctx.clone());

    let svc_api: Api<Service> = Api::all(kube_cli.clone());
    let ing_api: Api<Ingress> = Api::all(kube_cli.clone());

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
            rules: Some(vec![IngressRule {
                host: Some("/".to_string()),
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

    svc_api.create(&PostParams::default(), &service).await?;
    ing_api.create(&PostParams::default(), &ingress).await?;

    Ok(())
}
