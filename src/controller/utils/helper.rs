use anyhow::anyhow;
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use std::sync::Arc;

use crate::{
    cloudflare::Credentials,
    context::Context,
    controller::clustertunnel::{CloudflareCredentials, CloudflareSecretRef},
    Error,
};

pub async fn get_credentials(
    ctx: Arc<Context>,
    ns: &str,
    creds: &CloudflareCredentials,
) -> Result<Credentials, Error> {
    let value = {
        let kube_cli = ctx.kube_cli.clone();

        let secret_api: Api<Secret> = Api::namespaced(kube_cli.clone(), ns);

        let secret_ref = creds.secret_ref.secret_ref();

        let secret = secret_api.get(&secret_ref.name).await?;
        let data = secret.data.ok_or_else(|| anyhow!("no data"))?;

        let value = data.get(&secret_ref.key).ok_or_else(|| {
            anyhow!(
                "key {} not found or invalid in {}",
                secret_ref.key,
                secret_ref.name
            )
        })?;

        String::from_utf8(value.clone().0).map_err(|err| anyhow!("value not a string: {err:?}"))?
    };

    let creds = match &creds.secret_ref {
        &CloudflareSecretRef::ApiKey(_) => {
            let Some(email) = &creds.email else {
                return Err(anyhow!("api key requires email").into());
            };

            Credentials::UserAuthKey {
                email: email.to_owned(),
                key: value,
            }
        }
        &CloudflareSecretRef::ApiToken(_) => Credentials::UserAuthToken { token: value },
    };

    Ok(creds)
}
