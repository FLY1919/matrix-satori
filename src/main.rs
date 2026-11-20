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

    // Start Matrix sync loop in the background (plain-text rooms + APIs).
    let sync_handle = {
        let b = bridge.clone();
        tokio::spawn(async move { b.run_sync().await })
    };

    // Start the official matrix-sdk E2EE sync by restoring the exact device
    // the main client logged in as (password / token / SSO / auto all carry a
    // device id). Keys persist in <state-dir>/crypto-store across restarts.
    // A bare access token with no device id has no keys and is skipped.
    let e2ee_handle = if matrix_satori::matrix::e2ee::e2ee_available(&creds) {
        let c = cfg.clone();
        let b = bridge.clone();
        info!(
            "[e2ee] enabled (restoring device {} via {} login)",
            creds.device_id.as_deref().unwrap_or("?"),
            creds.method
        );
        Some(tokio::spawn(async move {
            if let Err(e) = matrix_satori::matrix::e2ee::start(c, creds, b).await {
                tracing::error!("[e2ee] setup failed: {}", e);
            }
        }))
    } else {
        info!("[e2ee] disabled: no device id in credentials (bare access-token / appservice cannot decrypt)");
        None
    };

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    info!(
        "Satori endpoints ready at http://{}{}/v1/events",
        cfg.bind, prefix
    );
    info!("HTTP API at http://{}{}/v1/<method>", cfg.bind, prefix);

    axum::serve(listener, router).await?;
    sync_handle.abort();
    if let Some(h) = e2ee_handle {
        h.abort();
    }
    Ok(())
}
