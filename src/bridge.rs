//! Bridges Matrix sync events into Satori events and broadcasts them to clients.

use crate::config::Config;
use crate::element::matrix_to_satori;
use crate::matrix::{EventInfo, MatrixClient, SyncResponse};
use crate::satori::{Channel, Event, Guild, Login, Message, User};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

pub struct Bridge {
    pub matrix: MatrixClient,
    pub config: Arc<Config>,
    pub login: Login,
    pub self_id: String,
    pub tx: broadcast::Sender<Value>,
    last_token: std::sync::Mutex<String>,
    sn: AtomicI64,
    room_names: tokio::sync::RwLock<HashMap<String, String>>,
}

/// Extract the localpart from a Matrix user id: `@fly_hx:knoten.chat` -> `fly_hx`.
/// Koishi's status plugin reads `login.user.name`/`nick` to label the bot; leaving
/// them empty shows a blank name (e.g. `[matrix] : 运行中`). Fall back to the full
/// user id if the input is not in `@local:server` form.
fn display_name_from_user_id(user_id: &str) -> String {
    let rest = user_id.strip_prefix('@').unwrap_or(user_id);
    let local = rest.split(':').next().unwrap_or(rest).trim();
    if local.is_empty() {
        user_id.to_string()
    } else {
        local.to_string()
    }
}

impl Bridge {
    pub fn new(matrix: MatrixClient, config: Arc<Config>, self_id: String) -> Self {
        let display_name = display_name_from_user_id(&self_id);
        let login = Login {
            sn: 1,
            adapter: "matrix".into(),
            user: Some(User {
                id: self_id.clone(),
                name: Some(display_name.clone()),
                nick: Some(display_name),
                avatar: None,
                discriminator: None,
                is_bot: Some(true),
            }),
            platform: Some("matrix".into()),
            hidden: None,
            status: 1,
            features: crate::satori::FEATURES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        let (tx, _) = broadcast::channel(256);
        Self {
            matrix,
            config,
            login,
            self_id,
            tx,
            last_token: std::sync::Mutex::new(String::new()),
            sn: AtomicI64::new(0),
            room_names: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    fn next_sn(&self) -> i64 {
        self.sn.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Load checkpoint token from disk.
    pub fn load_token(&self) -> Option<String> {
        let path = self.config.state_dir.join("sync_token");
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn save_token(&self, token: &str) {
        let path = self.config.state_dir.join("sync_token");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, token) {
            warn!("failed to save sync token: {}", e);
        }
    }

    /// Main sync loop: long-poll Matrix and dispatch Satori events.
    pub async fn run_sync(self: Arc<Self>) {
        let mut since = self.load_token();
        info!("sync loop started, since={:?}", since);
        loop {
            match self
                .matrix
                .sync(since.as_deref(), self.config.sync_timeout)
                .await
            {
                Ok(resp) => {
                    self.handle_sync(resp).await;
                    since = Some(self.current_token());
                    self.save_token(since.as_ref().unwrap());
                }
                Err(e) => {
                    error!("sync failed: {}; retrying in 2s", e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    fn current_token(&self) -> String {
        self.last_token.lock().unwrap().clone()
    }

    async fn handle_sync(&self, resp: SyncResponse) {
        *self.last_token.lock().unwrap() = resp.next_batch.clone();
        for (room_id, room) in &resp.rooms.join {
            // cache room name from state and timeline
            for ev in room.state.events.iter().chain(room.timeline.events.iter()) {
                let info = EventInfo::from_value(ev);
                if info.event_type == "m.room.name" {
                    if let Some(name) = info.content.get("name").and_then(Value::as_str) {
                        self.room_names
                            .write()
                            .await
                            .insert(room_id.clone(), name.to_string());
                    }
                }
            }
            for ev in &room.timeline.events {
                self.handle_room_event(room_id, ev).await;
            }
        }
        for (room_id, invited) in &resp.rooms.invite {
            for ev in &invited.invite_state.events {
                let info = EventInfo::from_value(ev);
                if info.event_type == "m.room.member"
                    && info.state_key.as_deref() == Some(self.self_id.as_str())
                {
                    self.emit_invite(room_id, &info).await;
                }
            }
        }
    }

    async fn handle_room_event(&self, room_id: &str, ev: &Value) {
        let info = EventInfo::from_value(ev);
        match info.event_type.as_str() {
            "m.room.message" => self.emit_message(room_id, &info).await,
            "m.reaction" => self.emit_reaction(room_id, &info).await,
            "m.room.member" => self.emit_membership(room_id, &info).await,
            "m.room.name" => self.emit_channel_update(room_id, &info).await,
            "m.room.topic" => {}
            "m.room.redaction" => self.emit_message_deleted(room_id, &info).await,
            "m.room.encrypted" => {
                warn!(
                    "ignoring encrypted event {} (E2EE not supported)",
                    info.event_id
                );
            }
            _ => debug!("ignoring event type {}", info.event_type),
        }
    }

    pub(crate) async fn emit_message(&self, room_id: &str, info: &EventInfo) {
        // ignore our own echoes for message-created; client handles 'send' echo itself
        let is_self = info.sender == self.self_id;
        let content = matrix_to_satori(&info.content, &self.config.homeserver, &self.self_id);

        let user = User {
            id: info.sender.clone(),
            name: None,
            nick: None,
            avatar: None,
            discriminator: None,
            is_bot: Some(is_self),
        };
        let (channel, guild) = self.room_meta(room_id).await;
        let ts = if info.origin_server_ts > 0 {
            info.origin_server_ts
        } else {
            now_ms()
        };

        let mut message = Message {
            id: Some(info.event_id.clone()),
            channel: Some(channel.clone()),
            guild: guild.clone(),
            user: Some(user.clone()),
            member: None,
            content: Some(content),
            timestamp: Some(ts),
            quote: None,
            created_at: Some(ts),
            updated_at: None,
        };

        // reply relation -> quote
        if let Some(rel) = info
            .content
            .get("m.relates_to")
            .and_then(|r| r.get("m.in_reply_to"))
        {
            if let Some(eid) = rel.get("event_id").and_then(Value::as_str) {
                message.quote = Some(Box::new(Message {
                    id: Some(eid.to_string()),
                    ..Default::default()
                }));
            }
        }
        // edit relation
        if info
            .content
            .get("m.relates_to")
            .and_then(|r| r.get("rel_type"))
            .and_then(Value::as_str)
            == Some("m.replace")
        {
            let new_content = info
                .content
                .get("m.new_content")
                .cloned()
                .unwrap_or(info.content.clone());
            let c = matrix_to_satori(&new_content, &self.config.homeserver, &self.self_id);
            let target = info
                .content
                .get("m.relates_to")
                .and_then(|r| r.get("event_id"))
                .and_then(Value::as_str)
                .unwrap_or(info.event_id.as_str())
                .to_string();
            self.emit_event(
                "message-updated",
                room_id,
                Some(&user),
                Some(Message {
                    id: Some(target),
                    content: Some(c),
                    channel: Some(channel),
                    guild,
                    timestamp: Some(ts),
                    ..Default::default()
                }),
                None,
            )
            .await;
            return;
        }

        // The adapter emits a 'send' event client-side when user_id == self_id.
        self.emit_event("message-created", room_id, Some(&user), Some(message), None)
            .await;
    }

    async fn emit_reaction(&self, room_id: &str, info: &EventInfo) {
        let rel = match info.content.get("m.relates_to") {
            Some(r) => r,
            None => return,
        };
        if rel.get("rel_type").and_then(Value::as_str) != Some("m.annotation") {
            return;
        }
        let key = rel
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let target = rel
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let user = User {
            id: info.sender.clone(),
            name: None,
            nick: None,
            avatar: None,
            discriminator: None,
            is_bot: None,
        };
        let (channel, guild) = self.room_meta(room_id).await;
        let ts = if info.origin_server_ts > 0 {
            info.origin_server_ts
        } else {
            now_ms()
        };
        let mut event = self.base_event("reaction-added", ts, Some(&user));
        event.channel = Some(channel);
        event.guild = guild;
        event.message = Some(Message {
            id: Some(target),
            content: Some(key),
            ..Default::default()
        });
        self.broadcast(event);
    }

    async fn emit_membership(&self, room_id: &str, info: &EventInfo) {
        let membership = info
            .content
            .get("membership")
            .and_then(Value::as_str)
            .unwrap_or("");
        let state_key = info.state_key.clone().unwrap_or_default();
        let event_type = match membership {
            "join" => "guild-member-added",
            "leave" | "ban" => "guild-member-removed",
            "invite" => "guild-member-request",
            _ => return,
        };
        let user = User {
            id: state_key.clone(),
            name: None,
            nick: None,
            avatar: None,
            discriminator: None,
            is_bot: None,
        };
        let mut event = self.base_event(event_type, now_ms(), Some(&user));
        event.guild = Some(Guild {
            id: room_id.to_string(),
            name: None,
            avatar: None,
        });
        event.channel = Some(Channel {
            id: room_id.to_string(),
            r#type: 0,
            name: None,
            parent_id: None,
            position: None,
        });
        event.member = Some(crate::satori::GuildMember {
            user: Some(user),
            name: None,
            nick: None,
            avatar: None,
            joined_at: None,
        });
        self.broadcast(event);
    }

    async fn emit_invite(&self, room_id: &str, _info: &EventInfo) {
        let mut event = self.base_event("guild-request", now_ms(), None);
        event.guild = Some(Guild {
            id: room_id.to_string(),
            name: None,
            avatar: None,
        });
        self.broadcast(event);
    }

    async fn emit_channel_update(&self, room_id: &str, info: &EventInfo) {
        let name = info
            .content
            .get("name")
            .and_then(Value::as_str)
            .map(String::from);
        if let Some(n) = &name {
            self.room_names
                .write()
                .await
                .insert(room_id.to_string(), n.clone());
        }
        let (channel, guild) = self.room_meta(room_id).await;
        let mut event = self.base_event("channel-updated", now_ms(), None);
        event.channel = Some(channel);
        event.guild = guild;
        self.broadcast(event);
    }

    async fn emit_message_deleted(&self, room_id: &str, info: &EventInfo) {
        let redacts = info
            .content
            .get("redacts")
            .or_else(|| {
                info.unsigned
                    .as_ref()
                    .and_then(|u| u.get("redacted_event_id"))
            })
            .and_then(Value::as_str)
            .unwrap_or(info.event_id.as_str())
            .to_string();
        let (channel, guild) = self.room_meta(room_id).await;
        self.emit_event(
            "message-deleted",
            room_id,
            None,
            Some(Message {
                id: Some(redacts),
                channel: Some(channel),
                guild,
                ..Default::default()
            }),
            None,
        )
        .await;
    }

    async fn emit_event(
        &self,
        kind: &str,
        _room_id: &str,
        user: Option<&User>,
        message: Option<Message>,
        _operator: Option<&User>,
    ) {
        let ts = message
            .as_ref()
            .and_then(|m| m.timestamp)
            .unwrap_or_else(now_ms);
        let mut event = self.base_event(kind, ts, user);
        if let Some(m) = message {
            event.channel = m.channel.clone();
            event.guild = m.guild.clone();
            event.message = Some(m);
        }
        self.broadcast(event);
    }

    fn base_event(&self, kind: &str, timestamp: i64, user: Option<&User>) -> Event {
        Event {
            sn: self.next_sn(),
            event_type: kind.to_string(),
            login: self.login.clone(),
            self_id: self.self_id.clone(),
            platform: "matrix".into(),
            timestamp,
            channel: None,
            guild: None,
            user: user.cloned(),
            member: None,
            message: None,
            operator: None,
            referrer: None,
        }
    }

    async fn room_meta(&self, room_id: &str) -> (Channel, Option<Guild>) {
        let name = self.room_names.read().await.get(room_id).cloned();
        (
            Channel {
                id: room_id.to_string(),
                r#type: 0,
                name: name.clone(),
                parent_id: None,
                position: None,
            },
            Some(Guild {
                id: room_id.to_string(),
                name,
                avatar: None,
            }),
        )
    }

    fn broadcast(&self, event: Event) {
        match serde_json::to_value(&event) {
            Ok(v) => {
                if let Err(e) = self.tx.send(v) {
                    debug!("no subscribers ({}): {}", e, event.event_type);
                }
            }
            Err(e) => error!("failed to serialize event: {}", e),
        }
    }

    /// Drain the room-name cache for `channel.list` / `guild.list`.
    pub async fn rooms(&self) -> Vec<(String, Option<String>)> {
        self.room_names
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::display_name_from_user_id;

    #[test]
    fn display_name_from_regular_matrix_id() {
        assert_eq!(display_name_from_user_id("@fly_hx:knoten.chat"), "fly_hx");
    }

    #[test]
    fn display_name_from_id_with_server_port() {
        assert_eq!(display_name_from_user_id("@bot:example.com:8448"), "bot");
    }

    #[test]
    fn display_name_fallback_to_full_id() {
        assert_eq!(
            display_name_from_user_id("not-a-matrix-id"),
            "not-a-matrix-id"
        );
        assert_eq!(display_name_from_user_id("@:server"), "@:server");
    }
}
