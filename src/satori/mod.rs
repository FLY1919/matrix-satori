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

/// Satori event; the client dispatches by `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Event {
    pub sn: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub login: Login,
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
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_serializes_snake_case() {
        let login = Login {
            sn: 1,
            adapter: "matrix".into(),
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
        assert_eq!(v["adapter"], "matrix");
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
}
