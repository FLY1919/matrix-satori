//! End-to-end encrypted room support via the official `matrix-sdk`.
//!
//! Faithful adaptation of matrix-rust-sdk's `examples/getting_started`: build a
//! `Client` with a persistent SQLite store, restore the *existing* login
//! session (so we reuse the same device and its olm keys), register an event
//! handler, and `sync()`. The SDK transparently decrypts `m.room.encrypted`
//! events, so the handler receives a plain `SyncRoomMessageEvent`.
//!
//! Because we `restore_session` with the credentials already obtained by the
//! main (plain-text) client, **every** login method that has a device id can
//! decrypt: password, login-token, interactive SSO, and auto. (A bare access
//! token with no device id has no keys to restore and is skipped.)
//!
//! The SQLite store persists olm/megolm keys under `<state-dir>/crypto-store`,
//! so decryption survives restarts. Messages sent *before* this device existed
//! still cannot be decrypted without cross-signing / key backup, which is a
//! Matrix protocol limitation, not a bridge limitation.
//!
//! The plain-text sync loop in `Bridge::run_sync` keeps handling unencrypted
//! rooms; this client only forwards messages that arrived decrypted, so the
//! two paths never duplicate events.

use crate::auth::Credentials;
use crate::bridge::Bridge;
use crate::config::Config;
use crate::matrix::EventInfo;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::event_handler::Ctx;
use matrix_sdk::ruma::events::room::message::SyncRoomMessageEvent;
use matrix_sdk::ruma::{OwnedDeviceId, OwnedUserId};
use matrix_sdk::{Client, Room, SessionMeta, SessionTokens};
use std::sync::Arc;
use tracing::{debug, error, info};

/// Whether E2EE can be enabled for the resolved credentials.
///
/// Decryption needs this process to own the device's olm keys. We restore the
/// session using the access token + device id that the main client already
/// obtained, so any credential set carrying a device id works — including SSO.
pub fn e2ee_available(creds: &Credentials) -> bool {
    creds
        .device_id
        .as_deref()
        .map(|d| !d.is_empty())
        .unwrap_or(false)
        && !creds.access_token.is_empty()
        && !creds.user_id.is_empty()
}

/// Start the E2EE sync loop in the background. Returns an error only for setup
/// failures (client build / session restore); the sync loop itself retries.
pub async fn start(
    config: Arc<Config>,
    creds: Credentials,
    bridge: Arc<Bridge>,
) -> anyhow::Result<()> {
    // Persistent store so olm/megolm keys survive restarts.
    let store_dir = config.state_dir.join("crypto-store");
    std::fs::create_dir_all(&store_dir)?;

    let client = Client::builder()
        .homeserver_url(config.homeserver.clone())
        .sqlite_store(&store_dir, None::<&str>)
        .build()
        .await?;

    // Reuse the exact device the main client logged in as. Restoring (instead
    // of logging in again) avoids creating a second device with different keys.
    let user_id = OwnedUserId::try_from(creds.user_id.as_str())
        .map_err(|e| anyhow::anyhow!("invalid user_id '{}': {}", creds.user_id, e))?;
    let device_id = OwnedDeviceId::from(
        creds
            .device_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("no device_id to restore E2EE session"))?,
    );
    let session = MatrixSession {
        meta: SessionMeta { user_id, device_id },
        tokens: SessionTokens {
            access_token: creds.access_token,
            refresh_token: None,
        },
    };
    client.restore_session(session).await?;
    info!(
        "[e2ee] session restored as {} (device from {} login)",
        client.user_id().map(|u| u.as_str()).unwrap_or("?"),
        creds.method
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

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(device: Option<&str>, token: &str, user: &str) -> Credentials {
        Credentials {
            access_token: token.into(),
            user_id: user.into(),
            device_id: device.map(String::from),
            method: "sso",
        }
    }

    #[test]
    fn sso_with_device_enables_e2ee() {
        assert!(e2ee_available(&creds(Some("DEV1"), "syt_x", "@u:s")));
    }

    #[test]
    fn no_device_disables_e2ee() {
        assert!(!e2ee_available(&creds(None, "syt_x", "@u:s")));
        assert!(!e2ee_available(&creds(Some(""), "syt_x", "@u:s")));
    }

    #[test]
    fn empty_token_or_user_disables_e2ee() {
        assert!(!e2ee_available(&creds(Some("DEV1"), "", "@u:s")));
        assert!(!e2ee_available(&creds(Some("DEV1"), "syt_x", "")));
    }
}
