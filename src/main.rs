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
    let (config, _creds) = resolve_config(config).await?;
    let (bridge, cfg) = init_bridge(config).await?;
    info!("authenticated as {}", bridge.self_id);

    let prefix = cfg.normalized_prefix();
    let router = build_router(bridge.clone(), &prefix);

    // Start Matrix sync loop in the background (plain-text rooms + APIs).
    let sync_handle = {
        let b = bridge.clone();
        tokio::spawn(async move { b.run_sync().await })
    };

    // Start the official matrix-sdk E2EE sync when we own the login keys.
    // Password / login-token logins create a device here, so encrypted rooms
    // can be decrypted. access-token / appservice logins are skipped.
    let e2ee_handle = if matrix_satori::matrix::e2ee::e2ee_available(&cfg) {
        let c = cfg.clone();
        let b = bridge.clone();
        info!("[e2ee] enabled (password/token login owns the decryption keys)");
        Some(tokio::spawn(async move {
            if let Err(e) = matrix_satori::matrix::e2ee::start(c, b).await {
                tracing::error!("[e2ee] setup failed: {}", e);
            }
        }))
    } else {
        info!("[e2ee] disabled: reusing an existing access token cannot decrypt; use --login-method password/token");
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
