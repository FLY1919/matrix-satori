//! matrix-satori: a Satori protocol server backed by Matrix.

pub mod auth;
pub mod bridge;
pub mod config;
pub mod element;
pub mod matrix;
pub mod satori;

use auth::{AuthClient, Credentials};
use axum::routing::{get, post};
use axum::Router;
use bridge::Bridge;
use config::Config;
use matrix::MatrixClient;
use std::sync::Arc;
use tracing::info;

/// All Satori methods the client may call (from @satorijs/protocol Methods).
const METHODS: &[&str] = &[
    "channel.create",
    "channel.delete",
    "channel.get",
    "channel.list",
    "channel.mute",
    "channel.update",
    "friend.approve",
    "friend.delete",
    "friend.list",
    "guild.approve",
    "guild.get",
    "guild.list",
    "guild.member.approve",
    "guild.member.get",
    "guild.member.kick",
    "guild.member.list",
    "guild.member.mute",
    "guild.member.role.list",
    "guild.member.role.set",
    "guild.member.role.unset",
    "guild.role.create",
    "guild.role.delete",
    "guild.role.list",
    "guild.role.update",
    "login.get",
    "message.create",
    "message.delete",
    "message.get",
    "message.list",
    "message.update",
    "reaction.clear",
    "reaction.create",
    "reaction.delete",
    "reaction.list",
    "upload.create",
    "user.channel.create",
    "user.get",
];

pub fn build_router(bridge: Arc<Bridge>, prefix: &str) -> Router {
    let events = format!("{}/v1/events", prefix);
    let health = format!("{}/healthz", prefix);
    let mut router = Router::new()
        .route(&health, get(|| async { "ok" }))
        .route(&events, get(satori::gateway::ws_handler));

    // Register every method explicitly. axum's matchit wildcard routes with
    // dotted segments proved unreliable across builds; explicit paths are safe.
    for method in METHODS {
        let path = format!("{}/v1/{}", prefix, method);
        if *method == "upload.create" {
            // Upload uses multipart/form-data, not JSON.
            router = router.route(&path, post(satori::api::upload_handler));
        } else {
            router = router.route(
                &path,
                post(satori::api::api_handler).get(satori::api::api_handler),
            );
        }
    }

    router.with_state(bridge)
}

/// Resolve Matrix credentials via the configured login flow, returning the
/// effective config (with access_token/user_id filled in) and the credentials.
pub async fn resolve_config(mut config: Config) -> anyhow::Result<(Config, Credentials)> {
    // Reuse locally cached credentials whenever the user supplied no fresh
    // login material. This is especially important for SSO/password logins:
    // without a cache the user would have to re-open a browser on every start.
    // Direct access-token / appservice logins carry their own credentials and
    // are intentionally excluded (there is nothing worth persisting).
    let has_explicit = !config.access_token.is_empty()
        || !config.login_token.is_empty()
        || (!config.username.is_empty() && !config.password.is_empty())
        || !config.as_token.is_empty();
    let method_uses_cache = matches!(
        config.login_method,
        config::LoginMethod::Auto
            | config::LoginMethod::Sso
            | config::LoginMethod::Password
            | config::LoginMethod::Token
    );
    if !has_explicit && method_uses_cache {
        if let Some(cached) = auth::load_credentials(&config) {
            info!(
                "using cached credentials for {} (method={})",
                cached.user_id, cached.method
            );
            config.access_token = cached.access_token.clone();
            if config.user_id.is_empty() {
                config.user_id = cached.user_id.clone();
            }
            return Ok((config, cached));
        }
    }

    let client = AuthClient::new(&config)?;
    let creds = client.resolve().await?;
    info!("logged in as {} via {}", creds.user_id, creds.method);

    if creds.method != "access-token" {
        if let Err(e) = auth::save_credentials(&config, &creds) {
            tracing::warn!("could not cache credentials: {}", e);
        }
    }

    config.access_token = creds.access_token.clone();
    if config.user_id.is_empty() {
        config.user_id = creds.user_id.clone();
    }
    Ok((config, creds))
}

pub async fn init_bridge(config: Config) -> anyhow::Result<(Arc<Bridge>, Arc<Config>)> {
    let config = Arc::new(config);
    let matrix = MatrixClient::new(config.clone())?;

    let user_id = if config.user_id.is_empty() {
        let who = matrix.whoami().await?;
        if who.is_empty() {
            anyhow::bail!("could not determine user id; set --user-id / MATRIX_USER_ID");
        }
        who
    } else {
        config.user_id.clone()
    };

    Ok((
        Arc::new(Bridge::new(matrix, config.clone(), user_id)),
        config,
    ))
}
