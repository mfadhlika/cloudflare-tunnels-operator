use crate::Error;
use base64::{prelude::BASE64_STANDARD, Engine};
use cloudflare::endpoints::dns::DnsRecord;
use rand::RngCore;

use super::TunnelCredentials;
pub use cloudflare::framework::auth::Credentials;

pub struct Client {
    account_id: String,
    client: cloudflare::framework::async_api::Client,
}

impl Client {
    pub fn new(account_id: String, credentials: Credentials) -> Result<Self, Error> {
        let client = cloudflare::framework::async_api::Client::new(
            credentials,
            cloudflare::framework::HttpApiClientConfig::default(),
            cloudflare::framework::Environment::Production,
        )?;

        Ok(Self { account_id, client })
    }

    pub async fn create_tunnel(&self, tunnel_name: &str) -> Result<TunnelCredentials, Error> {
        let mut tunnel_secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut tunnel_secret);

        let tunnel_secret = tunnel_secret.to_vec();

        let endpoint = cloudflare::endpoints::cfd_tunnel::create_tunnel::CreateTunnel {
            account_identifier: &self.account_id,
            params: cloudflare::endpoints::cfd_tunnel::create_tunnel::Params {
                name: &tunnel_name,
                tunnel_secret: &tunnel_secret,
                config_src: &cloudflare::endpoints::cfd_tunnel::ConfigurationSrc::Local,
                metadata: None,
            },
        };

        let response = self.client.request(&endpoint).await?;

        let tunnel_credentials = TunnelCredentials {
            account_tag: self.account_id.to_owned(),
            tunnel_secret: BASE64_STANDARD.encode(&tunnel_secret),
            tunnel_id: response.result.id.to_string(),
        };

        Ok(tunnel_credentials)
    }

    pub async fn find_tunnel(&self, tunnel_name: &str) -> Result<Option<String>, Error> {
        let endpoint = cloudflare::endpoints::cfd_tunnel::list_tunnels::ListTunnels {
            account_identifier: &self.account_id,
            params: cloudflare::endpoints::cfd_tunnel::list_tunnels::Params {
                name: Some(tunnel_name.to_owned()),
                is_deleted: Some(false),
                ..cloudflare::endpoints::cfd_tunnel::list_tunnels::Params::default()
            },
        };

        let response = self.client.request(&endpoint).await?;

        Ok(response.result.first().map(|tunnel| tunnel.id.to_string()))
    }

    pub async fn delete_tunnel(&self, tunnel_id: &str) -> Result<(), Error> {
        let endpoint = cloudflare::endpoints::cfd_tunnel::delete_tunnel::DeleteTunnel {
            account_identifier: &self.account_id,
            tunnel_id,
            params: cloudflare::endpoints::cfd_tunnel::delete_tunnel::Params { cascade: true },
        };

        self.client.request(&endpoint).await?;

        Ok(())
    }

    pub async fn create_dns_record(
        &self,
        zone_id: &str,
        hostname: &str,
        content: &str,
    ) -> Result<(), Error> {
        let endpoint = cloudflare::endpoints::dns::CreateDnsRecord {
            zone_identifier: zone_id,
            params: cloudflare::endpoints::dns::CreateDnsRecordParams {
                proxied: Some(true),
                name: hostname,
                content: cloudflare::endpoints::dns::DnsContent::CNAME {
                    content: content.to_string(),
                },
                ttl: None,
                priority: None,
            },
        };

        self.client.request(&endpoint).await?;

        Ok(())
    }

    pub async fn update_dns_record(
        &self,
        zone_id: &str,
        domain_id: &str,
        hostname: &str,
        tunnel_id: &str,
    ) -> Result<(), Error> {
        let endpoint = cloudflare::endpoints::dns::UpdateDnsRecord {
            zone_identifier: zone_id,
            identifier: domain_id,
            params: cloudflare::endpoints::dns::UpdateDnsRecordParams {
                proxied: Some(true),
                name: hostname,
                content: cloudflare::endpoints::dns::DnsContent::CNAME {
                    content: format!("{tunnel_id}.cfargotunnel.com"),
                },
                ttl: None,
            },
        };

        self.client.request(&endpoint).await?;

        Ok(())
    }

    pub async fn find_dns_record(
        &self,
        zone_id: &str,
        hostname: &str,
    ) -> Result<Option<DnsRecord>, Error> {
        let endpoint = cloudflare::endpoints::dns::ListDnsRecords {
            zone_identifier: zone_id,
            params: cloudflare::endpoints::dns::ListDnsRecordsParams {
                name: Some(hostname.to_string()),
                ..cloudflare::endpoints::dns::ListDnsRecordsParams::default()
            },
        };

        let response = self.client.request(&endpoint).await?;

        Ok(response.result.into_iter().find(|rec| rec.name == hostname))
    }

    pub async fn delete_dns_record(&self, zone_id: &str, domain_id: &str) -> Result<(), Error> {
        let endpoint = cloudflare::endpoints::dns::DeleteDnsRecord {
            zone_identifier: zone_id,
            identifier: domain_id,
        };

        self.client.request(&endpoint).await?;

        Ok(())
    }
}
