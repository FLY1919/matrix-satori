//! Minimal Matrix Client-Server API client (stable v3 endpoints).

use crate::config::Config;
use serde_json::{json, Value};
use std::sync::Arc;
use thiserror::Error;
use tracing::debug;

pub mod e2ee;
pub mod types;
pub use types::*;

#[derive(Debug, Error)]
pub enum MatrixError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Matrix API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, MatrixError>;

#[derive(Clone)]
pub struct MatrixClient {
    http: reqwest::Client,
    config: Arc<Config>,
}

impl MatrixClient {
    pub fn new(config: Arc<Config>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(
                config.sync_timeout + 15_000,
            ))
            .build()?;
        Ok(Self { http, config })
    }

    fn base(&self) -> &str {
        self.config.homeserver.trim_end_matches('/')
    }

    async fn request(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let body = resp.text().await?;
        if status >= 400 {
            return Err(MatrixError::Api { status, body });
        }
        if body.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&body)?)
    }

    /// GET /_matrix/client/v3/account/whoami
    pub async fn whoami(&self) -> Result<String> {
        let v = self
            .request(
                self.http
                    .get(format!("{}/_matrix/client/v3/account/whoami", self.base()))
                    .bearer_auth(&self.config.access_token),
            )
            .await?;
        Ok(v.get("user_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// GET /_matrix/client/v3/sync
    pub async fn sync(&self, since: Option<&str>, timeout_ms: u64) -> Result<SyncResponse> {
        let mut q = vec![
            ("timeout".to_string(), timeout_ms.to_string()),
            (
                "filter".to_string(),
                json!({"room": {"timeline": {"limit": 50}}}).to_string(),
            ),
        ];
        if let Some(s) = since {
            q.push(("since".to_string(), s.to_string()));
        }
        debug!("sync since={:?} timeout={}", since, timeout_ms);
        let v = self
            .request(
                self.http
                    .get(format!("{}/_matrix/client/v3/sync", self.base()))
                    .bearer_auth(&self.config.access_token)
                    .query(&q),
            )
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// PUT /_matrix/client/v3/rooms/{room}/send/m.room.message/{txn}
    pub async fn send_message(
        &self,
        room_id: &str,
        content: &Value,
        txn: Option<&str>,
    ) -> Result<String> {
        let txn = txn
            .map(str::to_string)
            .unwrap_or_else(|| format!("m{}", uuid::Uuid::new_v4().simple()));
        let v = self
            .request(
                self.http
                    .put(format!(
                        "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
                        self.base(),
                        urlencoding(room_id),
                        txn
                    ))
                    .bearer_auth(&self.config.access_token)
                    .json(content),
            )
            .await?;
        Ok(v.get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// PUT /_matrix/client/v3/rooms/{room}/send/m.reaction/{txn}
    pub async fn send_reaction(&self, room_id: &str, event_id: &str, key: &str) -> Result<String> {
        let content = json!({
            "m.relates_to": {
                "rel_type": "m.annotation",
                "event_id": event_id,
                "key": key,
            }
        });
        let v = self
            .request(
                self.http
                    .put(format!(
                        "{}/_matrix/client/v3/rooms/{}/send/m.reaction/m{}",
                        self.base(),
                        urlencoding(room_id),
                        uuid::Uuid::new_v4().simple()
                    ))
                    .bearer_auth(&self.config.access_token)
                    .json(&content),
            )
            .await?;
        Ok(v.get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// PUT /_matrix/client/v3/rooms/{room}/redact/{event}/{txn}
    pub async fn redact(&self, room_id: &str, event_id: &str) -> Result<()> {
        self.request(
            self.http
                .put(format!(
                    "{}/_matrix/client/v3/rooms/{}/redact/{}/m{}",
                    self.base(),
                    urlencoding(room_id),
                    urlencoding(event_id),
                    uuid::Uuid::new_v4().simple()
                ))
                .bearer_auth(&self.config.access_token)
                .json(&json!({})),
        )
        .await?;
        Ok(())
    }

    /// GET /_matrix/client/v3/rooms/{room}/event/{event}
    pub async fn get_event(&self, room_id: &str, event_id: &str) -> Result<Value> {
        self.request(
            self.http
                .get(format!(
                    "{}/_matrix/client/v3/rooms/{}/event/{}",
                    self.base(),
                    urlencoding(room_id),
                    urlencoding(event_id)
                ))
                .bearer_auth(&self.config.access_token),
        )
        .await
    }

    /// GET /_matrix/client/v3/rooms/{room}/messages
    pub async fn messages(
        &self,
        room_id: &str,
        from: Option<&str>,
        dir: &str,
        limit: u64,
    ) -> Result<Value> {
        let mut q: Vec<(String, String)> = vec![
            (String::from("dir"), String::from(dir)),
            (String::from("limit"), limit.to_string()),
        ];
        if let Some(f) = from {
            q.push((String::from("from"), String::from(f)));
        }
        self.request(
            self.http
                .get(format!(
                    "{}/_matrix/client/v3/rooms/{}/messages",
                    self.base(),
                    urlencoding(room_id)
                ))
                .bearer_auth(&self.config.access_token)
                .query(&q),
        )
        .await
    }

    /// GET /_matrix/client/v3/joined_rooms
    pub async fn joined_rooms(&self) -> Result<Vec<String>> {
        let v = self
            .request(
                self.http
                    .get(format!("{}/_matrix/client/v3/joined_rooms", self.base()))
                    .bearer_auth(&self.config.access_token),
            )
            .await?;
        Ok(v.get("joined_rooms")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// GET /_matrix/client/v3/rooms/{room}/state/m.room.name
    pub async fn room_name(&self, room_id: &str) -> Result<Option<String>> {
        let v = self
            .request(
                self.http
                    .get(format!(
                        "{}/_matrix/client/v3/rooms/{}/state/m.room.name",
                        self.base(),
                        urlencoding(room_id)
                    ))
                    .bearer_auth(&self.config.access_token),
            )
            .await;
        match v {
            Ok(v) => Ok(v.get("name").and_then(Value::as_str).map(String::from)),
            Err(MatrixError::Api { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// GET /_matrix/client/v3/rooms/{room}/members
    pub async fn room_members(&self, room_id: &str) -> Result<Vec<String>> {
        let v = self
            .request(
                self.http
                    .get(format!(
                        "{}/_matrix/client/v3/rooms/{}/members",
                        self.base(),
                        urlencoding(room_id)
                    ))
                    .bearer_auth(&self.config.access_token)
                    .query(&[("membership", "join")]),
            )
            .await?;
        Ok(v.get("chunk")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.get("state_key").and_then(Value::as_str))
                    .filter(|s| s.starts_with('@'))
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// GET /_matrix/client/v3/profile/{user_id}
    pub async fn profile(&self, user_id: &str) -> Result<UserProfile> {
        let v = self
            .request(
                self.http
                    .get(format!(
                        "{}/_matrix/client/v3/profile/{}",
                        self.base(),
                        urlencoding(user_id)
                    ))
                    .bearer_auth(&self.config.access_token),
            )
            .await?;
        Ok(UserProfile {
            displayname: v
                .get("displayname")
                .and_then(Value::as_str)
                .map(String::from),
            avatar_url: v
                .get("avatar_url")
                .and_then(Value::as_str)
                .map(String::from),
        })
    }

    /// POST /_matrix/media/v3/upload  -> mxc:// URI
    pub async fn upload_media(&self, content_type: &str, bytes: Vec<u8>) -> Result<String> {
        let v = self
            .request(
                self.http
                    .post(format!("{}/_matrix/media/v3/upload", self.base()))
                    .bearer_auth(&self.config.access_token)
                    .header("Content-Type", content_type)
                    .body(bytes),
            )
            .await?;
        Ok(v.get("content_uri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// GET /_matrix/client/v1/rooms/{room}/relations/{event}/m.annotation/{type}
    /// Returns the list of reaction/annotation events related to an event.
    pub async fn relations(&self, room_id: &str, event_id: &str) -> Result<Vec<Value>> {
        let v = self
            .request(
                self.http
                    .get(format!(
                        "{}/_matrix/client/v1/rooms/{}/relations/{}/m.annotation/m.reaction",
                        self.base(),
                        urlencoding(room_id),
                        urlencoding(event_id)
                    ))
                    .bearer_auth(&self.config.access_token),
            )
            .await
            .unwrap_or(Value::Null);
        Ok(v.get("chunk")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// POST /_matrix/client/v3/createRoom -> new room id. `invitees` are Matrix user ids.
    pub async fn create_room(&self, name: Option<&str>, invitees: &[String]) -> Result<String> {
        let mut body = json!({});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        if !invitees.is_empty() {
            body["invite"] = json!(invitees);
        }
        body["is_direct"] = json!(!invitees.is_empty());
        let v = self
            .request(
                self.http
                    .post(format!("{}/_matrix/client/v3/createRoom", self.base()))
                    .bearer_auth(&self.config.access_token)
                    .json(&body),
            )
            .await?;
        Ok(v.get("room_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// POST /_matrix/client/v3/rooms/{room}/join
    pub async fn join_room(&self, room_id: &str) -> Result<String> {
        let v = self
            .request(
                self.http
                    .post(format!(
                        "{}/_matrix/client/v3/rooms/{}/join",
                        self.base(),
                        urlencoding(room_id)
                    ))
                    .bearer_auth(&self.config.access_token)
                    .json(&json!({})),
            )
            .await?;
        Ok(v.get("room_id")
            .and_then(Value::as_str)
            .unwrap_or(room_id)
            .to_string())
    }

    /// POST /_matrix/client/v3/rooms/{room}/leave
    pub async fn leave_room(&self, room_id: &str) -> Result<()> {
        self.request(
            self.http
                .post(format!(
                    "{}/_matrix/client/v3/rooms/{}/leave",
                    self.base(),
                    urlencoding(room_id)
                ))
                .bearer_auth(&self.config.access_token)
                .json(&json!({})),
        )
        .await?;
        Ok(())
    }

    /// POST /_matrix/client/v3/rooms/{room}/invite
    pub async fn invite_user(&self, room_id: &str, user_id: &str) -> Result<()> {
        self.request(
            self.http
                .post(format!(
                    "{}/_matrix/client/v3/rooms/{}/invite",
                    self.base(),
                    urlencoding(room_id)
                ))
                .bearer_auth(&self.config.access_token)
                .json(&json!({"user_id": user_id})),
        )
        .await?;
        Ok(())
    }

    /// POST /_matrix/client/v3/rooms/{room}/kick
    pub async fn kick_user(
        &self,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({"user_id": user_id});
        if let Some(r) = reason {
            body["reason"] = json!(r);
        }
        self.request(
            self.http
                .post(format!(
                    "{}/_matrix/client/v3/rooms/{}/kick",
                    self.base(),
                    urlencoding(room_id)
                ))
                .bearer_auth(&self.config.access_token)
                .json(&body),
        )
        .await?;
        Ok(())
    }

    /// PUT /_matrix/client/v3/rooms/{room}/state/m.room.name
    pub async fn set_room_name(&self, room_id: &str, name: &str) -> Result<()> {
        self.request(
            self.http
                .put(format!(
                    "{}/_matrix/client/v3/rooms/{}/state/m.room.name",
                    self.base(),
                    urlencoding(room_id)
                ))
                .bearer_auth(&self.config.access_token)
                .json(&json!({"name": name})),
        )
        .await?;
        Ok(())
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
}

#[derive(Debug, Default, Clone)]
pub struct UserProfile {
    pub displayname: Option<String>,
    pub avatar_url: Option<String>,
}

/// Percent-encode path segments (Matrix IDs contain `/` after room id? no, but `:` and `@` need care).
fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encoding() {
        assert_eq!(urlencoding("@bot:example.com"), "%40bot%3Aexample.com");
        assert_eq!(urlencoding("!room:example.com"), "%21room%3Aexample.com");
        assert_eq!(urlencoding("abc-123_X.y~z"), "abc-123_X.y~z");
    }
}
