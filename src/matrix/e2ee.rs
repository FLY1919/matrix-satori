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
use matrix_sdk::ruma::events::{AnySyncStateEvent, AnySyncTimelineEvent, AnyToDeviceEvent};
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedDeviceId, OwnedUserId};
use matrix_sdk::{Client, Room, SessionMeta, SessionTokens};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

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

/// Build an SDK client backed by the persistent per-account sqlite store and
/// restore the session from `creds`. Shared by the long-running sync engine
/// and the one-shot `verify` CLI so both operate on the SAME device/olm keys.
pub async fn build_client(config: &Config, creds: &Credentials) -> anyhow::Result<Client> {
    // The SDK sqlite store binds to one account (user_id + device_id) and
    // refuses to open if a different account was stored there ("account in the
    // store doesn't match the account in the constructor"). Namespace the store
    // per account so multiple accounts / a changed device can coexist under the
    // same state_dir without clobbering each other.
    let account_key = format!(
        "{}@{}",
        creds.user_id.replace(['@', ':', '/', '\\'], "_"),
        creds
            .device_id
            .as_deref()
            .unwrap_or("nodevice")
            .replace(['/', '\\'], "_")
    );
    let store_dir = config.state_dir.join("crypto-store").join(account_key);
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
            access_token: creds.access_token.clone(),
            refresh_token: None,
        },
    };
    client.restore_session(session).await?;
    info!(
        "[sdk] session restored as {} (device from {} login)",
        client.user_id().map(|u| u.as_str()).unwrap_or("?"),
        creds.method
    );
    Ok(client)
}

/// Build + restore the SDK client, register every event handler, start the
/// single sync loop, and install the client into `Bridge` so Satori API calls
/// (send message / reaction / upload) can reuse it.
pub async fn start(
    config: Arc<Config>,
    creds: Credentials,
    bridge: Arc<Bridge>,
) -> anyhow::Result<()> {
    let client = build_client(&config, &creds).await?;

    // Install this client into the bridge BEFORE sync so API sends reuse it.
    bridge.set_sdk_client(client.clone());

    // Register TWO generic handlers covering EVERY sync event:
    //   - AnySyncTimelineEvent: m.room.message, m.reaction, m.room.redaction,
    //     m.sticker, and all other timeline (message-like) events.
    //   - AnySyncStateEvent: m.room.member, m.room.name, m.room.pinned_events,
    //     and all other state events.
    // Previously only message/sticker typed handlers were registered, which
    // silently dropped reaction/membership/name/redaction/pin events. Serializing
    // the whole event to wire JSON feeds the single `handle_event_json` entry
    // point, which `EventInfo::from_value` already understands. The SDK still
    // decrypts encrypted rooms before these handlers run.
    client.add_event_handler_context(bridge.clone());
    client.add_event_handler(
        |event: Raw<AnySyncTimelineEvent>, room: Room, bridge: Ctx<Arc<Bridge>>| async move {
            let room_id = room.room_id().as_str();
            if let Ok(mut v) = serde_json::to_value(&event) {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("room_id".into(), json!(room_id));
                }
                bridge.handle_event_json(room_id, &v).await;
            }
        },
    );
    client.add_event_handler(
        |event: Raw<AnySyncStateEvent>, room: Room, bridge: Ctx<Arc<Bridge>>| async move {
            let room_id = room.room_id().as_str();
            if let Ok(mut v) = serde_json::to_value(&event) {
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("room_id".into(), json!(room_id));
                }
                bridge.handle_event_json(room_id, &v).await;
            }
        },
    );

    // Key-verification requests arrive on the to-device stream (never in a
    // room timeline). The SDK crypto machine consumes these to build its own
    // VerificationRequest objects; this read-only handler just learns the
    // flow_id so Koishi can drive the SAS flow via `matrix.verify.*` calls.
    // It must not mutate the request — only observe and announce it.
    client.add_event_handler(
        |event: AnyToDeviceEvent, bridge: Ctx<Arc<Bridge>>| async move {
            if let AnyToDeviceEvent::KeyVerificationRequest(ev) = &event {
                let flow_id = ev.content.transaction_id.as_str();
                let methods: Vec<String> = ev
                    .content
                    .methods
                    .iter()
                    .map(|m| {
                        serde_json::to_value(m)
                            .map(|v| v.as_str().unwrap_or("").to_string())
                            .unwrap_or_default()
                    })
                    .filter(|s| !s.is_empty())
                    .collect();
                // The request to-device event carries the sender user but not
                // its device id; `matrix.verify.list` enriches it from the
                // crypto VerificationRequest later. Use the sender as a stable
                // placeholder here.
                bridge
                    .note_verify_request(flow_id, ev.sender.as_str(), "", &methods)
                    .await;
            }
        },
    );

    // Persistent sync loop.
    //
    // A Matrix `/sync` is a long poll: transient transport failures (TLS cut,
    // IncompleteMessage, proxy hiccup) are normal and must NOT kill the bridge.
    // The SDK returns Err on such failures instead of retrying internally, so
    // we wrap sync() in a backoff loop here. SyncToken defaults to ReusePrevious,
    // meaning the SDK resumes from the token persisted in the sqlite store —
    // no events are lost and we don't re-sync from genesis on reconnect.
    info!("[sdk] sync loop started (plain + encrypted rooms)");
    let mut backoff_secs: u64 = 1;
    loop {
        // default() => token = ReusePrevious (auto-resume from store).
        match client.sync(SyncSettings::default()).await {
            Ok(()) => {
                // sync() only returns Ok if it was stopped cleanly; treat as a
                // reconnect with a short delay and reset backoff.
                warn!(
                    "[sdk] sync stopped cleanly, reconnecting in {}s",
                    backoff_secs
                );
            }
            Err(e) => {
                error!("[sdk] sync error (reconnect in {}s): {}", backoff_secs, e);
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = next_backoff(backoff_secs);
    }
    #[allow(unreachable_code)]
    Ok(())
}

/// Exponential backoff with a 30s ceiling: 1 → 2 → 4 → 8 → 16 → 30 → 30 ...
const MAX_BACKOFF: u64 = 30;
fn next_backoff(current: u64) -> u64 {
    current.saturating_mul(2).min(MAX_BACKOFF)
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
    fn sync_backoff_doubles_and_caps() {
        assert_eq!(next_backoff(1), 2);
        assert_eq!(next_backoff(2), 4);
        assert_eq!(next_backoff(8), 16);
        assert_eq!(next_backoff(16), 30);
        assert_eq!(next_backoff(30), 30);
        assert_eq!(next_backoff(u64::MAX), 30); // saturating, no overflow
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
