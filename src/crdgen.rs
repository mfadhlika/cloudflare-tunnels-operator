use kube::CustomResourceExt;
fn main() {
    print!(
        "{}",
        serde_yaml::to_string(&cloudflare_tunnels_operator::ClusterTunnel::crd()).unwrap()
    )
}
