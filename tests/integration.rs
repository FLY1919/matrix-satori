//! End-to-end integration tests with a mock Matrix homeserver.
//!
//! Verifies the two flows Koishi actually uses:
//!   1. WebSocket gateway handshake (READY) + event push (EVENT) + PING/PONG
//!   2. HTTP API `message.create` -> Matrix /send call

use axum::extract::State;
use axum::routing::{get, put};
use axum::{Json, Router};
use matrix_satori::config::Config;
use matrix_satori::{build_router, init_bridge};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Default)]
struct MockState {
    /// sync responses to hand out, in order; last one repeats
    syncs: Vec<Value>,
    sync_index: AtomicUsize,
    sent: Mutex<Vec<Value>>,
}

async fn whoami() -> Json<Value> {
    Json(json!({"user_id": "@bot:example.com"}))
}

async fn sync(State(state): State<Arc<MockState>>) -> Json<Value> {
    let idx = state.sync_index.fetch_add(1, Ordering::SeqCst);
    let i = idx.min(state.syncs.len().saturating_sub(1));
    Json(
        state
            .syncs
            .get(i)
            .cloned()
            .unwrap_or_else(|| json!({"next_batch": "s0", "rooms": {"join": {}}})),
    )
}

async fn send(
    State(state): State<Arc<MockState>>,
    axum::extract::Path((room, _txn)): axum::extract::Path<(String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut sent = state.sent.lock().await;
    sent.push(json!({"room": room, "content": body}));
    Json(json!({"event_id": format!("$sent{}", sent.len())}))
}

async fn joined_rooms() -> Json<Value> {
    Json(json!({"joined_rooms": ["!room:example.com"]}))
}

async fn room_name() -> Json<Value> {
    Json(json!({"name": "测试房间"}))
}

async fn account_data() -> Json<Value> {
    Json(json!({}))
}

async fn mock_router(state: Arc<MockState>) -> Router {
    Router::new()
        .route("/_matrix/client/v3/account/whoami", get(whoami))
        .route("/_matrix/client/v3/sync", get(sync))
        .route(
            "/_matrix/client/v3/rooms/:room/send/m.room.message/:txn",
            put(send),
        )
        .route("/_matrix/client/v3/joined_rooms", get(joined_rooms))
        .route(
            "/_matrix/client/v3/rooms/:room/state/m.room.name",
            get(room_name),
        )
        .route("/_matrix/client/v3/account_data", get(account_data))
        .with_state(state)
}

fn test_config(homeserver: String, bind: String) -> Config {
    use clap::Parser;
    let state = std::env::temp_dir().join(format!("ms-test-{}", uuid::Uuid::new_v4()));
    Config::parse_from([
        "matrix-satori",
        "--homeserver",
        &homeserver,
        "--access-token",
        "test-token",
        "--bind",
        &bind,
        "--satori-token",
        "satori-secret",
        "--sync-timeout",
        "50",
        "--state-dir",
        state.to_str().unwrap(),
        "--log",
        "error",
    ])
}

async fn free_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").await.unwrap()
}

#[tokio::test]
async fn ws_ready_and_event_push_and_ping_pong() {
    // Matrix sync: first returns a room message, then keeps returning empty.
    let msg_event = json!({
        "type": "m.room.message",
        "event_id": "$evt1",
        "sender": "@alice:example.com",
        "origin_server_ts": 1700000000000i64,
        "content": {"msgtype": "m.text", "body": "hello from matrix"}
    });
    let mock = Arc::new(MockState {
        syncs: vec![
            json!({
                "next_batch": "s1",
                "rooms": {"join": {"!room:example.com": {
                    "timeline": {"events": [msg_event]}
                }}}
            }),
            json!({"next_batch": "s2", "rooms": {"join": {}}}),
        ],
        ..Default::default()
    });

    let mock_listener = free_listener().await;
    let mock_port = mock_listener.local_addr().unwrap().port();
    let mrouter = mock_router(mock.clone()).await;
    tokio::spawn(async move { axum::serve(mock_listener, mrouter).await.unwrap() });

    let satori_listener = free_listener().await;
    let satori_port = satori_listener.local_addr().unwrap().port();
    let cfg = test_config(
        format!("http://127.0.0.1:{}", mock_port),
        format!("127.0.0.1:{}", satori_port),
    );
    let (bridge, _cfg) = init_bridge(cfg).await.unwrap();
    let router = build_router(bridge.clone(), "");
    tokio::spawn(async move { axum::serve(satori_listener, router).await.unwrap() });
    tokio::spawn(async move { bridge.run_sync().await });

    // Connect as the Koishi adapter does (Authorization header on the upgrade request).
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri = format!("ws://127.0.0.1:{}/v1/events", satori_port);
    let mut req = IntoClientRequest::into_client_request(&uri).unwrap();
    req.headers_mut()
        .insert("Authorization", "Bearer satori-secret".parse().unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    use futures::SinkExt;
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    // IDENTIFY
    ws.send(Message::Text(
        json!({"op": 3, "body": {"token": "satori-secret", "sn": 0}}).to_string(),
    ))
    .await
    .unwrap();

    let mut got_ready = false;
    let mut got_message = false;
    for _ in 0..40 {
        let msg = tokio::time::timeout(std::time::Duration::from_millis(500), ws.next())
            .await
            .ok()
            .flatten();
        let Some(Ok(Message::Text(text))) = msg else {
            continue;
        };
        let v: Value = serde_json::from_str(&text).unwrap();
        let op = v["op"].as_u64().unwrap();
        if op == 4 {
            // READY
            assert_eq!(v["body"]["logins"][0]["platform"], "matrix");
            assert_eq!(v["body"]["logins"][0]["user"]["id"], "@bot:example.com");
            got_ready = true;
        } else if op == 0 {
            let body = &v["body"];
            if body["type"] == "message-created"
                && body["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .contains("hello from matrix")
            {
                assert_eq!(body["user"]["id"], "@alice:example.com");
                assert_eq!(body["channel"]["id"], "!room:example.com");
                got_message = true;
            }
        }
        if got_ready && got_message {
            break;
        }
    }
    assert!(got_ready, "never received READY");
    assert!(got_message, "never received the message-created event");

    // PING -> PONG
    ws.send(Message::Text(json!({"op": 1, "body": {}}).to_string()))
        .await
        .unwrap();
    let mut got_pong = false;
    for _ in 0..10 {
        if let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout(std::time::Duration::from_millis(300), ws.next()).await
        {
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["op"] == 2 {
                got_pong = true;
                break;
            }
        }
    }
    assert!(got_pong, "PING did not get PONG");
}

#[tokio::test]
async fn http_message_create_calls_matrix_send() {
    let mock = Arc::new(MockState {
        syncs: vec![json!({"next_batch": "s0", "rooms": {"join": {}}})],
        ..Default::default()
    });
    let mock_listener = free_listener().await;
    let mock_port = mock_listener.local_addr().unwrap().port();
    let mrouter = mock_router(mock.clone()).await;
    tokio::spawn(async move { axum::serve(mock_listener, mrouter).await.unwrap() });

    let satori_listener = free_listener().await;
    let satori_port = satori_listener.local_addr().unwrap().port();
    let cfg = test_config(
        format!("http://127.0.0.1:{}", mock_port),
        format!("127.0.0.1:{}", satori_port),
    );
    let (bridge, _cfg) = init_bridge(cfg).await.unwrap();
    let router = build_router(bridge.clone(), "");
    tokio::spawn(async move { axum::serve(satori_listener, router).await.unwrap() });

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/message.create", satori_port);
    let resp = client
        .post(&url)
        .bearer_auth("satori-secret")
        .header("Satori-Platform", "matrix")
        .header("Satori-User-ID", "@bot:example.com")
        .json(&json!({
            "channel_id": "!room:example.com",
            "content": "hi <at id=\"@alice:example.com\" name=\"Alice\"/>"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "body: {}",
        resp.text().await.unwrap_or_default()
    );
    // We consumed body above only on failure; re-fetch via second call to inspect result.
    let resp = client
        .post(&url)
        .bearer_auth("satori-secret")
        .json(&json!({"channel_id": "!room:example.com", "content": "second"}))
        .send()
        .await
        .unwrap();
    let result: Value = resp.json().await.unwrap();
    assert!(
        result.is_array(),
        "message.create must return an array: {:?}",
        result
    );
    assert!(
        result[0]["id"].as_str().unwrap().starts_with("$sent"),
        "{:?}",
        result
    );

    let sent = mock.sent.lock().await;
    assert_eq!(sent.len(), 2, "Matrix /send should have been called twice");
    let first = &sent[0]["content"];
    assert_eq!(first["msgtype"], "m.text");
    assert!(
        first["body"].as_str().unwrap().contains("hi Alice"),
        "{:?}",
        first
    );
    assert!(first["formatted_body"]
        .as_str()
        .unwrap()
        .contains("matrix.to/#/@alice:example.com"));
}

#[tokio::test]
async fn http_unauthorized_without_token() {
    let mock = Arc::new(MockState::default());
    let mock_listener = free_listener().await;
    let mock_port = mock_listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_router(mock).await)
            .await
            .unwrap()
    });

    let satori_listener = free_listener().await;
    let satori_port = satori_listener.local_addr().unwrap().port();
    let cfg = test_config(
        format!("http://127.0.0.1:{}", mock_port),
        format!("127.0.0.1:{}", satori_port),
    );
    let (bridge, _cfg) = init_bridge(cfg).await.unwrap();
    let router = build_router(bridge, "");
    tokio::spawn(async move { axum::serve(satori_listener, router).await.unwrap() });

    // wait for server to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/v1/login.get", satori_port))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = client
        .get(format!("http://127.0.0.1:{}/v1/login.get", satori_port))
        .bearer_auth("satori-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["adapter"], "matrix");
}
