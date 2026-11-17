use clap::Parser;
use matrix_satori::{build_router, config::Config, init_bridge, resolve_config};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
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

    // Start Matrix sync loop in the background.
    let sync_handle = {
        let b = bridge.clone();
        tokio::spawn(async move { b.run_sync().await })
    };

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    info!(
        "Satori endpoints ready at http://{}{}/v1/events",
        cfg.bind, prefix
    );
    info!("HTTP API at http://{}{}/v1/<method>", cfg.bind, prefix);

    axum::serve(listener, router).await?;
    sync_handle.abort();
    Ok(())
}
