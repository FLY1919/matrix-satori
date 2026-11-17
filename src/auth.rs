//! Matrix authentication supporting every common login flow.
//!
//! Supported (detected from `GET /_matrix/client/v3/login`):
//!   - `m.login.password`            username + password
//!   - `m.login.token`               login token (typically from an SSO callback)
//!   - `m.login.sso`                 interactive web login with local callback server
//!   - `m.login.application_service` appservice token + user id
//!   - direct access token           skip login entirely
//!
//! Nothing is hard-coded to a particular homeserver; flows are discovered at runtime.

use crate::config::{Config, LoginMethod};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info, warn};

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Matrix API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AuthError>;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_token: String,
    pub user_id: String,
    pub device_id: Option<String>,
    pub method: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoginFlows {
    #[serde(default)]
    pub flows: Vec<LoginFlow>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoginFlow {
    #[serde(rename = "type")]
    pub flow_type: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    access_token: String,
    user_id: String,
    #[serde(default)]
    device_id: Option<String>,
}

pub struct AuthClient {
    http: reqwest::Client,
    config: Config,
}

impl AuthClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            config: config.clone(),
        })
    }

    fn base(&self) -> &str {
        self.config.homeserver.trim_end_matches('/')
    }

    async fn post_json(&self, path: &str, body: &Value, token: Option<&str>) -> Result<Value> {
        let mut req = self.http.post(format!("{}{}", self.base(), path));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.json(body).send().await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        if status >= 400 {
            return Err(AuthError::Api { status, body: text });
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    async fn get_json(&self, path: &str, token: Option<&str>) -> Result<Value> {
        let mut req = self.http.get(format!("{}{}", self.base(), path));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        if status >= 400 {
            return Err(AuthError::Api { status, body: text });
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Discover login flows advertised by the homeserver.
    pub async fn flows(&self) -> Result<Vec<String>> {
        let v = self.get_json("/_matrix/client/v3/login", None).await?;
        let flows: LoginFlows = serde_json::from_value(v)?;
        Ok(flows.flows.into_iter().map(|f| f.flow_type).collect())
    }

    fn has_flow(flows: &[String], name: &str) -> bool {
        flows.iter().any(|f| f == name)
    }

    /// Resolve credentials using the configured login method (auto by default).
    pub async fn resolve(&self) -> Result<Credentials> {
        // 1. Direct access token always wins.
        if !self.config.access_token.is_empty() && self.config.login_method == LoginMethod::Auto {
            return self.as_access_token().await;
        }
        if self.config.login_method == LoginMethod::AccessToken {
            if self.config.access_token.is_empty() {
                return Err(AuthError::Other("--access-token is empty".into()));
            }
            return self.as_access_token().await;
        }

        let flows = self.flows().await.unwrap_or_else(|e| {
            warn!(
                "could not discover login flows: {}; will try configured method",
                e
            );
            Vec::new()
        });
        info!("homeserver login flows: {:?}", flows);

        match self.config.login_method {
            LoginMethod::Password => self.login_password(&flows).await,
            LoginMethod::Token => self.login_token(&flows).await,
            LoginMethod::Sso => self.login_sso(&flows).await,
            LoginMethod::ApplicationService => self.login_appservice().await,
            LoginMethod::AccessToken => unreachable!(),
            LoginMethod::Auto => self.auto(&flows).await,
        }
    }

    async fn auto(&self, flows: &[String]) -> Result<Credentials> {
        // priority: login token > password > appservice > interactive SSO
        if !self.config.login_token.is_empty() {
            return self.login_token(flows).await;
        }
        if !self.config.username.is_empty() && !self.config.password.is_empty() {
            return self.login_password(flows).await;
        }
        if !self.config.as_token.is_empty() && !self.config.user_id.is_empty() {
            return self.login_appservice().await;
        }
        if Self::has_flow(flows, "m.login.sso") || Self::has_flow(flows, "org.matrix.login.sso") {
            return self.login_sso(flows).await;
        }
        if Self::has_flow(flows, "m.login.password") {
            return Err(AuthError::Other(
                "server supports password login; provide --username and --password".into(),
            ));
        }
        Err(AuthError::Other(
            "no usable login flow: provide --access-token, --login-token, username/password, or ensure server supports SSO".into(),
        ))
    }

    async fn as_access_token(&self) -> Result<Credentials> {
        let user_id = if self.config.user_id.is_empty() {
            let v = self
                .get_json(
                    "/_matrix/client/v3/account/whoami",
                    Some(&self.config.access_token),
                )
                .await?;
            v.get("user_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        } else {
            self.config.user_id.clone()
        };
        if user_id.is_empty() {
            return Err(AuthError::Other(
                "could not determine user_id from access token; set --user-id".into(),
            ));
        }
        Ok(Credentials {
            access_token: self.config.access_token.clone(),
            user_id,
            device_id: None,
            method: "access-token",
        })
    }

    async fn login_password(&self, flows: &[String]) -> Result<Credentials> {
        if self.config.username.is_empty() || self.config.password.is_empty() {
            return Err(AuthError::Other(
                "password login needs --username and --password".into(),
            ));
        }
        if !flows.is_empty() && !Self::has_flow(flows, "m.login.password") {
            warn!("server does not advertise m.login.password; trying anyway");
        }
        let mut body = json!({
            "type": "m.login.password",
            "identifier": {"type": "m.id.user", "user": self.config.username},
            "password": self.config.password,
        });
        if !self.config.device_id.is_empty() {
            body["device_id"] = json!(self.config.device_id);
        }
        let v = self
            .post_json("/_matrix/client/v3/login", &body, None)
            .await?;
        Self::parse_login(v, "password")
    }

    async fn login_token(&self, _flows: &[String]) -> Result<Credentials> {
        if self.config.login_token.is_empty() {
            return Err(AuthError::Other(
                "token login needs --login-token (the loginToken from an SSO callback)".into(),
            ));
        }
        let mut body = json!({"type": "m.login.token", "token": self.config.login_token});
        if !self.config.device_id.is_empty() {
            body["device_id"] = json!(self.config.device_id);
        }
        let v = self
            .post_json("/_matrix/client/v3/login", &body, None)
            .await?;
        Self::parse_login(v, "token")
    }

    async fn login_appservice(&self) -> Result<Credentials> {
        if self.config.as_token.is_empty() || self.config.user_id.is_empty() {
            return Err(AuthError::Other(
                "appservice login needs --as-token and --user-id".into(),
            ));
        }
        // Appservice login uses the AS token as bearer and the user_id as query param.
        let path = format!(
            "/_matrix/client/v3/login?user_id={}",
            url_encode(&self.config.user_id)
        );
        let body = json!({"type": "m.login.application_service", "identifier": {
            "type": "m.id.user", "user": self.config.user_id
        }});
        let v = self
            .post_json(&path, &body, Some(&self.config.as_token))
            .await?;
        Self::parse_login(v, "application-service")
    }

    async fn login_sso(&self, _flows: &[String]) -> Result<Credentials> {
        // Interactive SSO: spin up a local callback server, print the login URL,
        // wait for the homeserver to redirect back with a loginToken, exchange it.
        use axum::extract::Query;
        use axum::routing::get;
        use axum::Router;
        use std::collections::HashMap;
        use tokio::sync::oneshot;

        let redirect_uri = self.config.sso_redirect_uri();
        let (tx, rx) = oneshot::channel::<String>();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let redirect_path = "/satori-sso-callback";

        let app = Router::new().route(
            redirect_path,
            get(move |Query(params): Query<HashMap<String, String>>| {
                let tx = tx.clone();
                async move {
                    if let Some(token) = params.get("loginToken") {
                        if let Some(sender) = tx.lock().unwrap().take() {
                            let _ = sender.send(token.clone());
                        }
                        axum::response::Html(
                            "<h1>登录成功</h1><p>可以关闭此页面，返回终端继续。</p>",
                        )
                    } else {
                        axum::response::Html("<h1>缺少 loginToken</h1>")
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind(&self.config.sso_callback_bind).await?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let sso_url = format!(
            "{}/_matrix/client/v3/login/sso/redirect?redirectUrl={}",
            self.base(),
            url_encode(&redirect_uri)
        );
        info!("SSO login required. Open this URL in your browser:");
        println!(
            "\n  请在浏览器打开以下链接完成 SSO 登录：\n\n  {}\n",
            sso_url
        );

        let login_token = tokio::time::timeout(Duration::from_secs(300), rx)
            .await
            .map_err(|_| AuthError::Other("SSO login timed out (5 minutes)".into()))?
            .map_err(|_| AuthError::Other("SSO callback channel closed".into()))?;

        server.abort();

        let mut body = json!({"type": "m.login.token", "token": login_token});
        if !self.config.device_id.is_empty() {
            body["device_id"] = json!(self.config.device_id);
        }
        let v = self
            .post_json("/_matrix/client/v3/login", &body, None)
            .await?;
        Self::parse_login(v, "sso")
    }

    fn parse_login(v: Value, method: &'static str) -> Result<Credentials> {
        let resp: LoginResponse = serde_json::from_value(v).map_err(|e| {
            // Give a helpful message if the server rejected the login.
            AuthError::Decode(e)
        })?;
        Ok(Credentials {
            access_token: resp.access_token,
            user_id: resp.user_id,
            device_id: resp.device_id,
            method,
        })
    }
}

/// Helper: persist credentials to disk for reuse across restarts.
pub fn save_credentials(config: &Config, creds: &Credentials) -> Result<()> {
    let path = config.state_dir.join("credentials.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[derive(Serialize)]
    struct Stored {
        homeserver: String,
        access_token: String,
        user_id: String,
        device_id: Option<String>,
        method: &'static str,
    }
    let stored = Stored {
        homeserver: config.homeserver.clone(),
        access_token: creds.access_token.clone(),
        user_id: creds.user_id.clone(),
        device_id: creds.device_id.clone(),
        method: creds.method,
    };
    std::fs::write(&path, serde_json::to_string_pretty(&stored)?)?;
    debug!("saved credentials to {:?}", path);
    Ok(())
}

/// Load cached credentials if they match this homeserver.
pub fn load_credentials(config: &Config) -> Option<Credentials> {
    let path = config.state_dir.join("credentials.json");
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v.get("homeserver").and_then(Value::as_str) != Some(config.homeserver.as_str()) {
        return None;
    }
    Some(Credentials {
        access_token: v.get("access_token").and_then(Value::as_str)?.to_string(),
        user_id: v.get("user_id").and_then(Value::as_str)?.to_string(),
        device_id: v.get("device_id").and_then(Value::as_str).map(String::from),
        method: "cached",
    })
}

fn url_encode(s: &str) -> String {
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
    use crate::config::Config;
    use clap::Parser;

    fn base_cfg(args: &[&str]) -> Config {
        Config::parse_from(args)
    }

    #[test]
    fn url_encoding_login_token() {
        assert_eq!(
            url_encode("https://x.com/cb?a=1&b=2"),
            "https%3A%2F%2Fx.com%2Fcb%3Fa%3D1%26b%3D2"
        );
    }

    #[test]
    fn parse_password_login_response() {
        let v = json!({"access_token":"syt_abc","user_id":"@bot:s","device_id":"DEV"});
        let c = AuthClient::parse_login(v, "password").unwrap();
        assert_eq!(c.access_token, "syt_abc");
        assert_eq!(c.user_id, "@bot:s");
        assert_eq!(c.device_id.as_deref(), Some("DEV"));
        assert_eq!(c.method, "password");
    }

    #[test]
    fn parse_token_login_response() {
        let v = json!({"access_token":"syt_xyz","user_id":"@u:s"});
        let c = AuthClient::parse_login(v, "token").unwrap();
        assert_eq!(c.user_id, "@u:s");
        assert_eq!(c.method, "token");
    }

    #[test]
    fn parse_login_flows() {
        let f: LoginFlows = serde_json::from_value(json!({"flows":[
            {"type":"m.login.password"},{"type":"m.login.sso"},{"type":"m.login.token"}
        ]}))
        .unwrap();
        let names: Vec<_> = f.flows.iter().map(|x| x.flow_type.clone()).collect();
        assert!(names.contains(&"m.login.password".to_string()));
        assert!(names.contains(&"m.login.sso".to_string()));
    }

    #[test]
    fn login_method_env() {
        let c = base_cfg(&["matrix-satori", "--login-method", "sso"]);
        assert_eq!(c.login_method, LoginMethod::Sso);
    }
}
