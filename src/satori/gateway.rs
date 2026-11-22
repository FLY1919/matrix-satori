//! Satori WebSocket gateway: `/v1/events`.
//!
//! Flow observed from `@satorijs/adapter-satori` 1.5.x:
//!   - client connects with `Authorization: Bearer <token>`
//!   - client sends IDENTIFY { op:3, body:{ token, sn } }
//!   - server replies READY { op:4, body:{ logins, proxy_urls } }
//!   - client pings every 10s { op:1 } -> server PONG { op:2 }
//!   - server pushes EVENT { op:0, body: Event }

use crate::bridge::Bridge;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::{Opcode, Payload};

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    if token.is_empty() {
        return true;
    }
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim() == token)
        .unwrap_or(false)
}

pub async fn ws_handler(
    State(bridge): State<Arc<Bridge>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if !authorized(&headers, &bridge.config.satori_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(|socket| handle_socket(socket, bridge))
}

async fn handle_socket(socket: WebSocket, bridge: Arc<Bridge>) {
    info!("satori client connected");
    let (mut sender, mut receiver) = socket.split();
    let mut events = bridge.subscribe();

    // Handshake follows the official @satorijs/adapter-satori flow exactly:
    // the client sends IDENTIFY (op=3) first, and only then do we reply with
    // READY (op=4). We do NOT push READY eagerly on connect — some adapter
    // versions ignore an out-of-order READY and never create the bot.
    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(body) => {
                        let payload = Payload::new(Opcode::Event, body);
                        match serde_json::to_string(&payload) {
                            Ok(text) => {
                                if sender.send(WsMessage::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => warn!("serialize event failed: {}", e),
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("client lagged {} events", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(WsMessage::Text(text))) => {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(payload) => {
                                let op = payload.get("op").and_then(Value::as_u64).unwrap_or(255);
                                match op {
                                    x if x == Opcode::Ping as u64 => {
                                        let pong = Payload::new(Opcode::Pong, json!({}));
                                        if let Ok(t) = serde_json::to_string(&pong) {
                                            let _ = sender.send(WsMessage::Text(t)).await;
                                        }
                                    }
                                    x if x == Opcode::Identify as u64 => {
                                        debug!("IDENTIFY from client: {}", text);
                                        // Reply with READY carrying the current logins.
                                        let ready = Payload::new(
                                            Opcode::Ready,
                                            json!({ "logins": [bridge.login], "proxy_urls": [] }),
                                        );
                                        if let Ok(t) = serde_json::to_string(&ready) {
                                            let _ = sender.send(WsMessage::Text(t)).await;
                                        }
                                    }
                                    _ => debug!("unhandled WS op {}", op),
                                }
                            }
                            Err(e) => warn!("invalid WS payload: {}", e),
                        }
                    }
                    Some(Ok(WsMessage::Ping(data))) => {
                        let _ = sender.send(WsMessage::Pong(data)).await;
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(e)) => {
                        warn!("WS error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    info!("satori client disconnected");
}
