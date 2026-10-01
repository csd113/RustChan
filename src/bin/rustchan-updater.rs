#![expect(
    unused_crate_dependencies,
    reason = "the updater links the shared library; Cargo also exposes dependencies used only by the web server"
)]
//! Restricted native Linux updater daemon. Configuration is operator-owned.

/// Start the daemon with its fixed root-owned configuration.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_provider| anyhow::anyhow!("TLS provider already configured"))?;
    tracing_subscriber::fmt().with_env_filter("info").init();
    chan::updates::run(std::path::Path::new("/etc/rustchan/updater.toml")).await
}
