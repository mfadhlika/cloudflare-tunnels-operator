use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, get, middleware};
use clap::Parser;
use cloudflare_tunnels_operator::{Context, cloudflare, controller};
use log::info;
use std::sync::Arc;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    ingress_class: Option<String>,
    #[arg(long)]
    disable_dns: Option<bool>,
    #[arg(long)]
    owner: Option<String>,
    #[arg(long)]
    cloudflare_account_id: String,
    #[arg(long)]
    cloudflare_zone_id: String,
    #[arg(long)]
    cloudflare_api_token: Option<String>,
    #[arg(long)]
    cloudflare_email: Option<String>,
    #[arg(long)]
    cloudflare_api_key: Option<String>,
    #[arg(long, default_value = "2026.2.0")]
    cloudflared_version: String,
}

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok()
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    env_logger::init();

    info!("starting cloudflare tunnels operator");

    let args: Args = Args::parse();

    let kube_cli = kube::Client::try_default().await?;

    let cloudflare_credentials = if let Some(token) = args.cloudflare_api_token {
        cloudflare::Credentials::UserAuthToken { token }
    } else if let Some(key) = args.cloudflare_api_key {
        let Some(email) = args.cloudflare_email else {
            return Err(anyhow::anyhow!("api key requires email").into());
        };

        cloudflare::Credentials::UserAuthKey { email, key }
    } else {
        return Err(anyhow::anyhow!("api key requires email").into());
    };

    let cloudflare_client = cloudflare::Client::new(
        args.cloudflare_account_id.clone(),
        args.cloudflare_zone_id.clone(),
        cloudflare_credentials,
        cloudflare::Environment::Production,
    )?;

    let ctx = Arc::new(Context {
        kube_cli,
        ingress_class: args.ingress_class.clone(),
        disable_dns: args.disable_dns,
        owner: args.owner,
        cloudflared_version: args.cloudflared_version,
        cloudflare_client,
    });

    let clustertunnel = controller::clustertunnel::run(ctx.clone());
    let ingress = controller::ingress::run(ctx.clone());

    let server = HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(health)
    })
    .bind("[::]:2000")?
    .shutdown_timeout(5)
    .run();

    let _ = tokio::join!(clustertunnel, ingress, server);

    Ok(())
}
