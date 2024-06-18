pub struct Context {
    pub kube_cli: kube::Client,
    pub ingress_class: Option<String>,
}
