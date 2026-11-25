use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which Matrix login flow to use. `auto` picks based on supplied credentials
/// and what the homeserver advertises.
#[derive(Parser, Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[clap(rename_all = "lower")]
#[serde(rename_all = "lowercase")]
pub enum LoginMethod {
    Auto,
    Password,
    Token,
    Sso,
    ApplicationService,
    AccessToken,
}

/// Satori protocol server backed by a Matrix homeserver.
#[derive(Parser, Debug, Clone, Serialize, Deserialize)]
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

    /// Human-readable device name shown in Element's device list. Applied via
    /// `PUT /_matrix/client/v3/devices/{deviceId}` after login. Set empty to skip.
    #[arg(long, env = "MATRIX_DEVICE_DISPLAY_NAME", default_value = "satori")]
    pub device_display_name: String,

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

/// Default filename written into the state directory on first run.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// CLI / environment overrides. Every field is optional so that only values
/// the user actually supplied take precedence over the TOML file. This is
/// separate from `Config` (which carries clap `default_value`s) so tests and
/// existing callers of `Config::parse_from` keep working unchanged.
#[derive(Parser, Debug, Default, Clone)]
#[command(name = "matrix-satori", version, about, long_about = None)]
pub struct CliOverrides {
    #[arg(long, env = "MATRIX_HOMESERVER")]
    pub homeserver: Option<String>,
    #[arg(long, env = "MATRIX_LOGIN_METHOD")]
    pub login_method: Option<LoginMethod>,
    #[arg(long, env = "MATRIX_ACCESS_TOKEN")]
    pub access_token: Option<String>,
    #[arg(long, env = "MATRIX_USERNAME")]
    pub username: Option<String>,
    #[arg(long, env = "MATRIX_PASSWORD")]
    pub password: Option<String>,
    #[arg(long, env = "MATRIX_LOGIN_TOKEN")]
    pub login_token: Option<String>,
    #[arg(long, env = "MATRIX_DEVICE_ID")]
    pub device_id: Option<String>,
    #[arg(long, env = "MATRIX_DEVICE_DISPLAY_NAME")]
    pub device_display_name: Option<String>,
    #[arg(long, env = "MATRIX_USER_ID")]
    pub user_id: Option<String>,
    #[arg(long, env = "MATRIX_AS_TOKEN")]
    pub as_token: Option<String>,
    #[arg(long, env = "MATRIX_SSO_CALLBACK_BIND")]
    pub sso_callback_bind: Option<String>,
    #[arg(long, env = "MATRIX_SSO_REDIRECT_URI")]
    pub sso_redirect_uri: Option<String>,
    #[arg(long, env = "SATORI_BIND")]
    pub bind: Option<String>,
    #[arg(long, env = "SATORI_TOKEN")]
    pub satori_token: Option<String>,
    #[arg(long, env = "SATORI_PATH")]
    pub path_prefix: Option<String>,
    #[arg(long, env = "MATRIX_SYNC_TIMEOUT")]
    pub sync_timeout: Option<u64>,
    #[arg(long, env = "MATRIX_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    #[arg(long, env = "RUST_LOG")]
    pub log: Option<String>,
    /// Explicit path to a config file (overrides <state-dir>/config.toml).
    #[arg(long, env = "MATRIX_CONFIG")]
    pub config: Option<PathBuf>,
}

impl Config {
    /// Built-in defaults, matching the clap `default_value`s above.
    pub fn defaults() -> Self {
        Self {
            homeserver: "http://localhost:8008".into(),
            login_method: LoginMethod::Auto,
            access_token: String::new(),
            username: String::new(),
            password: String::new(),
            login_token: String::new(),
            device_id: String::new(),
            device_display_name: "satori".into(),
            user_id: String::new(),
            as_token: String::new(),
            sso_callback_bind: "127.0.0.1:8088".into(),
            sso_redirect_uri: String::new(),
            bind: "127.0.0.1:5140".into(),
            satori_token: String::new(),
            path_prefix: String::new(),
            sync_timeout: 30000,
            state_dir: PathBuf::from("./data"),
            log: "info".into(),
        }
    }

    /// Load configuration: built-in defaults <- config.toml <- CLI/env.
    /// On first run a commented default `config.toml` is created in the state
    /// directory. Any field supplied via CLI/env overrides the file.
    pub fn load() -> anyhow::Result<Self> {
        let cli = CliOverrides::parse();
        Self::load_from(&cli)
    }

    pub fn load_from(cli: &CliOverrides) -> anyhow::Result<Self> {
        // The state dir itself may be overridden on the CLI; resolve it first.
        let state_dir = cli
            .state_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("./data"));
        let config_path = cli
            .config
            .clone()
            .unwrap_or_else(|| state_dir.join(CONFIG_FILE_NAME));

        let mut config = if config_path.exists() {
            let text = std::fs::read_to_string(&config_path)?;
            toml::from_str::<Config>(&text).map_err(|e| {
                anyhow::anyhow!("invalid config file {}: {}", config_path.display(), e)
            })?
        } else {
            let defaults = Self::defaults();
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let text = toml::to_string_pretty(&defaults)?;
            std::fs::write(&config_path, format!("# matrix-satori config\n# Edit this file, then restart. CLI/env values override these.\n\n{}", text))?;
            tracing::info!("created default config at {}", config_path.display());
            defaults
        };

        // Ensure the state dir used at runtime matches whichever was resolved.
        config.state_dir = state_dir;

        // Apply explicit CLI/env overrides.
        macro_rules! apply {
            ($field:ident) => {
                if let Some(v) = &cli.$field {
                    config.$field = v.clone();
                }
            };
        }
        apply!(homeserver);
        apply!(login_method);
        apply!(access_token);
        apply!(username);
        apply!(password);
        apply!(login_token);
        apply!(device_id);
        apply!(device_display_name);
        apply!(user_id);
        apply!(as_token);
        apply!(sso_callback_bind);
        apply!(sso_redirect_uri);
        apply!(bind);
        apply!(satori_token);
        apply!(path_prefix);
        apply!(sync_timeout);
        apply!(log);
        // state_dir already applied above.
        Ok(config)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("matrix-satori-cfg-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn overrides_none() -> CliOverrides {
        CliOverrides::default()
    }

    #[test]
    fn first_run_creates_config_toml() {
        let dir = tmp_dir("first");
        let cli = CliOverrides {
            state_dir: Some(dir.clone()),
            ..overrides_none()
        };
        let cfg = Config::load_from(&cli).unwrap();
        let path = dir.join(CONFIG_FILE_NAME);
        assert!(path.exists(), "config.toml should be created");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("matrix-satori config"));
        assert_eq!(cfg.homeserver, Config::defaults().homeserver);
        assert_eq!(cfg.bind, "127.0.0.1:5140");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_existing_toml() {
        let dir = tmp_dir("read");
        std::fs::create_dir_all(&dir).unwrap();
        let mut custom = Config::defaults();
        custom.homeserver = "https://example.org".into();
        custom.login_method = LoginMethod::Password;
        custom.username = "bot".into();
        std::fs::write(
            dir.join(CONFIG_FILE_NAME),
            toml::to_string_pretty(&custom).unwrap(),
        )
        .unwrap();
        let cli = CliOverrides {
            state_dir: Some(dir.clone()),
            ..overrides_none()
        };
        let cfg = Config::load_from(&cli).unwrap();
        assert_eq!(cfg.homeserver, "https://example.org");
        assert_eq!(cfg.login_method, LoginMethod::Password);
        assert_eq!(cfg.username, "bot");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cli_overrides_toml() {
        let dir = tmp_dir("override");
        std::fs::create_dir_all(&dir).unwrap();
        let mut custom = Config::defaults();
        custom.homeserver = "https://from-file.example".into();
        std::fs::write(
            dir.join(CONFIG_FILE_NAME),
            toml::to_string_pretty(&custom).unwrap(),
        )
        .unwrap();
        let cli = CliOverrides {
            state_dir: Some(dir.clone()),
            homeserver: Some("https://from-cli.example".into()),
            bind: Some("0.0.0.0:9999".into()),
            ..overrides_none()
        };
        let cfg = Config::load_from(&cli).unwrap();
        assert_eq!(cfg.homeserver, "https://from-cli.example");
        assert_eq!(cfg.bind, "0.0.0.0:9999");
        std::fs::remove_dir_all(dir).ok();
    }
}
