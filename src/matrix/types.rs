//! Wire types for Matrix /sync responses and room events.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SyncResponse {
    #[serde(default)]
    pub next_batch: String,
    #[serde(default)]
    pub rooms: Rooms,
    #[serde(default)]
    pub account_data: Option<Value>,
    #[serde(default)]
    pub presence: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Rooms {
    #[serde(default)]
    pub join: std::collections::HashMap<String, JoinedRoom>,
    #[serde(default)]
    pub invite: std::collections::HashMap<String, InvitedRoom>,
    #[serde(default)]
    pub leave: std::collections::HashMap<String, LeftRoom>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JoinedRoom {
    #[serde(default)]
    pub timeline: Timeline,
    #[serde(default)]
    pub state: State,
    #[serde(default)]
    pub ephemeral: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InvitedRoom {
    #[serde(default)]
    pub invite_state: State,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LeftRoom {
    #[serde(default)]
    pub timeline: Timeline,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Timeline {
    #[serde(default)]
    pub events: Vec<Value>,
    #[serde(default)]
    pub limited: bool,
    #[serde(default)]
    pub prev_batch: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct State {
    #[serde(default)]
    pub events: Vec<Value>,
}

/// A joined room member with the profile fields carried by the membership
/// event. Used to build complete Satori `User` objects for member lists.
#[derive(Debug, Clone, Default)]
pub struct RoomMemberDetail {
    pub user_id: String,
    pub displayname: Option<String>,
    pub avatar_url: Option<String>,
}

/// Extract common fields from a Matrix room event.
#[derive(Debug, Clone, Default)]
pub struct EventInfo {
    pub event_id: String,
    pub sender: String,
    pub event_type: String,
    pub origin_server_ts: i64,
    pub state_key: Option<String>,
    pub content: Value,
    pub unsigned: Option<Value>,
}

impl EventInfo {
    pub fn from_value(v: &Value) -> Self {
        Self {
            event_id: v
                .get("event_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            sender: v.get("sender").and_then(Value::as_str).unwrap_or("").into(),
            event_type: v.get("type").and_then(Value::as_str).unwrap_or("").into(),
            origin_server_ts: v
                .get("origin_server_ts")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            state_key: v.get("state_key").and_then(Value::as_str).map(String::from),
            content: v
                .get("content")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default())),
            unsigned: v.get("unsigned").cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sync() {
        let v = serde_json::json!({
            "next_batch": "s1",
            "rooms": {"join": {"!r:s": {"timeline": {"events": [
                {"type": "m.room.message", "event_id": "$e", "sender": "@u:s",
                 "origin_server_ts": 1234, "content": {"msgtype": "m.text", "body": "hi"}}
            ]}}}}
        });
        let s: SyncResponse = serde_json::from_value(v).unwrap();
        assert_eq!(s.next_batch, "s1");
        let room = &s.rooms.join["!r:s"];
        assert_eq!(room.timeline.events.len(), 1);
        let info = EventInfo::from_value(&room.timeline.events[0]);
        assert_eq!(info.event_id, "$e");
        assert_eq!(info.sender, "@u:s");
        assert_eq!(info.event_type, "m.room.message");
    }
}
