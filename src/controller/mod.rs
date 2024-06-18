use std::{sync::Arc, time::Duration};

use kube::runtime::controller::Action;
use log::error;

use crate::{context::Context, error::Error};

pub mod clustertunnel;
pub use clustertunnel::ClusterTunnel;

pub mod ingress;

mod utils;

pub(super) const OPERATOR_MANAGER: &'static str = "cloudflare-tunnels-operator";

pub(super) fn error_policy<K>(_obj: Arc<K>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!("reason: {}", err);
    Action::requeue(Duration::from_secs(15))
}
