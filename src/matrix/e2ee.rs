//! End-to-end encrypted room support via the official `matrix-sdk`.
//!
//! This is a faithful, minimal adaptation of matrix-rust-sdk's
//! `examples/getting_started`: build a `Client`, log in so that *this process*
//! owns the olm keys, register an event handler, and `sync()`. The SDK
//! transparently decrypts `m.room.encrypted` events, so the handler receives
//! a plain `SyncRoomMessageEvent` even for encrypted rooms.
//!
//! Important: E2EE decryption requires the private keys to live in this
//! process. That means we can only enable it when we perform the login here
//! (password or login-token). A pre-obtained access token or appservice token
//! cannot decrypt, because the corresponding device keys live elsewhere.
//!
//! The plain-text sync loop in `Bridge::run_sync` keeps handling unencrypted
//! rooms; this client only forwards messages that arrived decrypted, so the
//! two paths never duplicate events.

use crate::bridge::Bridge;
use crate::config::{Config, LoginMethod};
use crate::matrix::EventInfo;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::event_handler::Ctx;
use matrix_sdk::ruma::events::room::message::SyncRoomMessageEvent;
use matrix_sdk::{Client, Room};
use std::sync::Arc;
use tracing::{debug, error, info};

/// Decide whether E2EE can be enabled for the given configuration.
pub fn e2ee_available(config: &Config) -> bool {
    match config.login_method {
        // Password and login-token logins create a fresh device with keys here.
        LoginMethod::Password => !config.username.is_empty() && !config.password.is_empty(),
        LoginMethod::Token => !config.login_token.is_empty(),
        // Auto: usable when we have password or login-token credentials.
        LoginMethod::Auto => {
            (!config.username.is_empty() && !config.password.is_empty())
                || !config.login_token.is_empty()
        }
        // These reuse a token whose keys we do not own -> cannot decrypt.
        LoginMethod::AccessToken | LoginMethod::Sso | LoginMethod::ApplicationService => false,
    }
}

/// Start the E2EE sync loop in the background. Returns an error only for setup
/// failures (client build / login); the sync loop itself retries internally.
pub async fn start(config: Arc<Config>, bridge: Arc<Bridge>) -> anyhow::Result<()> {
    let client = Client::builder()
        .homeserver_url(config.homeserver.clone())
        .build()
        .await?;

    // Log in so this device owns olm keys. Mirror the official getting_started.
    let auth = client.matrix_auth();
    match config.login_method {
        LoginMethod::Token | LoginMethod::Auto if !config.login_token.is_empty() => {
            auth.login_token(&config.login_token)
                .initial_device_display_name("matrix-satori e2ee")
                .await?;
        }
        _ => {
            // username may be a full MXID or a localpart; SDK accepts both.
            auth.login_username(&config.username, &config.password)
                .initial_device_display_name("matrix-satori e2ee")
                .await?;
        }
    }
    info!(
        "[e2ee] logged in as {}",
        client.user_id().map(|u| u.as_str()).unwrap_or("?")
    );

    // Register the bridge as handler context (official `add_event_handler_context`).
    // `Room` is injected by the SDK so we know which room the event came from.
    client.add_event_handler_context(bridge.clone());
    client.add_event_handler(
        |event: SyncRoomMessageEvent, room: Room, bridge: Ctx<Arc<Bridge>>| async move {
            on_room_message(event, room, bridge).await;
        },
    );

    // Initial sync to establish room state / upload keys, then stream forever.
    let token = client.sync_once(SyncSettings::default()).await?.next_batch;
    info!("[e2ee] sync loop started (encrypted rooms will be decrypted)");

    let settings = SyncSettings::default().token(token);
    if let Err(e) = client.sync(settings).await {
        error!("[e2ee] sync ended: {}", e);
    }
    Ok(())
}

/// Official-style event handler. Receives decrypted message events.
async fn on_room_message(event: SyncRoomMessageEvent, room: Room, bridge: Ctx<Arc<Bridge>>) {
    // ruma's SyncMessageLikeEvent enum does not implement Serialize, but its
    // content type does. Rebuild the wire shape we feed to EventInfo by hand.
    let (event_id, sender, ts, content) = match &event {
        SyncRoomMessageEvent::Original(e) => {
            let Ok(content) = serde_json::to_value(&e.content) else {
                debug!("[e2ee] could not serialize content");
                return;
            };
            (
                e.event_id.as_str().to_string(),
                e.sender.as_str().to_string(),
                u64::from(e.origin_server_ts.0) as i64,
                content,
            )
        }
        SyncRoomMessageEvent::Redacted(_) => return,
    };
    if content.get("body").is_none() {
        return;
    }
    let value = serde_json::json!({
        "type": "m.room.message",
        "event_id": event_id,
        "sender": sender,
        "origin_server_ts": ts,
        "room_id": room.room_id().as_str(),
        "content": content,
    });
    let info = EventInfo::from_value(&value);
    let room_id = room.room_id().to_string();
    debug!("[e2ee] decrypted message {} in {}", info.event_id, room_id);
    bridge.emit_message(&room_id, &info).await;
}
