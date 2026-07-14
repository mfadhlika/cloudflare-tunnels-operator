use std::sync::Arc;

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, get, middleware};
use clap::Parser;
use cloudflare_tunnels_operator::{Context, controller};
use log::info;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    ingress_class: Option<String>,
    #[arg(long)]
    disable_dns: Option<bool>,
    #[arg(long)]
    owner: Option<String>,
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

    let ctx = Arc::new(Context {
        kube_cli,
        ingress_class: args.ingress_class.clone(),
        disable_dns: args.disable_dns,
        owner: args.owner,
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
