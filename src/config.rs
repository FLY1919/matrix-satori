use clap::{Parser, ValueEnum};
use std::path::PathBuf;

/// Which Matrix login flow to use. `auto` picks based on supplied credentials
/// and what the homeserver advertises.
#[derive(Parser, Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum LoginMethod {
    Auto,
    Password,
    Token,
    Sso,
    ApplicationService,
    AccessToken,
}

/// Satori protocol server backed by a Matrix homeserver.
#[derive(Parser, Debug, Clone)]
#[command(name = "matrix-satori", version, about, long_about = None)]
pub struct Config {
    /// Matrix homeserver base URL, e.g. https://mozilla.modular.im
    #[arg(
        long,
        env = "MATRIX_HOMESERVER",
        default_value = "http://localhost:8008"
    )]
    pub homeserver: String,

    /// Login flow to use. auto detects from the credentials you provide.
    #[arg(long, env = "MATRIX_LOGIN_METHOD", value_enum, default_value = "auto")]
    pub login_method: LoginMethod,

    /// Pre-obtained access token (highest priority). Skips interactive login.
    #[arg(long, env = "MATRIX_ACCESS_TOKEN", default_value = "")]
    pub access_token: String,

    /// Login username / localpart for password login (e.g. `mybot` or `@mybot:server`).
    #[arg(long, env = "MATRIX_USERNAME", default_value = "")]
    pub username: String,

    /// Password for password login.
    #[arg(long, env = "MATRIX_PASSWORD", default_value = "")]
    pub password: String,

    /// loginToken obtained from an SSO flow (exchanged via m.login.token).
    #[arg(long, env = "MATRIX_LOGIN_TOKEN", default_value = "")]
    pub login_token: String,

    /// Device ID to use when logging in (optional).
    #[arg(long, env = "MATRIX_DEVICE_ID", default_value = "")]
    pub device_id: String,

    /// Bot's Matrix user ID, e.g. @bot:example.com (auto-detected if empty).
    #[arg(long, env = "MATRIX_USER_ID", default_value = "")]
    pub user_id: String,

    /// Application-service token (used with --login-method application-service).
    #[arg(long, env = "MATRIX_AS_TOKEN", default_value = "")]
    pub as_token: String,

    /// For interactive SSO: local address for the login-token callback server.
    #[arg(
        long,
        env = "MATRIX_SSO_CALLBACK_BIND",
        default_value = "127.0.0.1:8088"
    )]
    pub sso_callback_bind: String,

    /// For interactive SSO: externally reachable redirect URI. If empty it is
    /// derived from --sso-callback-bind as http://<bind>/satori-sso-callback.
    #[arg(long, env = "MATRIX_SSO_REDIRECT_URI", default_value = "")]
    pub sso_redirect_uri: String,

    /// Address to listen on for Satori clients (Koishi).
    #[arg(long, env = "SATORI_BIND", default_value = "127.0.0.1:5140")]
    pub bind: String,

    /// Shared token required from Satori clients via `Authorization: Bearer <token>`.
    /// Leave empty to disable authentication (not recommended for public binds).
    #[arg(long, env = "SATORI_TOKEN", default_value = "")]
    pub satori_token: String,

    /// Path prefix for the Satori endpoints. Empty means `/v1/...` at root.
    #[arg(long, env = "SATORI_PATH", default_value = "")]
    pub path_prefix: String,

    /// Sync timeout in milliseconds passed to Matrix /sync.
    #[arg(long, env = "MATRIX_SYNC_TIMEOUT", default_value_t = 30000)]
    pub sync_timeout: u64,

    /// Directory for state: sync checkpoint and cached credentials.
    #[arg(long, env = "MATRIX_STATE_DIR", default_value = "./data")]
    pub state_dir: PathBuf,

    /// Log filter, e.g. info, matrix_satori=debug.
    #[arg(long, env = "RUST_LOG", default_value = "info")]
    pub log: String,
}

impl Config {
    pub fn normalized_prefix(&self) -> String {
        let p = self.path_prefix.trim();
        if p.is_empty() || p == "/" {
            String::new()
        } else {
            format!("/{}", p.trim_matches('/'))
        }
    }

    /// Matrix media download URL for an mxc:// URI.
    pub fn mxc_to_http(&self, mxc: &str) -> String {
        match mxc.strip_prefix("mxc://") {
            Some(rest) => format!(
                "{}/_matrix/media/v3/download/{}",
                self.homeserver.trim_end_matches('/'),
                rest
            ),
            None => mxc.to_string(),
        }
    }

    pub fn sso_redirect_uri(&self) -> String {
        if self.sso_redirect_uri.is_empty() {
            format!("http://{}/satori-sso-callback", self.sso_callback_bind)
        } else {
            self.sso_redirect_uri.clone()
        }
    }
}
