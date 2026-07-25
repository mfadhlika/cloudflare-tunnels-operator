use std::pin::Pin;

use mockito::{Matcher, Mock, ServerGuard};
use serde_json::json;

pub struct CloudflareMock {
    server: ServerGuard,
    asserts: Vec<Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()>>>>>,
}

impl CloudflareMock {
    pub async fn new() -> Self {
        Self {
            server: mockito::Server::new_async().await,
            asserts: vec![],
        }
    }

    pub fn url(&self) -> String {
        return self.server.url();
    }

    pub async fn setup_list_dns_mock(
        &mut self,
        zone_id: &str,
        name: &str,
        records: Vec<(String, String, String, String)>,
        assert: impl AsyncFnOnce(Mock) + 'static,
    ) {
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
                    "id": record.3,
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

        let mock = self
            .server
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

        self.asserts.push(Box::new(move || Box::pin(assert(mock))));
    }

    pub async fn setup_create_dns_mock(
        &mut self,
        zone_id: &str,
        record_type: &str,
        content: &str,
        assert: impl AsyncFnOnce(Mock) + 'static,
    ) {
        let mock = self
            .server
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

        self.asserts.push(Box::new(move || Box::pin(assert(mock))));
    }

    pub async fn setup_delete_dns_mock(
        &mut self,
        zone_id: &str,
        record_id: &str,
        assert: impl AsyncFnOnce(Mock) + 'static,
    ) {
        let mock = self
            .server
            .mock(
                "DELETE",
                format!("/zones/{zone_id}/dns_records/{record_id}").as_str(),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "result": {
                        "id": record_id
                    }
                })
                .to_string(),
            )
            .create_async()
            .await;

        self.asserts.push(Box::new(move || Box::pin(assert(mock))));
    }

    pub async fn setup_list_tunnels_mock(
        &mut self,
        account_id: &str,
        tunnel_name: &str,
        tunnels: Vec<String>,

        assert: impl AsyncFnOnce(Mock) + 'static,
    ) {
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

        let mock = self
            .server
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

        self.asserts.push(Box::new(move || Box::pin(assert(mock))));
    }

    pub async fn assert_async(self) {
        for assert in self.asserts.into_iter() {
            assert().await;
        }
    }
}
