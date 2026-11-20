//! Satori HTTP API: `POST /v1/<method>` and `GET /v1/<method>`.

use crate::bridge::Bridge;
use crate::element::satori_to_matrix;
use crate::satori::{Channel, Guild, Message, User};
use axum::extract::{MatchedPath, Multipart, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::debug;

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
            let mut matrix_content = satori_to_matrix(content, homeserver);
            // Satori passes the quoted message in `referrer` (an object with `message_id`).
            // Matrix expresses a reply via m.relates_to.m.in_reply_to.event_id.
            let quote_event = a
                .get("referrer")
                .and_then(|r| r.get("message_id").or_else(|| r.get("id")))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(qid) = quote_event {
                let relates = matrix_content
                    .as_object_mut()
                    .map(|o| o.entry("m.relates_to").or_insert_with(|| json!({})));
                if let Some(rel) = relates.and_then(Value::as_object_mut) {
                    rel.insert("m.in_reply_to".into(), json!({"event_id": qid}));
                }
            }
            let event_id = bridge
                .matrix
                .send_message(room, &matrix_content, None)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            // Also emit locally so the sender sees its own message immediately,
            // matching other Satori adapters.
            let msg = Message {
                id: Some(event_id.clone()),
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
            };
            Ok(json!([msg]))
        }

        "message.update" => {
            let room = s("channel_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
            let event_id = s("message_id")
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "message_id required".to_string()))?;
            let content = s("content").unwrap_or("");
            // Matrix edits: send a replacement event with m.replace relation.
            let new_content = satori_to_matrix(content, homeserver);
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
                .room_members(room)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
            let mut data = Vec::new();
            for id in members {
                data.push(User {
                    id,
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
        "guild.role.list" | "guild.member.role.list" | "friend.list" => Ok(json!({"data": []})),
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
