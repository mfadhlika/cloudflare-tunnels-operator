pub struct Context {
    pub kube_cli: kube::Client,
    pub ingress_class: Option<String>,
    pub disable_dns: Option<bool>,
    pub owner: Option<String>,
    pub cloudflared_version: String,
    pub cloudflare_client: crate::cloudflare::Client,
}
