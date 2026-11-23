//! End-to-end integration tests with a mock Matrix homeserver.
//!
//! Verifies the two flows Koishi actually uses:
//!   1. WebSocket gateway handshake (READY) + event push (EVENT) + PING/PONG
//!   2. HTTP API `message.create` -> Matrix /send call

use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use matrix_satori::bridge::Bridge;
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
    redacted: Mutex<Vec<String>>,
    /// (device_id, display_name) pairs from PUT /devices/{id}
    device_names: Mutex<Vec<(String, String)>>,
    /// number of media upload calls (remote media auto-upload test)
    upload_count: AtomicUsize,
    /// number of times the remote picture endpoint was fetched
    remote_count: AtomicUsize,
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

async fn relations() -> Json<Value> {
    Json(json!({"chunk": [
        {"type": "m.reaction", "event_id": "$r1", "sender": "@alice:example.com",
         "content": {"m.relates_to": {"rel_type": "m.annotation", "event_id": "$evt1", "key": "👍"}}},
        {"type": "m.reaction", "event_id": "$r2", "sender": "@bob:example.com",
         "content": {"m.relates_to": {"rel_type": "m.annotation", "event_id": "$evt1", "key": "👍"}}}
    ]}))
}

async fn redact(
    State(state): State<Arc<MockState>>,
    axum::extract::Path((_room, event, _txn)): axum::extract::Path<(String, String, String)>,
) -> Json<Value> {
    state.redacted.lock().await.push(event);
    Json(json!({"event_id": "$redaction"}))
}

async fn create_room() -> Json<Value> {
    Json(json!({"room_id": "!new:example.com"}))
}

/// A quoted/related event for the reply-quote integration test.
async fn get_event(
    axum::extract::Path((_room, event)): axum::extract::Path<(String, String)>,
) -> Json<Value> {
    Json(json!({
        "type": "m.room.message",
        "event_id": event,
        "sender": "@alice:example.com",
        "origin_server_ts": 1699999999000i64,
        "content": {"msgtype": "m.text", "body": "original quoted text"}
    }))
}

async fn empty_post() -> Json<Value> {
    Json(json!({}))
}

async fn upload_media(State(state): State<Arc<MockState>>) -> Json<Value> {
    state.upload_count.fetch_add(1, Ordering::SeqCst);
    Json(json!({"content_uri": "mxc://example.com/UPLOAD1"}))
}

/// A fake remote image for the auto-upload test (download_url fetches this).
async fn remote_pic(State(state): State<Arc<MockState>>) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    state.remote_count.fetch_add(1, Ordering::SeqCst);
    let mut resp = (b"\x89PNG\r\n\x1a\nFAKE".to_vec()).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("image/png"),
    );
    resp
}

async fn set_device_name(
    State(state): State<Arc<MockState>>,
    axum::extract::Path(device_id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let name = body
        .get("display_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    state.device_names.lock().await.push((device_id, name));
    Json(json!({}))
}

async fn mock_router(state: Arc<MockState>) -> Router {
    Router::new()
        .route("/_matrix/client/v3/account/whoami", get(whoami))
        .route("/_matrix/client/v3/devices/:device", put(set_device_name))
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
        .route(
            "/_matrix/client/v3/rooms/:room/event/:event",
            get(get_event),
        )
        .route(
            "/_matrix/client/v1/rooms/:room/relations/:event/m.annotation/m.reaction",
            get(relations),
        )
        .route(
            "/_matrix/client/v3/rooms/:room/redact/:event/:txn",
            put(redact),
        )
        .route("/_matrix/client/v3/createRoom", post(create_room))
        .route("/_matrix/client/v3/rooms/:room/leave", post(empty_post))
        .route("/_matrix/client/v3/rooms/:room/kick", post(empty_post))
        .route("/_matrix/client/v3/rooms/:room/invite", post(empty_post))
        .route("/_matrix/client/v3/rooms/:room/join", post(empty_post))
        .route(
            "/_matrix/client/v3/rooms/:room/state/m.room.name",
            put(empty_post),
        )
        .route("/_matrix/media/v3/upload", post(upload_media))
        .route("/remote/pic.png", get(remote_pic))
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
    assert_eq!(v["adapter"], "satori");
    assert_eq!(v["platform"], "matrix");
}

async fn spawn_server<F>(mock: Arc<MockState>, build: F) -> (u16, u16)
where
    F: FnOnce(Arc<Bridge>, u16) -> Router + Send + 'static,
{
    let ml = free_listener().await;
    let mp = ml.local_addr().unwrap().port();
    let mr = mock_router(mock.clone()).await;
    tokio::spawn(async move { axum::serve(ml, mr).await.unwrap() });

    let sl = free_listener().await;
    let sp = sl.local_addr().unwrap().port();
    let cfg = test_config(
        format!("http://127.0.0.1:{}", mp),
        format!("127.0.0.1:{}", sp),
    );
    let (bridge, _c) = init_bridge(cfg).await.unwrap();
    let router = build(bridge, sp);
    tokio::spawn(async move { axum::serve(sl, router).await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    (mp, sp)
}

/// A mixed Satori message (text + several media) must be sent as separate
/// Matrix events so no part is dropped. Previously only the last media
/// survived and the rest was silently lost.
#[tokio::test]
async fn message_create_splits_mixed_media_into_separate_events() {
    let mock = Arc::new(MockState {
        syncs: vec![json!({"next_batch": "s0", "rooms": {"join": {}}})],
        ..Default::default()
    });
    let (_mp, sp) = spawn_server(mock.clone(), |b, _| build_router(b, "")).await;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/message.create", sp);

    let content = r#"文字开头<img src="mxc://s/a" title="A"/>中间文字<audio src="mxc://s/b"/>结尾"#;
    let resp = client
        .post(&url)
        .bearer_auth("satori-secret")
        .json(&json!({"channel_id": "!room:example.com", "content": content}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "body: {:?}", resp.text().await);
    let result: Value = resp.json().await.unwrap();
    // One returned message id per emitted Matrix event.
    assert!(
        result.as_array().unwrap().len() >= 5,
        "expected one id per segment: {:?}",
        result
    );

    let sent = mock.sent.lock().await;
    let mts: Vec<&str> = sent
        .iter()
        .map(|v| v["content"]["msgtype"].as_str().unwrap_or(""))
        .collect();
    assert!(
        mts.contains(&"m.image") && mts.contains(&"m.audio") && mts.contains(&"m.text"),
        "all message types present: {:?}",
        mts
    );
    // The image and audio must be separate events, not merged into one.
    let image = sent
        .iter()
        .find(|v| v["content"]["msgtype"] == "m.image")
        .unwrap();
    assert_eq!(image["content"]["url"], "mxc://s/a");
    let audio = sent
        .iter()
        .find(|v| v["content"]["msgtype"] == "m.audio")
        .unwrap();
    assert_eq!(audio["content"]["url"], "mxc://s/b");
}

/// Even when a remote media download fails (anti-hotlink), the event must keep
/// its media msgtype and original URL instead of degrading to a text link.
#[tokio::test]
async fn message_create_keeps_media_type_on_download_failure() {
    let mock = Arc::new(MockState {
        syncs: vec![json!({"next_batch": "s0", "rooms": {"join": {}}})],
        ..Default::default()
    });
    let (_mp, sp) = spawn_server(mock.clone(), |b, _| build_router(b, "")).await;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/message.create", sp);

    let content = r#"<audio src="https://m701.music.126.net/song.mp3?vuutv=x"/>"#;
    let resp = client
        .post(&url)
        .bearer_auth("satori-secret")
        .json(&json!({"channel_id": "!room:example.com", "content": content}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "body: {:?}", resp.text().await);

    let sent = mock.sent.lock().await;
    assert_eq!(sent.len(), 1);
    let c = &sent[0]["content"];
    assert_eq!(c["msgtype"], "m.audio", "media type kept: {:?}", c);
    assert_eq!(c["url"], "https://m701.music.126.net/song.mp3?vuutv=x");
}

#[tokio::test]
async fn reaction_list_returns_users_and_delete_redacts() {
    let mock = Arc::new(MockState::default());
    let (_mp, sp) = spawn_server(mock.clone(), |b, _| build_router(b, "")).await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", sp);

    // getReactionList -> List<User> of users who used 👍
    let resp = client
        .post(format!("{}/v1/reaction.list", base))
        .bearer_auth("satori-secret")
        .json(&json!({"channel_id": "!room:example.com", "message_id": "$evt1", "emoji_id": "👍"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert_eq!(v["data"][0]["id"], "@alice:example.com");

    // deleteReaction(key=👍, user=@alice) -> redact only $r1
    let resp = client
        .post(format!("{}/v1/reaction.delete", base))
        .bearer_auth("satori-secret")
        .json(
            &json!({"channel_id": "!room:example.com", "message_id": "$evt1",
                      "emoji_id": "👍", "user_id": "@alice:example.com"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let redacted = mock.redacted.lock().await;
    assert_eq!(redacted.len(), 1);
    assert_eq!(redacted[0], "$r1");
}

#[tokio::test]
async fn channel_and_upload_methods_are_implemented() {
    use matrix_satori::satori::Channel;
    let mock = Arc::new(MockState::default());
    let (_mp, sp) = spawn_server(mock.clone(), |b, _| build_router(b, "")).await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", sp);

    // channel.create -> Matrix createRoom
    let resp = client
        .post(format!("{}/v1/channel.create", base))
        .bearer_auth("satori-secret")
        .json(&json!({"guild_id": "", "data": {"name": "新房间"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "body: {}",
        resp.text().await.unwrap_or_default()
    );
    let ch: Channel = client
        .post(format!("{}/v1/channel.create", base))
        .bearer_auth("satori-secret")
        .json(&json!({"guild_id": "", "data": {"name": "新房间"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ch.id, "!new:example.com");

    // upload.create -> multipart -> mxc -> http url
    let part = reqwest::multipart::Part::bytes(b"hello".to_vec())
        .file_name("a.txt")
        .mime_str("text/plain")
        .unwrap();
    let form = reqwest::multipart::Form::new().part("file", part);
    let resp = client
        .post(format!("{}/v1/upload.create", base))
        .bearer_auth("satori-secret")
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "body: {}",
        resp.text().await.unwrap_or_default()
    );
}

#[tokio::test]
async fn no_registered_method_returns_404() {
    // Every method the adapter may call must be registered and return a
    // protocol-shaped response (2xx), never "unsupported method".
    let mock = Arc::new(MockState::default());
    let (_mp, sp) = spawn_server(mock, |b, _| build_router(b, "")).await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", sp);
    let methods = [
        "channel.get",
        "channel.list",
        "channel.create",
        "channel.update",
        "channel.delete",
        "channel.mute",
        "login.get",
        "message.update",
        "message.delete",
        "message.get",
        "message.list",
        "reaction.create",
        "reaction.delete",
        "reaction.clear",
        "reaction.list",
        "guild.get",
        "guild.list",
        "guild.member.get",
        "guild.member.list",
        "guild.member.mute",
        "guild.member.role.set",
        "guild.member.role.unset",
        "guild.member.role.list",
        "guild.role.list",
        "guild.role.create",
        "guild.role.update",
        "guild.role.delete",
        "user.get",
        "friend.list",
        "friend.delete",
        "friend.approve",
        "guild.approve",
        "guild.member.approve",
        "guild.member.kick",
        "user.channel.create",
    ];
    for m in methods {
        let resp = client
            .post(format!("{}/v1/{}", base, m))
            .bearer_auth("satori-secret")
            .json(
                &json!({"channel_id":"!room:example.com","guild_id":"!room:example.com",
                          "user_id":"@u:example.com","message_id":"$e","emoji_id":"x"}),
            )
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert_ne!(status, 404, "method {} returned 404: {}", m, body);
    }
}

/// End-to-end: a Satori message containing every standard element must reach
/// Matrix /send intact (send direction). We send one element at a time so each
/// becomes its own m.room.message and can be inspected independently.
#[tokio::test]
async fn message_create_all_elements_reach_matrix() {
    let mock = Arc::new(MockState {
        syncs: vec![json!({"next_batch": "s0", "rooms": {"join": {}}})],
        ..Default::default()
    });
    let (mp, sp) = spawn_server(mock.clone(), |b, _| build_router(b, "")).await;
    let _ = mp;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/message.create", sp);

    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("plain text", json!({"msgtype": "m.text"})),
        (
            r#"<at id="@u:s" name="Bob"/>"#,
            json!({"expect_body": "Bob", "link": "https://matrix.to/#/@u:s"}),
        ),
        (
            r#"<a href="https://example.com">site</a>"#,
            json!({"expect_body": "site", "link": "https://example.com"}),
        ),
        (r#"<b>bold</b>"#, json!({"html": "<b>bold</b>"})),
        (
            r#"<i>it</i><u>ul</u><s>del</s><code>c</code>"#,
            json!({"html_all": ["<i>it</i>", "<u>ul</u>", "<del>del</del>", "<code>c</code>"]}),
        ),
        (
            r#"<img src="mxc://s/i" width="100"/>"#,
            json!({"msgtype": "m.image", "url": "mxc://s/i", "w": 100}),
        ),
        (
            r#"<audio src="mxc://s/a" duration="12"/>"#,
            json!({"msgtype": "m.audio", "url": "mxc://s/a", "duration": 12}),
        ),
        (
            r#"<video src="mxc://s/v" width="640"/>"#,
            json!({"msgtype": "m.video", "url": "mxc://s/v", "w": 640}),
        ),
        (
            r#"<file src="mxc://s/f" title="doc.pdf"/>"#,
            json!({"msgtype": "m.file", "url": "mxc://s/f", "body": "doc.pdf"}),
        ),
        (
            r#"<quote id="$e1">quoted</quote>"#,
            json!({"reply": "$e1", "body": "quoted"}),
        ),
    ];

    for (idx, (content, _)) in cases.iter().enumerate() {
        let resp = client
            .post(&url)
            .bearer_auth("satori-secret")
            .header("Satori-Platform", "matrix")
            .header("Satori-User-ID", "@bot:example.com")
            .json(&json!({"channel_id": "!room:example.com", "content": content}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "case {}: {}", idx, content);
    }

    let sent = mock.sent.lock().await;
    assert_eq!(sent.len(), cases.len(), "one /send per element group");

    for (idx, (_, exp)) in cases.iter().enumerate() {
        let c = &sent[idx]["content"];
        if let Some(mt) = exp.get("msgtype").and_then(Value::as_str) {
            assert_eq!(c["msgtype"], mt, "case {} msgtype", idx);
        }
        if let Some(b) = exp
            .get("expect_body")
            .or_else(|| exp.get("body"))
            .and_then(Value::as_str)
        {
            assert!(
                c["body"].as_str().unwrap_or("").contains(b),
                "case {} body: {}",
                idx,
                c
            );
        }
        if let Some(link) = exp.get("link").and_then(Value::as_str) {
            let html = c["formatted_body"].as_str().unwrap_or("");
            assert!(
                html.contains(link),
                "case {} link {} missing in {}",
                idx,
                link,
                html
            );
        }
        if let Some(h) = exp.get("html").and_then(Value::as_str) {
            assert_eq!(
                c["formatted_body"].as_str().unwrap(),
                h,
                "case {} html",
                idx
            );
        }
        if let Some(list) = exp.get("html_all").and_then(Value::as_array) {
            let html = c["formatted_body"].as_str().unwrap_or("");
            for v in list {
                assert!(
                    html.contains(v.as_str().unwrap()),
                    "case {} missing {} in {}",
                    idx,
                    v,
                    html
                );
            }
        }
        if let Some(u) = exp.get("url").and_then(Value::as_str) {
            assert_eq!(c["url"], u, "case {} url", idx);
        }
        if let Some(w) = exp.get("w").and_then(Value::as_i64) {
            assert_eq!(c["info"]["w"], w, "case {} width", idx);
        }
        if let Some(d) = exp.get("duration").and_then(Value::as_i64) {
            assert_eq!(c["info"]["duration"], d, "case {} duration", idx);
        }
        if let Some(r) = exp.get("reply").and_then(Value::as_str) {
            assert_eq!(
                c["m.relates_to"]["m.in_reply_to"]["event_id"], r,
                "case {} reply",
                idx
            );
        }
    }
}

/// End-to-end: a Matrix formatted_body containing all decoration tags must
/// arrive over the WebSocket as valid Satori elements.
#[tokio::test]
async fn sync_all_elements_arrive_over_ws_as_satori() {
    let formatted = "hi <a href=\"https://matrix.to/#/@u:s\">Bob</a> \
<b>bold</b> <i>it</i> <u>ul</u> <del>del</del> <code>c</code> \
<span data-mx-spoiler>secret</span>";
    let msg_event = json!({
        "type": "m.room.message",
        "event_id": "$all1",
        "sender": "@alice:example.com",
        "origin_server_ts": 1700000000000i64,
        "content": {
            "msgtype": "m.text", "body": "hi Bob bold it ul del c secret",
            "format": "org.matrix.custom.html", "formatted_body": formatted
        }
    });
    let mock = Arc::new(MockState {
        syncs: vec![
            json!({"next_batch": "s1", "rooms": {"join": {"!room:example.com": {
                "timeline": {"events": [msg_event]}
            }}}}),
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

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri = format!("ws://127.0.0.1:{}/v1/events", satori_port);
    let mut req = IntoClientRequest::into_client_request(&uri).unwrap();
    req.headers_mut()
        .insert("Authorization", "Bearer satori-secret".parse().unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    ws.send(Message::Text(
        json!({"op": 3, "body": {"token": "satori-secret", "sn": 0}}).to_string(),
    ))
    .await
    .unwrap();

    let mut content = String::new();
    for _ in 0..60 {
        let msg = tokio::time::timeout(std::time::Duration::from_millis(300), ws.next())
            .await
            .ok()
            .flatten();
        let Some(Ok(Message::Text(text))) = msg else {
            continue;
        };
        let v: Value = serde_json::from_str(&text).unwrap();
        if v["op"] == 0
            && v["body"]["type"] == "message-created"
            && v["body"]["message"]["id"] == "$all1"
        {
            content = v["body"]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            break;
        }
    }
    assert!(!content.is_empty(), "never received message $all1 over WS");
    for el in [
        r#"<at id="@u:s" name="Bob"/>"#,
        "<b>bold</b>",
        "<i>it</i>",
        "<u>ul</u>",
        "<del>del</del>",
        "<code>c</code>",
        "<spl>secret</spl>",
    ] {
        assert!(
            content.contains(el),
            "Satori content missing `{}`: {}",
            el,
            content
        );
    }
}

/// A Matrix reply must reach Koishi as a leading `<quote>` element inside
/// `message.content` (NOT only as a standalone `message.quote` field, which the
/// official adapter clears when it assigns session.content). This is what
/// makes `session.quote` readable inside plugins.
#[tokio::test]
async fn sync_reply_reaches_koishi_as_quote_element() {
    let reply_event = json!({
        "type": "m.room.message",
        "event_id": "$reply1",
        "sender": "@bob:example.com",
        "origin_server_ts": 1700000001000i64,
        "content": {
            "msgtype": "m.text",
            "body": "> <@alice:example.com> original quoted text\n\nmy reply",
            "m.relates_to": {"m.in_reply_to": {"event_id": "$orig"}}
        }
    });
    let mock = Arc::new(MockState {
        syncs: vec![
            json!({
                "next_batch": "s1",
                "rooms": {"join": {"!room:example.com": {
                    "timeline": {"events": [reply_event]}
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

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri = format!("ws://127.0.0.1:{}/v1/events", satori_port);
    let mut req = IntoClientRequest::into_client_request(&uri).unwrap();
    req.headers_mut()
        .insert("Authorization", "Bearer satori-secret".parse().unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    ws.send(Message::Text(
        json!({"op": 3, "body": {"token": "satori-secret", "sn": 0}}).to_string(),
    ))
    .await
    .unwrap();

    let mut content = String::new();
    for _ in 0..60 {
        let msg = tokio::time::timeout(std::time::Duration::from_millis(300), ws.next())
            .await
            .ok()
            .flatten();
        let Some(Ok(Message::Text(text))) = msg else {
            continue;
        };
        let v: Value = serde_json::from_str(&text).unwrap();
        if v["op"] == 0
            && v["body"]["type"] == "message-created"
            && v["body"]["message"]["id"] == "$reply1"
        {
            content = v["body"]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            break;
        }
    }
    assert!(!content.is_empty(), "never received reply $reply1 over WS");
    assert!(
        content.starts_with("<quote "),
        "content must start with a <quote> element: {}",
        content
    );
    assert!(
        content.contains("id=\"$orig\""),
        "quote must carry the referenced event id: {}",
        content
    );
    assert!(
        content.contains("original quoted text"),
        "quote must carry the referenced message content: {}",
        content
    );
    assert!(
        content.contains("<user id=\"@alice:example.com\""),
        "quote must carry the referenced sender: {}",
        content
    );
    assert!(
        content.contains("my reply"),
        "original reply text must follow the quote: {}",
        content
    );
}

#[tokio::test]
async fn device_display_name_is_applied_via_api() {
    let mock = Arc::new(MockState::default());
    let mock_listener = free_listener().await;
    let mock_port = mock_listener.local_addr().unwrap().port();
    let mrouter = mock_router(mock.clone()).await;
    tokio::spawn(async move { axum::serve(mock_listener, mrouter).await.unwrap() });

    let mut cfg = test_config(
        format!("http://127.0.0.1:{}", mock_port),
        "127.0.0.1:0".into(),
    );
    cfg.device_display_name = "my-satori-bot".into();
    matrix_satori::apply_device_display_name(&cfg, Some("DEV123")).await;

    let names = mock.device_names.lock().await;
    assert_eq!(names.len(), 1, "expected exactly one PUT /devices call");
    assert_eq!(names[0].0, "DEV123");
    assert_eq!(names[0].1, "my-satori-bot");
}

#[tokio::test]
async fn device_display_name_skipped_without_device_or_when_empty() {
    let mock = Arc::new(MockState::default());
    let mock_listener = free_listener().await;
    let mock_port = mock_listener.local_addr().unwrap().port();
    let mrouter = mock_router(mock.clone()).await;
    tokio::spawn(async move { axum::serve(mock_listener, mrouter).await.unwrap() });

    let mut cfg = test_config(
        format!("http://127.0.0.1:{}", mock_port),
        "127.0.0.1:0".into(),
    );
    // No device id -> skipped.
    matrix_satori::apply_device_display_name(&cfg, None).await;
    // Empty display name -> skipped.
    cfg.device_display_name.clear();
    matrix_satori::apply_device_display_name(&cfg, Some("DEV123")).await;

    assert!(
        mock.device_names.lock().await.is_empty(),
        "no PUT /devices call expected"
    );
}

#[tokio::test]
async fn ws_sends_ready_only_after_identify() {
    let mock = Arc::new(MockState {
        syncs: vec![json!({"next_batch": "s1", "rooms": {"join": {}}})],
        ..Default::default()
    });
    let (mock_port, satori_port) = spawn_server(mock, |bridge, _| build_router(bridge, "")).await;
    let _ = mock_port;
    tokio::spawn(async move {});

    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    let uri = format!("ws://127.0.0.1:{}/v1/events", satori_port);
    let mut req = IntoClientRequest::into_client_request(&uri).unwrap();
    req.headers_mut()
        .insert("Authorization", "Bearer satori-secret".parse().unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Before IDENTIFY the server must NOT send READY (op=4).
    let early = tokio::time::timeout(std::time::Duration::from_millis(300), ws.next()).await;
    if let Ok(Some(Ok(Message::Text(text)))) = early {
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_ne!(
            v["op"].as_u64(),
            Some(4),
            "READY must not be sent before IDENTIFY: {}",
            text
        );
    }

    // After IDENTIFY, READY must arrive.
    ws.send(Message::Text(
        json!({"op": 3, "body": {"token": "satori-secret", "sn": 0}}).to_string(),
    ))
    .await
    .unwrap();
    let mut got_ready = false;
    for _ in 0..20 {
        if let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout(std::time::Duration::from_millis(300), ws.next()).await
        {
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["op"].as_u64() == Some(4) {
                assert_eq!(v["body"]["logins"][0]["adapter"], "satori");
                assert_eq!(v["body"]["logins"][0]["platform"], "matrix");
                got_ready = true;
                break;
            }
        }
    }
    assert!(got_ready, "READY not received after IDENTIFY");
}

#[tokio::test]
async fn message_create_auto_uploads_remote_image() {
    let mock = Arc::new(MockState {
        syncs: vec![json!({"next_batch": "s0", "rooms": {"join": {}}})],
        ..Default::default()
    });
    let (mp, sp) = spawn_server(mock.clone(), |b, _| build_router(b, "")).await;

    let remote = format!("http://127.0.0.1:{}/remote/pic.png", mp);
    let content = format!(r#"<img src="{}" title="pic"/>"#, remote);
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/v1/message.create", sp);
    let resp = client
        .post(&url)
        .bearer_auth("satori-secret")
        .json(&json!({"channel_id": "!room:example.com", "content": content}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "body: {:?}", resp.text().await);

    // The remote URL must have been fetched and re-uploaded to the homeserver.
    assert!(
        mock.remote_count.load(Ordering::SeqCst) >= 1,
        "remote image was never downloaded"
    );
    assert!(
        mock.upload_count.load(Ordering::SeqCst) >= 1,
        "remote image was never uploaded to Matrix"
    );

    // And the actual /send payload must be a real m.image with the mxc URI.
    let sent = mock.sent.lock().await;
    let image = sent
        .iter()
        .find(|v| {
            v.get("content")
                .and_then(|c| c.get("msgtype"))
                .and_then(|m| m.as_str())
                == Some("m.image")
        })
        .expect("no m.image event was sent");
    let img = &image["content"];
    assert_eq!(img["url"], "mxc://example.com/UPLOAD1");
    assert_eq!(img["body"], "pic");
}
