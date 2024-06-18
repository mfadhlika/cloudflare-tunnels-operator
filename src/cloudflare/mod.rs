use std::time::Duration;

pub use client::*;
mod client;

pub use cloudflare::endpoints::*;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct TunnelCredentials {
    pub account_tag: String,
    pub tunnel_secret: String,
    pub tunnel_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_pool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_tls_verify: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_timeout: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_2_origin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_host_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_chunjed_encoding: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_timeout: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_happy_eyeball: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_port: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive_timeout: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive_connection: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_keep_alive: Option<Duration>,
}

#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelIngress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_request: Option<OriginRequest>,
}

#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    pub tunnel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_request: Option<OriginRequest>,
    #[serde(rename = "credentials-file")]
    pub credentials_file: String,
    pub ingress: Vec<TunnelIngress>,
}
