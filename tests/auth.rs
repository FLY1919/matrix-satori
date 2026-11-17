//! Integration tests for every Matrix login flow against a mock homeserver.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use clap::Parser;
use matrix_satori::auth::AuthClient;
use matrix_satori::config::{Config, LoginMethod};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct AuthMock {
    /// Which flows to advertise.
    flows: Vec<String>,
    /// Count password logins.
    password_logins: AtomicUsize,
    /// Count token logins.
    token_logins: AtomicUsize,
}

async fn login_flows(State(m): State<Arc<AuthMock>>) -> Json<Value> {
    let flows: Vec<Value> = m.flows.iter().map(|f| json!({"type": f})).collect();
    Json(json!({"flows": flows}))
}

async fn login(State(m): State<Arc<AuthMock>>, Json(body): Json<Value>) -> Json<Value> {
    let kind = body.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "m.login.password" => {
            m.password_logins.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "access_token": "syt_password_token",
                "user_id": "@bot:mock",
                "device_id": "PWDEV"
            }))
        }
        "m.login.token" => {
            m.token_logins.fetch_add(1, Ordering::SeqCst);
            let token = body.get("token").and_then(Value::as_str).unwrap_or("");
            Json(json!({
                "access_token": format!("syt_from_{}", token),
                "user_id": "@ssobot:mock"
            }))
        }
        "m.login.application_service" => Json(json!({
            "access_token": "syt_appservice_token",
            "user_id": "@appservice:mock"
        })),
        _ => Json(json!({"error": "unknown flow"})),
    }
}

async fn whoami() -> Json<Value> {
    Json(json!({"user_id": "@direct:mock"}))
}

async fn mock_router(m: Arc<AuthMock>) -> Router {
    Router::new()
        .route("/_matrix/client/v3/login", get(login_flows).post(login))
        .route("/_matrix/client/v3/account/whoami", get(whoami))
        .with_state(m)
}

fn cfg(homeserver: &str, args: &[&str]) -> Config {
    let state = std::env::temp_dir().join(format!("auth-test-{}", uuid::Uuid::new_v4()));
    let state_str = state.to_str().unwrap().to_string();
    let mut argv = vec![
        "matrix-satori",
        "--homeserver",
        homeserver,
        "--state-dir",
        &state_str,
    ];
    argv.extend_from_slice(args);
    Config::parse_from(argv)
}

async fn spawn_mock(m: Arc<AuthMock>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = mock_router(m).await;
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://127.0.0.1:{}", port)
}

#[tokio::test]
async fn direct_access_token() {
    let hs = spawn_mock(Arc::new(AuthMock::default())).await;
    let c = cfg(&hs, &["--access-token", "syt_direct"]);
    let client = AuthClient::new(&c).unwrap();
    let creds = client.resolve().await.unwrap();
    assert_eq!(creds.access_token, "syt_direct");
    assert_eq!(creds.user_id, "@direct:mock");
    assert_eq!(creds.method, "access-token");
}

#[tokio::test]
async fn password_login() {
    let m = Arc::new(AuthMock {
        flows: vec!["m.login.password".into()],
        ..Default::default()
    });
    let hs = spawn_mock(m.clone()).await;
    let c = cfg(
        &hs,
        &[
            "--login-method",
            "password",
            "--username",
            "bot",
            "--password",
            "secret",
        ],
    );
    let client = AuthClient::new(&c).unwrap();
    let creds = client.resolve().await.unwrap();
    assert_eq!(creds.access_token, "syt_password_token");
    assert_eq!(creds.device_id.as_deref(), Some("PWDEV"));
    assert_eq!(creds.method, "password");
    assert_eq!(m.password_logins.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn token_login() {
    let m = Arc::new(AuthMock {
        flows: vec!["m.login.token".into()],
        ..Default::default()
    });
    let hs = spawn_mock(m.clone()).await;
    let c = cfg(
        &hs,
        &["--login-method", "token", "--login-token", "LOGINTOK"],
    );
    let client = AuthClient::new(&c).unwrap();
    let creds = client.resolve().await.unwrap();
    assert_eq!(creds.access_token, "syt_from_LOGINTOK");
    assert_eq!(creds.user_id, "@ssobot:mock");
    assert_eq!(creds.method, "token");
    assert_eq!(m.token_logins.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn auto_picks_password_when_given() {
    let m = Arc::new(AuthMock {
        flows: vec!["m.login.sso".into(), "m.login.password".into()],
        ..Default::default()
    });
    let hs = spawn_mock(m.clone()).await;
    let c = cfg(&hs, &["--username", "bot", "--password", "pw"]);
    let client = AuthClient::new(&c).unwrap();
    let creds = client.resolve().await.unwrap();
    assert_eq!(creds.method, "password");
    assert_eq!(m.password_logins.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn discover_flows_from_server() {
    let m = Arc::new(AuthMock {
        flows: vec![
            "m.login.password".into(),
            "m.login.token".into(),
            "m.login.sso".into(),
        ],
        ..Default::default()
    });
    let hs = spawn_mock(m).await;
    let c = cfg(&hs, &[]);
    let client = AuthClient::new(&c).unwrap();
    let flows = client.flows().await.unwrap();
    assert_eq!(flows.len(), 3);
    assert!(flows.contains(&"m.login.password".to_string()));
    assert!(flows.contains(&"m.login.sso".to_string()));
}

#[tokio::test]
async fn application_service_login() {
    // Appservice login uses the AS token as bearer; our mock accepts any bearer
    // and returns a login response.
    let m = Arc::new(AuthMock::default());
    let hs = spawn_mock(m.clone()).await;
    let c = cfg(
        &hs,
        &[
            "--login-method",
            "applicationservice",
            "--as-token",
            "as_token_123",
            "--user-id",
            "@appservice:mock",
        ],
    );
    assert_eq!(c.login_method, LoginMethod::ApplicationService);
    let client = AuthClient::new(&c).unwrap();
    let creds = client.resolve().await.unwrap();
    // mock returns password-style response for unknown flows; method label is what matters
    assert_eq!(creds.method, "application-service");
    assert!(!creds.access_token.is_empty());
}

#[tokio::test]
async fn credentials_cache_roundtrip() {
    let dir = std::env::temp_dir().join(format!("cache-test-{}", uuid::Uuid::new_v4()));
    let c = Config::parse_from([
        "matrix-satori",
        "--homeserver",
        "https://matrix.example.com",
        "--state-dir",
        dir.to_str().unwrap(),
    ]);
    let creds = matrix_satori::auth::Credentials {
        access_token: "syt_cached".into(),
        user_id: "@cached:example.com".into(),
        device_id: None,
        method: "password",
    };
    matrix_satori::auth::save_credentials(&c, &creds).unwrap();
    let loaded = matrix_satori::auth::load_credentials(&c).unwrap();
    assert_eq!(loaded.access_token, "syt_cached");
    assert_eq!(loaded.user_id, "@cached:example.com");

    // Different homeserver must not reuse the token.
    let c2 = Config::parse_from([
        "matrix-satori",
        "--homeserver",
        "https://other.example.com",
        "--state-dir",
        dir.to_str().unwrap(),
    ]);
    assert!(matrix_satori::auth::load_credentials(&c2).is_none());
}
