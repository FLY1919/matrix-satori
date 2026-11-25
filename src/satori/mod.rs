//! Satori protocol data model.
//!
//! Field naming follows the wire format: snake_case, matching what
//! `@satorijs/adapter-satori` sends and expects (it camelizes on receipt).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod api;
pub mod gateway;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    Event = 0,
    Ping = 1,
    Pong = 2,
    Identify = 3,
    Ready = 4,
    Meta = 5,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload<T = Value> {
    pub op: u8,
    pub body: T,
}

impl<T: Serialize> Payload<T> {
    pub fn new(op: Opcode, body: T) -> Self {
        Self { op: op as u8, body }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct User {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_bot: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Guild {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Channel {
    pub id: String,
    /// 0 = TEXT, 1 = DIRECT, 2 = CATEGORY, 3 = VOICE
    #[serde(default)]
    pub r#type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GuildMember {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joined_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Message {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guild: Option<Guild>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<GuildMember>,
    /// Satori element string, e.g. `hello <at id="@u:s"/>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<Box<Message>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Login {
    pub sn: i64,
    pub adapter: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    pub status: u8,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Meta {
    pub logins: Vec<Login>,
    #[serde(default)]
    pub proxy_urls: Vec<String>,
}

/// The reduced login carried by a non-login event. Per Satori protocol, a
/// non-login event's `login` resource contains only `sn`, `user` and
/// `platform` (the full login is delivered once in READY / login.get). The
/// Koishi adapter reads `login.user.id` and `login.platform` from it, so all
/// three fields are present here and nothing extra is emitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EventLogin {
    pub sn: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// Satori event; the client dispatches by `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Event {
    pub sn: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub login: EventLogin,
    pub self_id: String,
    pub platform: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guild: Option<Guild>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<GuildMember>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<User>,
    /// Referrer is always present in the protocol; null is fine.
    pub referrer: Option<Value>,
}

pub const FEATURES: &[&str] = &[
    "message.create",
    "message.get",
    "message.delete",
    "message.list",
    "message.update",
    "channel.get",
    "channel.list",
    "guild.get",
    "guild.list",
    "guild.member.get",
    "guild.member.list",
    "user.get",
    "login.get",
    "reaction.create",
    "reaction.delete",
    "reaction.clear",
    "reaction.list",
    // Matrix-specific extensions (custom methods beyond the Satori standard).
    "matrix.send_raw",
    "matrix.get_state",
    "matrix.get_state_event",
    "matrix.power_levels",
    "matrix.get_event",
    "matrix.room_members",
    "matrix.mxc_to_http",
    "matrix.verify.status",
    "matrix.devices.list",
    "matrix.verify.list",
    "matrix.verify.request",
    "matrix.verify.accept",
    "matrix.verify.start_sas",
    "matrix.verify.sas_info",
    "matrix.verify.confirm",
    "matrix.verify.mismatch",
    "matrix.verify.cancel",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_serializes_snake_case() {
        let login = Login {
            sn: 1,
            adapter: "satori".into(),
            user: Some(User {
                id: "@bot:s".into(),
                ..Default::default()
            }),
            platform: Some("matrix".into()),
            hidden: None,
            status: 1,
            features: vec!["message.create".into()],
        };
        let v = serde_json::to_value(&login).unwrap();
        assert_eq!(v["adapter"], "satori");
        assert_eq!(v["platform"], "matrix");
        assert_eq!(v["user"]["id"], "@bot:s");
        assert!(v.get("self_id").is_none());
    }

    #[test]
    fn payload_opcode_values() {
        assert_eq!(Payload::new(Opcode::Identify, serde_json::json!({})).op, 3);
        assert_eq!(Opcode::Ready as u8, 4);
        assert_eq!(Opcode::Event as u8, 0);
    }

    /// A non-login event must serialize with a reduced `login` (sn/user/platform
    /// only), full `self_id`/`platform` at the top level, and a message whose
    /// `channel.id` equals `guild.id` — the "频道/群聊 id 一致" contract that
    /// Koishi relies on for session routing (session.channelId == guildId).
    #[test]
    fn message_event_is_satori_compliant() {
        let room = "!room:example.org";
        let event = Event {
            sn: 7,
            event_type: "message-created".into(),
            login: EventLogin {
                sn: 1,
                user: Some(User {
                    id: "@bot:example.org".into(),
                    name: Some("bot".into()),
                    ..Default::default()
                }),
                platform: Some("matrix".into()),
            },
            self_id: "@bot:example.org".into(),
            platform: "matrix".into(),
            timestamp: 1234,
            channel: Some(Channel {
                id: room.into(),
                r#type: 0,
                name: Some("group".into()),
                parent_id: None,
                position: None,
            }),
            guild: Some(Guild {
                id: room.into(),
                name: Some("group".into()),
                avatar: None,
            }),
            user: Some(User {
                id: "@alice:example.org".into(),
                ..Default::default()
            }),
            member: None,
            message: Some(Message {
                id: Some("$ev".into()),
                channel: Some(Channel {
                    id: room.into(),
                    r#type: 0,
                    ..Default::default()
                }),
                guild: Some(Guild {
                    id: room.into(),
                    ..Default::default()
                }),
                user: Some(User {
                    id: "@alice:example.org".into(),
                    ..Default::default()
                }),
                content: Some("hi".into()),
                created_at: Some(1234),
                ..Default::default()
            }),
            operator: None,
            referrer: None,
        };

        let v = serde_json::to_value(&event).unwrap();
        // wire format: snake_case, `type` renamed from event_type
        assert_eq!(v["type"], "message-created");
        assert_eq!(v["self_id"], "@bot:example.org");
        assert_eq!(v["platform"], "matrix");
        assert_eq!(v["sn"], 7);
        // reduced login: exactly sn/user/platform, no adapter/status/features
        assert_eq!(v["login"]["sn"], 1);
        assert_eq!(v["login"]["user"]["id"], "@bot:example.org");
        assert_eq!(v["login"]["platform"], "matrix");
        assert!(v["login"].get("adapter").is_none());
        assert!(v["login"].get("status").is_none());
        assert!(v["login"].get("features").is_none());
        // 频道/群聊 id 一致 (event + message 两层都要一致)
        assert_eq!(v["channel"]["id"], v["guild"]["id"]);
        assert_eq!(v["message"]["channel"]["id"], v["message"]["guild"]["id"]);
        assert_eq!(v["channel"]["id"], v["message"]["channel"]["id"]);
        // message content is a Satori element string
        assert_eq!(v["message"]["content"], "hi");
    }

    #[test]
    fn event_login_omits_none_fields() {
        let v = serde_json::to_value(EventLogin {
            sn: 1,
            user: None,
            platform: None,
        })
        .unwrap();
        assert_eq!(v["sn"], 1);
        assert!(v.get("user").is_none());
        assert!(v.get("platform").is_none());
    }
}
