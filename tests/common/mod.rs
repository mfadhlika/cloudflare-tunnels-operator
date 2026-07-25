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
use serde::de::DeserializeOwned;
use tokio::sync::oneshot::Receiver;

pub mod cloudflare_mock;

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
