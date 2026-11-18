mod cert;
mod clipboard;
mod credentials;
mod drives;
mod files;
mod plugin;
mod protocol;
mod session;

use dbx_plugin_sdk::{PluginMetadata, PluginServer, PluginTransport};
use tracing_subscriber::EnvFilter;

use crate::plugin::RdpPlugin;

fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,ironrdp=warn,sspi=warn")),
        )
        .with_target(false)
        .init();

    let metadata = PluginMetadata::new("io.github.lwclick.rdp", env!("CARGO_PKG_VERSION"))
        .with_capability("connections")
        .with_capability("events");
    PluginServer::new(metadata, RdpPlugin::new()?)
        .transport(PluginTransport::Framed)
        .worker_threads(8)
        .work_queue_capacity(512)
        .serve()
}
