use std::sync::Arc;

use actix_web::{get, middleware, App, HttpRequest, HttpResponse, HttpServer, Responder};
use clap::Parser;
use cloudflare_tunnels_operator::{controller, Context};
use log::info;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    ingress_class: Option<String>,
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
    });

    let clustertunnel = controller::clustertunnel::run(ctx.clone());
    let ingress = controller::ingress::run(ctx.clone());

    let server = HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(health)
    })
    .bind("0.0.0.0:2000")?
    .shutdown_timeout(5)
    .run();

    let _ = tokio::join!(clustertunnel, ingress, server);

    Ok(())
}
