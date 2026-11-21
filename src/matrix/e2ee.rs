//! Single Matrix sync and I/O engine built on the official `matrix-sdk`.
//!
//! The login is resolved by our own `auth` module (all methods, incl. SSO),
//! then the resulting credentials are handed to the SDK via `restore_session`.
//! The SDK owns the ONLY sync loop: it transparently decrypts encrypted rooms,
//! so plain and encrypted messages arrive through the same handlers — no
//! duplicate events, no second lightweight sync.
//!
//! Sending also goes through the SDK (`Room::send_raw`), which encrypts
//! automatically in encrypted rooms and works in plain rooms too.
//!
//! Keys persist in a SQLite store at `<state-dir>/crypto-store`, so decryption
//! survives restarts. Messages from before this device existed still cannot be
//! decrypted without cross-signing / key backup (a Matrix protocol limitation).

use crate::auth::Credentials;
use crate::bridge::Bridge;
use crate::config::Config;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::event_handler::Ctx;
use matrix_sdk::ruma::events::room::message::SyncRoomMessageEvent;
use matrix_sdk::ruma::{OwnedDeviceId, OwnedUserId};
use matrix_sdk::{Client, Room, SessionMeta, SessionTokens};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{error, info};

/// Any credential carrying a device id can drive the SDK engine. A bare
/// access token (no device id) or appservice has no device to restore.
pub fn e2ee_available(creds: &Credentials) -> bool {
    creds
        .device_id
        .as_deref()
        .map(|d| !d.is_empty())
        .unwrap_or(false)
        && !creds.access_token.is_empty()
        && !creds.user_id.is_empty()
}

/// Build + restore the SDK client, register every event handler, start the
/// single sync loop, and install the client into `Bridge` so Satori API calls
/// (send message / reaction / upload) can reuse it.
pub async fn start(
    config: Arc<Config>,
    creds: Credentials,
    bridge: Arc<Bridge>,
) -> anyhow::Result<()> {
    let store_dir = config.state_dir.join("crypto-store");
    std::fs::create_dir_all(&store_dir)?;

    let client = Client::builder()
        .homeserver_url(config.homeserver.clone())
        .sqlite_store(&store_dir, None::<&str>)
        .build()
        .await?;

    let user_id = OwnedUserId::try_from(creds.user_id.as_str())
        .map_err(|e| anyhow::anyhow!("invalid user_id '{}': {}", creds.user_id, e))?;
    // Even a bare access-token can drive the SDK sync: Matrix lets any token
    // sync under an arbitrary device id. There are no olm keys in that case, so
    // encrypted rooms stay undecryptable, but plain rooms work and we keep a
    // single sync engine for every login method.
    let device_id = creds
        .device_id
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(OwnedDeviceId::from)
        .unwrap_or_else(|| OwnedDeviceId::from(format!("MS_{}", uuid::Uuid::new_v4().simple())));
    let session = MatrixSession {
        meta: SessionMeta { user_id, device_id },
        tokens: SessionTokens {
            access_token: creds.access_token,
            refresh_token: None,
        },
    };
    client.restore_session(session).await?;
    info!(
        "[sdk] session restored as {} (device from {} login)",
        client.user_id().map(|u| u.as_str()).unwrap_or("?"),
        creds.method
    );

    // Install this client into the bridge BEFORE sync so API sends reuse it.
    bridge.set_sdk_client(client.clone());

    // Register handlers. The SDK delivers decrypted content for encrypted rooms
    // and plain content for plain rooms through these same typed handlers.
    client.add_event_handler_context(bridge.clone());
    client.add_event_handler(
        |event: SyncRoomMessageEvent, room: Room, bridge: Ctx<Arc<Bridge>>| async move {
            if let Some(v) = message_value(&event, room.room_id().as_str()) {
                bridge.handle_event_json(room.room_id().as_str(), &v).await;
            }
        },
    );

    // Initial sync establishes room state and uploads olm keys.
    let token = client.sync_once(SyncSettings::default()).await?.next_batch;
    info!("[sdk] sync loop started (plain + encrypted rooms)");

    let settings = SyncSettings::default().token(token);
    if let Err(e) = client.sync(settings).await {
        error!("[sdk] sync ended: {}", e);
    }
    Ok(())
}

/// Rebuild the wire JSON shape that `EventInfo::from_value` expects from a
/// decrypted (or plain) `SyncRoomMessageEvent`.
fn message_value(event: &SyncRoomMessageEvent, room_id: &str) -> Option<Value> {
    let (event_id, sender, ts, content) = match event {
        SyncRoomMessageEvent::Original(e) => {
            let content = serde_json::to_value(&e.content).ok()?;
            (
                e.event_id.as_str().to_string(),
                e.sender.as_str().to_string(),
                u64::from(e.origin_server_ts.0) as i64,
                content,
            )
        }
        SyncRoomMessageEvent::Redacted(_) => return None,
    };
    Some(json!({
        "type": "m.room.message",
        "event_id": event_id,
        "sender": sender,
        "origin_server_ts": ts,
        "room_id": room_id,
        "content": content,
    }))
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
    fn sso_with_device_enables_sdk() {
        assert!(e2ee_available(&creds(Some("DEV1"), "syt_x", "@u:s")));
    }

    #[test]
    fn no_device_disables_sdk() {
        assert!(!e2ee_available(&creds(None, "syt_x", "@u:s")));
        assert!(!e2ee_available(&creds(Some(""), "syt_x", "@u:s")));
    }

    #[test]
    fn empty_token_or_user_disables_sdk() {
        assert!(!e2ee_available(&creds(Some("DEV1"), "", "@u:s")));
        assert!(!e2ee_available(&creds(Some("DEV1"), "syt_x", "")));
    }
}
