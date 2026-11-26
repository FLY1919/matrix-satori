//! `matrix-satori verify` — drive Matrix device verification (SAS) from the
//! terminal.
//!
//! Why a CLI? Cross-signing our `satori` device from an already-verified device
//! needs a human to compare the short auth string (emoji/decimals). An AI
//! cannot click "verify" in someone's FluffyChat/Element, but the human can run
//! these commands themselves and compare the emoji printed here against the
//! ones on their other screen.
//!
//! IMPORTANT: SAS state lives in the SDK's crypto machine *in memory* for the
//! lifetime of a client. So the full flow runs in an interactive REPL that
//! keeps ONE client alive:
//!
//!   matrix-satori verify                 # no subcommand -> interactive REPL
//!   verify> devices
//!   verify> request --device <other-device-id>
//!   verify> start-sas --flow <flow-id>   # after the other device accepts
//!   verify> sas --flow <flow-id>         # compare these emoji with the other screen
//!   verify> confirm --flow <flow-id>
//!   verify> status                        # cross_signed_by_owner = true
//!
//! The REPL runs a background sync loop so to-device events (ready / accept /
//! key / mac) are consumed as they arrive, not only while a command is running.
//!
//! Single-shot subcommands (`matrix-satori verify status`, `... devices`) are
//! still accepted for stateless queries. Stateful SAS steps should use the REPL.

use crate::auth::Credentials;
use crate::config::{CliOverrides, Config};
use crate::matrix::e2ee;
use clap::{Parser, Subcommand};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::ruma::UserId;
use matrix_sdk::Client;
use serde_json::{json, Value};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "matrix-satori verify",
    about = "Drive Matrix device verification (SAS) from the command line"
)]
pub struct VerifyArgs {
    /// Run one command and exit. Omit to enter the interactive REPL (needed for
    /// the full SAS flow, whose state lives only for the client's lifetime).
    #[command(subcommand)]
    pub command: Option<VerifyCommand>,
}

/// Wrapper used to parse a single REPL input line (e.g. `sas --flow abc`).
#[derive(Parser, Debug)]
struct ReplLine {
    #[command(subcommand)]
    cmd: VerifyCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum VerifyCommand {
    /// Show our own device's verification / cross-signing state
    Status,
    /// List devices of a user (default: ourselves) with verification flags
    Devices {
        #[arg(long)]
        user_id: Option<String>,
    },
    /// List known in-flight verification requests
    List {
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Start a SAS verification request against one of our devices
    Request {
        /// Target device id
        #[arg(long)]
        device: String,
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Accept an incoming verification request
    Accept {
        #[arg(long)]
        flow: String,
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Start the SAS exchange for an accepted request
    StartSas {
        #[arg(long)]
        flow: String,
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Print the SAS short auth string (emoji + decimals) — compare with the other device
    Sas {
        #[arg(long)]
        flow: String,
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Confirm the emoji match (signs the other device, uploads signatures)
    Confirm {
        #[arg(long)]
        flow: String,
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Report that the emoji do NOT match (aborts the flow)
    Mismatch {
        #[arg(long)]
        flow: String,
        #[arg(long)]
        user_id: Option<String>,
    },
    /// Cancel an in-flight verification flow
    Cancel {
        #[arg(long)]
        flow: String,
        #[arg(long)]
        user_id: Option<String>,
    },
}

struct Session {
    config: Arc<Config>,
    client: Client,
    creds: Credentials,
}

/// One-shot setup identical to the server: load config (env/TOML), resolve
/// credentials (cache-aware), restore the SAME sqlite-backed SDK client so we
/// operate on the very same device and olm keys.
async fn build_session() -> anyhow::Result<Session> {
    // Connection settings come from env vars / <state-dir>/config.toml, NOT
    // from the sub-argv. Parse an empty argv so clap only reads `env` values.
    let overrides = CliOverrides::parse_from(["matrix-satori"]);
    let config = Config::load_from(&overrides)?;
    let (config, creds) = crate::resolve_config(config).await?;
    if !e2ee::e2ee_available(&creds) {
        anyhow::bail!(
            "verification needs a device-bound login (access token + user id + device id). \
             Supply --access-token/--user-id/--device-id via env or config.toml first."
        );
    }
    let client = e2ee::build_client(&config, &creds).await?;
    Ok(Session {
        config: Arc::new(config),
        client,
        creds,
    })
}

/// Drive the crypto machine with bounded single syncs so pending to-device
/// events (accept / start / ready / mac) are consumed and the SAS state
/// advances. Uses `sync_once` (one long-poll) — `sync()` is an endless stream.
async fn pump(client: &Client, rounds: usize) {
    for _ in 0..rounds {
        let settings = SyncSettings::default().timeout(Duration::from_secs(2));
        if let Err(e) = client.sync_once(settings).await {
            // A transient keys/upload 400 (a one-time key already uploaded by a
            // sibling session of this device) is harmless; keep going.
            tracing::debug!("verify pump sync_once: {}", e);
        }
    }
}

fn own_uid(client: &Client, explicit: &Option<String>) -> anyhow::Result<OwnedUserId> {
    let raw = explicit
        .as_deref()
        .or_else(|| client.user_id().map(|u| u.as_str()))
        .ok_or_else(|| anyhow::anyhow!("no user id; pass --user-id"))?;
    UserId::parse(raw).map_err(|e| anyhow::anyhow!("invalid user id '{}': {}", raw, e))
}

fn print_json(label: &str, value: &Value) {
    println!("{}", label);
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

/// Execute one verification command against a live session. Stateful commands
/// pump a few sync rounds first so the crypto machine advances the flow.
async fn execute(session: &Session, cmd: VerifyCommand) -> anyhow::Result<()> {
    let client = &session.client;
    match cmd {
        VerifyCommand::Status => {
            let enc = client.encryption();
            let own = enc
                .get_own_device()
                .await
                .map_err(|e| anyhow::anyhow!("get_own_device: {}", e))?;
            let device = match own {
                Some(d) => json!({
                    "device_id": d.device_id().as_str(),
                    "display_name": d.display_name(),
                    "verified": d.is_verified(),
                    "cross_signed_by_owner": d.is_cross_signed_by_owner(),
                }),
                None => json!({"device_id": null, "verified": false}),
            };
            print_json(
                &format!(
                    "device={} homeserver={}",
                    session.creds.device_id.as_deref().unwrap_or("?"),
                    session.config.homeserver
                ),
                &json!({
                    "device": device,
                    "verification_state": format!("{:?}", enc.verification_state().get()),
                }),
            );
        }

        VerifyCommand::Devices { user_id } => {
            let uid = own_uid(client, &user_id)?;
            let devices = client
                .encryption()
                .get_user_devices(&uid)
                .await
                .map_err(|e| anyhow::anyhow!("get_user_devices: {}", e))?;
            let own_device = client
                .device_id()
                .map(|d| d.to_string())
                .unwrap_or_default();
            let list: Vec<Value> = devices
                .devices()
                .map(|d| {
                    json!({
                        "device_id": d.device_id().as_str(),
                        "display_name": d.display_name(),
                        "verified": d.is_verified(),
                        "cross_signed_by_owner": d.is_cross_signed_by_owner(),
                        "is_our_own": d.device_id().as_str() == own_device,
                    })
                })
                .collect();
            print_json(&format!("devices of {}:", uid), &json!(list));
        }

        VerifyCommand::List { user_id } => {
            let uid = own_uid(client, &user_id)?;
            // The high-level API has no "list verification requests" method;
            // surface per-device flags. In-flight to-device flows are also
            // pushed to Koishi as `verification-request` events by the server.
            let mut out = Vec::new();
            let devices = client
                .encryption()
                .get_user_devices(&uid)
                .await
                .map_err(|e| anyhow::anyhow!("get_user_devices: {}", e))?;
            for d in devices.devices() {
                out.push(json!({
                    "device_id": d.device_id().as_str(),
                    "verified": d.is_verified(),
                    "cross_signed_by_owner": d.is_cross_signed_by_owner(),
                }));
            }
            print_json(
                &format!(
                    "verification requests for {} (flow ids are printed by `request`):",
                    uid
                ),
                &json!(out),
            );
        }

        VerifyCommand::Request { device, user_id } => {
            let uid = own_uid(client, &user_id)?;
            let did: &matrix_sdk::ruma::DeviceId = device.as_str().into();
            let target = client
                .encryption()
                .get_device(&uid, did)
                .await
                .map_err(|e| anyhow::anyhow!("get_device: {}", e))?
                .ok_or_else(|| anyhow::anyhow!("device '{}' not found for {}", device, uid))?;
            let req = target
                .request_verification_with_methods(vec![VerificationMethod::SasV1])
                .await
                .map_err(|e| anyhow::anyhow!("request_verification: {}", e))?;
            let flow = req.flow_id().to_string();
            pump(client, 2).await;
            print_json(
                &format!(
                    "verification request sent. Now: (1) on device {} ACCEPT the popup; \
                     (2) here run `start-sas --flow {}`; (3) run `sas --flow {}` and compare \
                     emoji with the other screen; (4) if equal run `confirm --flow {}` \
                     (on the other device tap Match too).",
                    device, flow, flow, flow
                ),
                &json!({"flow": flow, "user_id": uid.as_str(), "device": device}),
            );
        }

        VerifyCommand::Accept { flow, user_id } => {
            let uid = own_uid(client, &user_id)?;
            pump(client, 1).await;
            let req = client
                .encryption()
                .get_verification_request(&uid, &flow)
                .await
                .ok_or_else(|| anyhow::anyhow!("verification '{}' not found", flow))?;
            req.accept()
                .await
                .map_err(|e| anyhow::anyhow!("accept: {}", e))?;
            pump(client, 2).await;
            print_json("accepted", &json!({"flow": flow, "accepted": true}));
        }

        VerifyCommand::StartSas { flow, user_id } => {
            let uid = own_uid(client, &user_id)?;
            pump(client, 1).await;
            let req = client
                .encryption()
                .get_verification_request(&uid, &flow)
                .await
                .ok_or_else(|| anyhow::anyhow!("verification '{}' not found", flow))?;
            let started = req
                .start_sas()
                .await
                .map_err(|e| anyhow::anyhow!("start_sas: {}", e))?
                .is_some();
            pump(client, 2).await;
            print_json("sas started", &json!({"flow": flow, "started": started}));
        }

        VerifyCommand::Sas { flow, user_id } => {
            let uid = own_uid(client, &user_id)?;
            // A few extra syncs give the peer time to send its start/accept.
            pump(client, 3).await;
            let v = client
                .encryption()
                .get_verification(&uid, &flow)
                .await
                .ok_or_else(|| anyhow::anyhow!("verification '{}' not found", flow))?;
            let sas = v
                .sas()
                .ok_or_else(|| anyhow::anyhow!("'{}' is not a SAS verification", flow))?;
            let emojis: Vec<Value> = sas
                .emoji()
                .map(|arr| {
                    arr.iter()
                        .map(|e| json!({"symbol": e.symbol, "description": e.description}))
                        .collect()
                })
                .unwrap_or_default();
            let decimals = sas.decimals().map(|(a, b, c)| json!([a, b, c]));
            let display = sas
                .emoji()
                .map(|arr| arr.iter().map(|e| e.symbol).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();

            println!("\n=== SAS for flow {} ===", flow);
            println!("state: {:?}", sas.state());
            if !emojis.is_empty() {
                println!("\n比较下面这串 emoji 是否与另一台设备上显示的一致：\n");
                for (i, e) in emojis.iter().enumerate() {
                    println!(
                        "  {:>2}. {}  ({})",
                        i + 1,
                        e["symbol"].as_str().unwrap_or("?"),
                        e["description"].as_str().unwrap_or("?")
                    );
                }
                println!("\n  > {} <\n", display);
            }
            if let Some(d) = &decimals {
                println!("数字版: {}", d);
            }
            if emojis.is_empty() && decimals.is_none() {
                println!("(SAS 字符串尚未生成 —— 等待对方接受并开始验证，稍后重试 `sas`)");
            }
            println!("一致则: confirm --flow {}", flow);
            println!("不一致: mismatch --flow {}\n", flow);
            print_json(
                "raw:",
                &json!({
                    "flow": flow,
                    "state": format!("{:?}", sas.state()),
                    "emojis": emojis,
                    "emoji_display": display,
                    "decimals": decimals,
                    "supports_emoji": sas.supports_emoji(),
                    "can_be_presented": sas.can_be_presented(),
                    "is_done": sas.is_done(),
                    "is_cancelled": sas.is_cancelled(),
                }),
            );
        }

        VerifyCommand::Confirm { flow, user_id } => {
            let uid = own_uid(client, &user_id)?;
            pump(client, 2).await;
            let v = client
                .encryption()
                .get_verification(&uid, &flow)
                .await
                .ok_or_else(|| anyhow::anyhow!("verification '{}' not found", flow))?;
            let sas = v
                .sas()
                .ok_or_else(|| anyhow::anyhow!("'{}' is not a SAS verification", flow))?;
            sas.confirm()
                .await
                .map_err(|e| anyhow::anyhow!("confirm: {}", e))?;
            pump(client, 2).await;
            print_json("confirmed", &json!({"flow": flow, "confirmed": true}));
        }

        VerifyCommand::Mismatch { flow, user_id } => {
            let uid = own_uid(client, &user_id)?;
            let v = client
                .encryption()
                .get_verification(&uid, &flow)
                .await
                .ok_or_else(|| anyhow::anyhow!("verification '{}' not found", flow))?;
            let sas = v
                .sas()
                .ok_or_else(|| anyhow::anyhow!("'{}' is not a SAS verification", flow))?;
            sas.mismatch()
                .await
                .map_err(|e| anyhow::anyhow!("mismatch: {}", e))?;
            print_json("mismatched", &json!({"flow": flow, "mismatched": true}));
        }

        VerifyCommand::Cancel { flow, user_id } => {
            let uid = own_uid(client, &user_id)?;
            if let Some(req) = client
                .encryption()
                .get_verification_request(&uid, &flow)
                .await
            {
                req.cancel()
                    .await
                    .map_err(|e| anyhow::anyhow!("cancel: {}", e))?;
            } else if let Some(v) = client.encryption().get_verification(&uid, &flow).await {
                if let Some(sas) = v.sas() {
                    sas.cancel()
                        .await
                        .map_err(|e| anyhow::anyhow!("cancel: {}", e))?;
                } else {
                    anyhow::bail!("unsupported verification type");
                }
            } else {
                anyhow::bail!("verification '{}' not found", flow);
            }
            print_json("cancelled", &json!({"flow": flow, "cancelled": true}));
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "\
命令:
  status                          查看本机设备验证/交叉签名状态
  devices [--user-id U]           列出某用户(默认自己)的设备及验证标志
  list [--user-id U]              列出设备验证状态
  request --device D              向设备 D 发起 SAS 验证(自验证)
  accept --flow F                 接受收到的验证请求
  start-sas --flow F              启动 SAS 交换
  sas --flow F                    打印 emoji/数字 —— 与另一台设备比对
  confirm --flow F                emoji 一致, 确认(签名并上传)
  mismatch --flow F               emoji 不一致, 中止
  cancel --flow F                 取消验证
  help                            显示本帮助
  quit / exit                     退出

后端保持同一个 SDK 客户端与 crypto store, SAS 状态在整个会话内保持。"
    );
}

async fn repl(session: Session) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    println!("Matrix 设备验证 (SAS) — 交互式会话");
    println!(
        "账号 {} 设备 {}",
        session.creds.user_id,
        session.creds.device_id.as_deref().unwrap_or("?")
    );
    print_help();

    // SAS advances purely on to-device events (m.key.verification.ready /
    // accept / start / key / mac). Only pumping inside a command leaves long
    // gaps where the peer's events sit unconsumed, so the state machine looks
    // stuck (" waiting for the other device"). Keep one sync loop alive for
    // the whole REPL; commands then just read the already-advanced state.
    let bg_client = session.client.clone();
    let bg_sync = tokio::spawn(async move {
        loop {
            match bg_client.sync(SyncSettings::default()).await {
                Ok(_) => break, // sync() only returns on cancellation
                Err(e) => {
                    // keys/upload 400s and transient network errors are common;
                    // back off briefly and keep the loop alive.
                    tracing::debug!("verify background sync: {}", e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    });

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();
    loop {
        print!("verify> ");
        let _ = std::io::stdout().flush();
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            println!();
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "quit" || trimmed == "exit" || trimmed == "q" {
            break;
        }
        if trimmed == "help" || trimmed == "?" {
            print_help();
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let mut argv = vec!["verify".to_string()];
        argv.extend(tokens.iter().map(|s| s.to_string()));
        match ReplLine::try_parse_from(argv) {
            Ok(parsed) => {
                if let Err(e) = execute(&session, parsed.cmd).await {
                    eprintln!("错误: {}", e);
                }
            }
            Err(e) => {
                // clap prints its own message; show the compact one too.
                eprintln!("{}", e);
            }
        }
    }
    bg_sync.abort();
    Ok(())
}

/// Entry point called from main when argv[1] == "verify". `argv` has the
/// leading "verify" token removed.
pub async fn run(argv: Vec<std::ffi::OsString>) -> anyhow::Result<()> {
    let args = VerifyArgs::parse_from(argv);
    // Keep CLI output clean by default: matrix-sdk's crypto background tasks
    // emit noisy ERROR/WARN lines (e.g. a one-time key already uploaded by a
    // sibling session) that are harmless for a one-shot verification command.
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        "warn,matrix_sdk=off,matrix_sdk_crypto=off,matrix_sdk_sqlite=off".into()
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();

    eprintln!("[verify] loading config + restoring session...");
    let session = build_session().await?;
    eprintln!("[verify] session ready as {}", session.creds.user_id);

    match args.command {
        Some(cmd) => {
            pump(&session.client, 1).await;
            execute(&session, cmd).await
        }
        None => repl(session).await,
    }
}
