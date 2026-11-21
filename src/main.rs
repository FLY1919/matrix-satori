#![recursion_limit = "256"]

use matrix_satori::{build_router, config::Config, init_bridge, resolve_config};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log)
        .with_target(true)
        .init();

    info!("starting matrix-satori (homeserver={})", config.homeserver);
    let (config, creds) = resolve_config(config).await?;
    let (bridge, cfg) = init_bridge(config).await?;
    info!("authenticated as {}", bridge.self_id);

    let prefix = cfg.normalized_prefix();
    let router = build_router(bridge.clone(), &prefix);

    // A single official matrix-sdk engine handles ALL receiving (plain +
    // encrypted rooms, decrypted transparently) and ALL sending (via
    // Room::send_raw). No second lightweight sync — one connection, no
    // duplicate events. Credentials from our own login are restored into it.
    let has_device = matrix_satori::matrix::e2ee::e2ee_available(&creds);
    info!(
        "[sdk] starting single sync engine ({}, device={})",
        creds.method,
        creds.device_id.as_deref().unwrap_or("generated")
    );
    if !has_device {
        info!("[sdk] no device id in credentials: plain rooms work, encrypted rooms need a device-bound login (password/token/sso)");
    }
    let c = cfg.clone();
    let b = bridge.clone();
    let sdk_handle = tokio::spawn(async move {
        if let Err(e) = matrix_satori::matrix::e2ee::start(c, creds, b).await {
            tracing::error!("[sdk] setup failed: {}", e);
        }
    });

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    info!(
        "Satori endpoints ready at http://{}{}/v1/events",
        cfg.bind, prefix
    );
    info!("HTTP API at http://{}{}/v1/<method>", cfg.bind, prefix);

    axum::serve(listener, router).await?;
    sdk_handle.abort();
    Ok(())
}
