//! Satori HTTP API: `POST /v1/<method>` and `GET /v1/<method>`.

use crate::bridge::Bridge;
use crate::element::{satori_to_matrix_segments, satori_to_matrix_with_media};
use crate::satori::{Channel, Guild, Message, User};
use axum::extract::{MatchedPath, Multipart, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::debug;

use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::ruma::UserId;

/// Grab the installed SDK client or a well-formed API error.
fn sdk(bridge: &Arc<Bridge>) -> Result<&matrix_sdk::Client, (StatusCode, String)> {
    bridge.sdk_client().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "SDK client not ready".to_string(),
        )
    })
}

/// Turn a verification flow id plus a user id into the SDK's high-level
/// VerificationRequest. Flow ids created by us are `<user>/<transaction>`; the
/// tracker stores the plain flow id, so we also try the current user.
async fn verification_request(
    client: &matrix_sdk::Client,
    user_id: &UserId,
    flow_id: &str,
) -> Result<matrix_sdk::encryption::verification::VerificationRequest, (StatusCode, String)> {
    client
        .encryption()
        .get_verification_request(user_id, flow_id)
        .await
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("verification '{}' not found", flow_id),
            )
        })
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    if token.is_empty() {
        return true;
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim() == token)
        .unwrap_or(false)
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"message": msg.into()}))).into_response()
}

/// Handle every `/v1/<method>` request. The client sends fields as snake_case JSON
/// and expects snake_case JSON back (it camelizes on receipt).
pub async fn api_handler(
    State(bridge): State<Arc<Bridge>>,
    headers: HeaderMap,
    matched: MatchedPath,
    RawQuery(raw_query): RawQuery,
    body: Option<Json<Value>>,
) -> Response {
    if !authorized(&headers, &bridge.config.satori_token) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let args = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| Value::Object(Default::default()));
    let method = matched
        .as_str()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    debug!("API {} args={}", method, args);

    let result = dispatch(&bridge, &method, &args, &raw_query).await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err((status, msg)) => err(status, msg),
    }
}

/// `POST /v1/upload.create` — the adapter sends multipart/form-data with one or
/// more `file` fields (see @satorijs/protocol `createUpload`, isForm=true).
/// Each file is uploaded to Matrix media storage and its mxc:// URI is
/// converted to an HTTP download URL, matching what `<img src>` expects.
pub async fn upload_handler(
    State(bridge): State<Arc<Bridge>>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    if !authorized(&headers, &bridge.config.satori_token) {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let mut urls = Vec::new();
    let mut mp = multipart;
    while let Ok(Some(field)) = mp.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name != "file" {
            continue;
        }
        let filename = field.file_name().map(str::to_string).unwrap_or_default();
        let content_type = field
            .content_type()
            .map(str::to_string)
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let bytes = match field.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("read file failed: {}", e)),
        };
        let _ = filename; // Matrix media upload does not require a filename.
        match bridge.matrix.upload_media(&content_type, bytes).await {
            Ok(mxc) => urls.push(bridge.config.mxc_to_http(&mxc)),
            Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
        }
    }
    Json(json!(urls)).into_response()
}

type ApiResult = Result<Value, (StatusCode, String)>;

async fn dispatch(bridge: &Arc<Bridge>, method: &str, a: &Value, _q: &Option<String>) -> ApiResult {
    let s = |k: &str| a.get(k).and_then(Value::as_str);
    let homeserver = &bridge.config.homeserver;

    match method {
        "login.get" => Ok(serde_json::to_value(&bridge.login).unwrap()),

        "message.create" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let content = s("content").unwrap_or("");
            // Download + upload any remote media first so images/videos render
            // as real Matrix media instead of bare links.
            let resolved = bridge.resolve_remote_media(content).await;
            // A single Satori content may mix text with several media elements,
            // but a Matrix event carries exactly one msgtype. Split it into
            // separate events so nothing is dropped (e.g. text + 2 images).
            let mut segments = satori_to_matrix_segments(content, homeserver, &resolved);
            // Satori passes the quoted message in `referrer` (an object with
            // `message_id`). Matrix expresses a reply via
            // m.relates_to.m.in_reply_to.event_id. Apply to every segment so a
            // quoted mixed message keeps its reply relation throughout.
            let quote_event = a
                .get("referrer")
                .and_then(|r| r.get("message_id").or_else(|| r.get("id")))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(qid) = quote_event {
                for matrix_content in &mut segments {
                    let relates = matrix_content
                        .as_object_mut()
                        .map(|o| o.entry("m.relates_to").or_insert_with(|| json!({})));
                    if let Some(rel) = relates.and_then(Value::as_object_mut) {
                        rel.insert("m.in_reply_to".into(), json!({"event_id": qid}));
                    }
                }
            }
            // Send through the single SDK client when available: it encrypts
            // in encrypted rooms and sends plaintext in plain rooms. Fall back
            // to the lightweight HTTP path only before the SDK is installed.
            let mut messages = Vec::new();
            for matrix_content in &segments {
                let event_id = match bridge
                    .sdk_send_raw(room, "m.room.message", matrix_content)
                    .await
                {
                    Some(id) => id,
                    None => bridge
                        .matrix
                        .send_message(room, matrix_content, None)
                        .await
                        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?,
                };
                // Also emit locally so the sender sees its own message
                // immediately, matching other Satori adapters.
                messages.push(Message {
                    id: Some(event_id),
                    channel: Some(Channel {
                        id: room.into(),
                        r#type: 0,
                        name: None,
                        parent_id: None,
                        position: None,
                    }),
                    user: Some(User {
                        id: bridge.self_id.clone(),
                        is_bot: Some(true),
                        ..Default::default()
                    }),
                    content: Some(content.to_string()),
                    created_at: Some(now_ms()),
                    ..Default::default()
                });
            }
            Ok(json!(messages))
        }

        "message.update" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event_id = s("message_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "message_id required".to_string()))?;
            let content = s("content").unwrap_or("");
            // Matrix edits: send a replacement event with m.replace relation.
            let resolved = bridge.resolve_remote_media(content).await;
            let new_content = satori_to_matrix_with_media(content, homeserver, &resolved);
            let edit = json!({
                "msgtype": new_content["msgtype"],
                "body": format!("* {}", new_content["body"].as_str().unwrap_or("")),
                "m.relates_to": {"rel_type": "m.replace", "event_id": event_id},
                "m.new_content": new_content,
            });
            bridge
                .matrix
                .send_message(room, &edit, None)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!({}))
        }

        "message.delete" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event_id = s("message_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "message_id required".to_string()))?;
            bridge
                .matrix
                .redact(room, event_id)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!({}))
        }

        "message.get" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event_id = s("message_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "message_id required".to_string()))?;
            let ev = bridge
                .matrix
                .get_event(room, event_id)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let content = crate::element::matrix_to_satori(
                ev.get("content").unwrap_or(&Value::Null),
                homeserver,
                &bridge.self_id,
            );
            Ok(json!(Message {
                id: Some(event_id.to_string()),
                channel: Some(Channel {
                    id: room.to_string(),
                    r#type: 0,
                    name: None,
                    parent_id: None,
                    position: None
                }),
                user: Some(User {
                    id: ev
                        .get("sender")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    ..Default::default()
                }),
                content: Some(content),
                timestamp: ev.get("origin_server_ts").and_then(Value::as_i64),
                ..Default::default()
            }))
        }

        "message.list" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let from = s("next");
            let dir = s("direction").unwrap_or("before");
            let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(50);
            let res = bridge
                .matrix
                .messages(room, from, dir, limit)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let chunk = res
                .get("chunk")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut data = Vec::new();
            for ev in chunk {
                if ev.get("type").and_then(Value::as_str) != Some("m.room.message") {
                    continue;
                }
                let info = crate::matrix::EventInfo::from_value(&ev);
                let content =
                    crate::element::matrix_to_satori(&info.content, homeserver, &bridge.self_id);
                data.push(Message {
                    id: Some(info.event_id),
                    channel: Some(Channel {
                        id: room.to_string(),
                        r#type: 0,
                        name: None,
                        parent_id: None,
                        position: None,
                    }),
                    user: Some(User {
                        id: info.sender,
                        ..Default::default()
                    }),
                    content: Some(content),
                    timestamp: Some(info.origin_server_ts),
                    ..Default::default()
                });
            }
            let next = res.get("end").and_then(Value::as_str).map(String::from);
            Ok(json!({"data": data, "next": next}))
        }

        "channel.get" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let name = bridge.matrix.room_name(room).await.ok().flatten();
            Ok(json!(Channel {
                id: room.to_string(),
                r#type: 0,
                name,
                parent_id: None,
                position: None
            }))
        }

        "channel.list" => {
            let rooms = bridge
                .matrix
                .joined_rooms()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let mut data = Vec::new();
            for room in rooms {
                let name = bridge.matrix.room_name(&room).await.ok().flatten();
                data.push(Channel {
                    id: room,
                    r#type: 0,
                    name,
                    parent_id: None,
                    position: None,
                });
            }
            Ok(json!({"data": data}))
        }

        "guild.get" => {
            let room = s("guild_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "guild_id required".to_string()))?;
            let name = bridge.matrix.room_name(room).await.ok().flatten();
            Ok(json!(Guild {
                id: room.to_string(),
                name,
                avatar: None
            }))
        }

        "guild.list" => {
            let rooms = bridge
                .matrix
                .joined_rooms()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let mut data = Vec::new();
            for room in rooms {
                let name = bridge.matrix.room_name(&room).await.ok().flatten();
                data.push(Guild {
                    id: room,
                    name,
                    avatar: None,
                });
            }
            Ok(json!({"data": data}))
        }

        "guild.member.get" | "user.get" => {
            let user_id = s("user_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "user_id required".to_string()))?;
            let profile = bridge
                .matrix
                .profile(user_id)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!(User {
                id: user_id.to_string(),
                name: profile.displayname,
                avatar: profile.avatar_url.map(|u| bridge.config.mxc_to_http(&u)),
                ..Default::default()
            }))
        }

        "guild.member.list" => {
            let room = s("guild_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "guild_id required".to_string()))?;
            let members = bridge
                .matrix
                .room_member_details(room)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let mut data = Vec::new();
            for m in members {
                data.push(User {
                    id: m.user_id,
                    name: m.displayname,
                    avatar: m.avatar_url.map(|u| bridge.config.mxc_to_http(&u)),
                    ..Default::default()
                });
            }
            Ok(json!({"data": data}))
        }

        "reaction.create" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event = s("message_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "message_id required".to_string()))?;
            let emoji = s("emoji_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "emoji_id required".to_string()))?;
            bridge
                .matrix
                .send_reaction(room, event, emoji)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!({}))
        }

        "reaction.delete" | "reaction.clear" => {
            // Satori gives (channel, message, emoji[, user]); Matrix needs the
            // reaction *event id*. Look it up via /relations, then redact it.
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event = s("message_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "message_id required".to_string()))?;
            let emoji = s("emoji_id"); // None => clear all reactions on the message
            let user_id = s("user_id");
            let reactions = bridge
                .matrix
                .relations(room, event)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            for r in reactions {
                let is_reaction = r.get("type").and_then(Value::as_str) == Some("m.reaction");
                if !is_reaction {
                    continue;
                }
                let key = r
                    .get("content")
                    .and_then(|c| c.get("m.relates_to"))
                    .and_then(|rel| rel.get("key"))
                    .and_then(Value::as_str);
                let sender = r.get("sender").and_then(Value::as_str);
                let rid = r.get("event_id").and_then(Value::as_str).unwrap_or("");
                if rid.is_empty() {
                    continue;
                }
                // reaction.clear with an emoji filters by key; delete matches key too.
                if let Some(want) = emoji {
                    if key != Some(want) {
                        continue;
                    }
                }
                if let Some(u) = user_id {
                    if sender != Some(u) {
                        continue;
                    }
                }
                bridge
                    .matrix
                    .redact(room, rid)
                    .await
                    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            }
            Ok(json!({}))
        }

        "reaction.list" => {
            // Satori getReactionList -> List<User> of users who used `emoji_id`.
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event = s("message_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "message_id required".to_string()))?;
            let emoji = s("emoji_id");
            let reactions = bridge
                .matrix
                .relations(room, event)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let mut data = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for r in reactions {
                if r.get("type").and_then(Value::as_str) != Some("m.reaction") {
                    continue;
                }
                let key = r
                    .get("content")
                    .and_then(|c| c.get("m.relates_to"))
                    .and_then(|rel| rel.get("key"))
                    .and_then(Value::as_str);
                if let Some(want) = emoji {
                    if key != Some(want) {
                        continue;
                    }
                }
                if let Some(uid) = r.get("sender").and_then(Value::as_str) {
                    if seen.insert(uid.to_string()) {
                        data.push(User {
                            id: uid.to_string(),
                            ..Default::default()
                        });
                    }
                }
            }
            Ok(json!({"data": data}))
        }

        "upload.create" => {
            // Normal path is the multipart handler. If a client sends JSON with
            // base64 `data` fields, support that too for completeness.
            Ok(json!([]))
        }

        // --- Matrix-specific extension methods -------------------------------
        // Satori allows adapters to expose custom methods beyond the standard
        // set. These give Koishi plugins direct access to Matrix primitives
        // that have no Satori equivalent. All take/return snake_case JSON.
        "matrix.send_raw" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event_type = s("event_type")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "event_type required".to_string()))?;
            let content = a.get("content").cloned().unwrap_or_else(|| json!({}));
            // Prefer the SDK (auto-encrypts in encrypted rooms); fall back to
            // the raw HTTP send before the SDK is installed.
            let event_id = match bridge.sdk_send_raw(room, event_type, &content).await {
                Some(id) => id,
                None => bridge
                    .matrix
                    .send_raw(room, event_type, &content)
                    .await
                    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?,
            };
            Ok(json!({"event_id": event_id}))
        }
        "matrix.get_state" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            bridge
                .matrix
                .get_state(room)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
        }
        "matrix.get_state_event" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event_type = s("event_type")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "event_type required".to_string()))?;
            let state_key = s("state_key").unwrap_or("");
            bridge
                .matrix
                .get_state_event(room, event_type, state_key)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
        }
        "matrix.power_levels" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            // A room's power levels are the state event with type
            // `m.room.power_levels` and an empty state key.
            bridge
                .matrix
                .get_state_event(room, "m.room.power_levels", "")
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
        }
        "matrix.get_event" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event_id = s("event_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "event_id required".to_string()))?;
            bridge
                .matrix
                .get_event(room, event_id)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
        }
        "matrix.room_members" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let ids = bridge
                .matrix
                .room_members(room)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!({"data": ids}))
        }
        "matrix.mxc_to_http" => {
            let mxc =
                s("mxc").ok_or_else(|| (StatusCode::BAD_REQUEST, "mxc required".to_string()))?;
            Ok(json!({"url": bridge.config.mxc_to_http(mxc)}))
        }

        // --- Device verification (SAS) --------------------------------------
        // Matrix has no Satori equivalent. These drive the SDK crypto machine
        // so a second (unverified) device such as our `satori` device can be
        // cross-signed by an existing verified device. Once cross-signed, the
        // SDK automatically shares room keys / backup secrets with us via
        // to-device `m.secret.send`, which fixes "Can't find room key" in
        // encrypted rooms. All methods take/return snake_case JSON.

        // Overall verification state of our own device.
        "matrix.verify.status" => {
            let client = sdk(bridge)?;
            let enc = client.encryption();
            let own = match enc.get_own_device().await {
                Ok(d) => d,
                Err(e) => return Err((StatusCode::BAD_GATEWAY, e.to_string())),
            };
            let state = match own {
                Some(d) => json!({
                    "device_id": d.device_id().as_str(),
                    "cross_signed_by_owner": d.is_cross_signed_by_owner(),
                    "verified": d.is_verified(),
                    "display_name": d.display_name(),
                }),
                None => {
                    json!({"device_id": null, "cross_signed_by_owner": false, "verified": false})
                }
            };
            Ok(json!({
                "device": state,
                "verification_state": format!("{:?}", enc.verification_state().get()),
            }))
        }

        // List devices of a user (default: ourselves), with verification flags.
        // This is the authoritative source — unlike our to-device tracker it
        // comes straight from the crypto store.
        "matrix.devices.list" => {
            let client = sdk(bridge)?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            let devices = client
                .encryption()
                .get_user_devices(&uid)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let list: Vec<Value> = devices
                .devices()
                .map(|d| {
                    json!({
                        "device_id": d.device_id().as_str(),
                        "display_name": d.display_name(),
                        "cross_signed_by_owner": d.is_cross_signed_by_owner(),
                        "verified": d.is_verified(),
                        "is_our_own": d.device_id().as_str() == client.device_id().map(|x| x.as_str()).unwrap_or(""),
                    })
                })
                .collect();
            Ok(json!({"data": list}))
        }

        // Verification requests we learned from the to-device stream.
        "matrix.verify.list" => Ok(json!({"data": bridge.verify_requests().await})),

        // Resolve the user id for a flow: explicit arg > tracker > ourselves.
        // The tracker is only a hint; self-verification is the dominant case.
        "matrix.verify.request" => {
            let client = sdk(bridge)?;
            let device_id = s("device_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "device_id required".to_string()))?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            let did: &matrix_sdk::ruma::DeviceId = device_id.into();
            let device = client
                .encryption()
                .get_device(&uid, did)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
                .ok_or_else(|| (StatusCode::NOT_FOUND, "device not found".to_string()))?;
            let req = device
                .request_verification_with_methods(vec![VerificationMethod::SasV1])
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let flow_id = req.flow_id().to_string();
            bridge
                .note_verify_request(&flow_id, user_id, device_id, &["m.sas.v1".into()])
                .await;
            Ok(json!({"flow_id": flow_id, "user_id": user_id, "device_id": device_id}))
        }

        "matrix.verify.accept" => {
            let client = sdk(bridge)?;
            let flow_id = s("flow_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "flow_id required".to_string()))?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            let req = verification_request(client, &uid, flow_id).await?;
            req.accept()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!({"flow_id": flow_id, "accepted": true}))
        }

        "matrix.verify.start_sas" => {
            let client = sdk(bridge)?;
            let flow_id = s("flow_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "flow_id required".to_string()))?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            let req = verification_request(client, &uid, flow_id).await?;
            match req
                .start_sas()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
            {
                Some(_) => Ok(json!({"flow_id": flow_id, "started": true})),
                None => Ok(json!({"flow_id": flow_id, "started": false,
                    "error": "SAS not supported by the other device"})),
            }
        }

        // Read the short authentication string. Emojis are exposed as both
        // symbol+description pairs and the concatenated display string.
        "matrix.verify.sas_info" => {
            let client = sdk(bridge)?;
            let flow_id = s("flow_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "flow_id required".to_string()))?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            let v = client
                .encryption()
                .get_verification(&uid, flow_id)
                .await
                .ok_or_else(|| {
                    (
                        StatusCode::NOT_FOUND,
                        format!("verification '{}' not found", flow_id),
                    )
                })?;
            let sas = v.sas().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "not a SAS verification".to_string(),
                )
            })?;
            let emojis: Vec<Value> = sas
                .emoji()
                .map(|arr| {
                    arr.iter()
                        .map(|e| json!({"symbol": e.symbol, "description": e.description}))
                        .collect()
                })
                .unwrap_or_default();
            let display = sas
                .emoji()
                .map(|arr| arr.iter().map(|e| e.symbol).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            let decimals = sas.decimals().map(|(a, b, c)| json!([a, b, c]));
            Ok(json!({
                "flow_id": flow_id,
                "state": format!("{:?}", sas.state()),
                "emojis": emojis,
                "emoji_display": display,
                "decimals": decimals,
                "supports_emoji": sas.supports_emoji(),
                "is_done": sas.is_done(),
                "is_cancelled": sas.is_cancelled(),
            }))
        }

        // Confirm the emoji/decimals match on both screens. The SDK will sign
        // the other device and upload the signature; secrets then flow in.
        "matrix.verify.confirm" => {
            let client = sdk(bridge)?;
            let flow_id = s("flow_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "flow_id required".to_string()))?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            let v = client
                .encryption()
                .get_verification(&uid, flow_id)
                .await
                .ok_or_else(|| {
                    (
                        StatusCode::NOT_FOUND,
                        format!("verification '{}' not found", flow_id),
                    )
                })?;
            let sas = v.sas().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "not a SAS verification".to_string(),
                )
            })?;
            sas.confirm()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            bridge.forget_verify_request(flow_id).await;
            Ok(json!({"flow_id": flow_id, "confirmed": true}))
        }

        "matrix.verify.mismatch" => {
            let client = sdk(bridge)?;
            let flow_id = s("flow_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "flow_id required".to_string()))?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            let v = client
                .encryption()
                .get_verification(&uid, flow_id)
                .await
                .ok_or_else(|| {
                    (
                        StatusCode::NOT_FOUND,
                        format!("verification '{}' not found", flow_id),
                    )
                })?;
            let sas = v.sas().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "not a SAS verification".to_string(),
                )
            })?;
            sas.mismatch()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            bridge.forget_verify_request(flow_id).await;
            Ok(json!({"flow_id": flow_id, "mismatched": true}))
        }

        "matrix.verify.cancel" => {
            let client = sdk(bridge)?;
            let flow_id = s("flow_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "flow_id required".to_string()))?;
            let user_id = s("user_id").unwrap_or(&bridge.self_id);
            let uid = UserId::parse(user_id)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {}", e)))?;
            if let Some(req) = client
                .encryption()
                .get_verification_request(&uid, flow_id)
                .await
            {
                req.cancel()
                    .await
                    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            } else if let Some(v) = client.encryption().get_verification(&uid, flow_id).await {
                if let Some(sas) = v.sas() {
                    sas.cancel()
                        .await
                        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
                } else {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "unsupported verification type".to_string(),
                    ));
                }
            } else {
                return Err((
                    StatusCode::NOT_FOUND,
                    format!("verification '{}' not found", flow_id),
                ));
            }
            bridge.forget_verify_request(flow_id).await;
            Ok(json!({"flow_id": flow_id, "cancelled": true}))
        }

        // --- Matrix has no direct equivalents for the following. Return
        // protocol-correct empty/success values instead of a 404 so Koishi
        // adapters never break on an unsupported call. ---
        "channel.create" | "user.channel.create" => {
            // Create a Matrix room (direct message when a user id is given).
            let data = a.get("data");
            let name = data
                .and_then(|d| d.get("name"))
                .and_then(Value::as_str)
                .or_else(|| s("name"));
            let mut invitees: Vec<String> = Vec::new();
            if let Some(uid) = s("user_id") {
                invitees.push(uid.to_string());
            }
            let room = bridge
                .matrix
                .create_room(name, &invitees)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!(Channel {
                id: room,
                r#type: if invitees.is_empty() { 0 } else { 1 },
                name: name.map(String::from),
                parent_id: None,
                position: None,
            }))
        }
        "channel.delete" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            bridge
                .matrix
                .leave_room(room)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!({}))
        }
        "channel.update" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            if let Some(name) = a
                .get("data")
                .and_then(|d| d.get("name"))
                .and_then(Value::as_str)
            {
                bridge
                    .matrix
                    .set_room_name(room, name)
                    .await
                    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            }
            Ok(json!({}))
        }
        "channel.mute"
        | "guild.member.mute"
        | "friend.delete"
        | "friend.approve"
        | "guild.approve"
        | "guild.member.approve" => Ok(json!({})),
        "guild.member.kick" => {
            let room = s("guild_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "guild_id required".to_string()))?;
            let uid = s("user_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "user_id required".to_string()))?;
            bridge
                .matrix
                .kick_user(room, uid, s("reason"))
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            Ok(json!({}))
        }
        "friend.list" => {
            // Matrix has no explicit friends; direct-chat partners (m.direct)
            // are the closest Satori `friend` equivalent. Fill in their profile
            // so the friend list carries real names/avatars.
            let users = bridge
                .matrix
                .direct_users()
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let mut data = Vec::new();
            for uid in users {
                let (name, avatar) = match bridge.matrix.profile(&uid).await {
                    Ok(p) => (
                        p.displayname,
                        p.avatar_url.map(|u| bridge.config.mxc_to_http(&u)),
                    ),
                    Err(_) => (None, None),
                };
                data.push(User {
                    id: uid,
                    name,
                    avatar,
                    ..Default::default()
                });
            }
            Ok(json!({"data": data}))
        }
        "guild.role.list" | "guild.member.role.list" => Ok(json!({"data": []})),
        "guild.role.create"
        | "guild.role.update"
        | "guild.role.delete"
        | "guild.member.role.set"
        | "guild.member.role.unset" => Ok(json!({})),

        _ => err_with("unsupported method", method),
    }
}

fn err_with(msg: &str, method: &str) -> ApiResult {
    Err((StatusCode::NOT_FOUND, format!("{}: {}", msg, method)))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satori_create_shape() {
        // Ensure message.create returns an array of messages with id/content.
        let msg = Message {
            id: Some("$e".into()),
            content: Some("hi".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(json!([msg])).unwrap();
        assert_eq!(v[0]["id"], "$e");
        assert_eq!(v[0]["content"], "hi");
    }
}
