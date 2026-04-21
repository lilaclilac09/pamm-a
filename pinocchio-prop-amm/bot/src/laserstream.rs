//! Real-time account-change stream via Solana WebSocket.
//!
//! Solana's `accountSubscribe` is equivalent to Helius LaserStream for
//! single-account monitoring: the validator pushes a notification within
//! one slot (~400ms) of any account write, rather than us polling every N ms.
//!
//! Usage pattern:
//!
//!   // Subscribe to our own PMM pool
//!   let rx = AccountStream::spawn(ws_url, pool_pubkey, "confirmed").await;
//!   while let Ok(()) = rx.changed().await {
//!       let snap = rx.borrow().clone();  // Option<AccountUpdate>
//!   }
//!
//! Two streams are started by the bot:
//!   1. PMM pool  → oracle_arm reads pool state without an extra RPC call
//!   2. HumidiFi pool → humidifi_arm reacts the instant their oracle moves
//!
//! WebSocket protocol (JSON-RPC 2.0):
//!
//!   → {"jsonrpc":"2.0","id":1,"method":"accountSubscribe",
//!      "params":["<pubkey>",{"encoding":"base64","commitment":"confirmed"}]}
//!
//!   ← {"jsonrpc":"2.0","result":<subscription_id>,"id":1}
//!
//!   ← {"jsonrpc":"2.0","method":"accountNotification",
//!      "params":{"subscription":<id>,"result":{
//!          "context":{"slot":<N>},
//!          "value":{"lamports":<N>,"data":["<base64>","base64"],...}}}}

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use std::time::Duration;
use tokio::{sync::watch, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

// ── Public types ──────────────────────────────────────────────────────────────

/// Latest snapshot of an account, delivered via WebSocket notification.
#[derive(Clone, Debug)]
pub struct AccountUpdate {
    pub slot:     u64,
    pub lamports: u64,
    /// Raw account data bytes (already decoded from base64).
    pub data:     Vec<u8>,
}

/// Receiver handle — call `.changed().await` then `.borrow()`.
pub type AccountRx = watch::Receiver<Option<AccountUpdate>>;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Subscribe to `pubkey` account changes and return a receiver.
///
/// The background task reconnects indefinitely on any error (dropped WS,
/// network blip, etc.).  Each reconnect re-subscribes.
pub fn spawn(ws_url: String, pubkey: Pubkey, label: &'static str) -> AccountRx {
    let (tx, rx) = watch::channel::<Option<AccountUpdate>>(None);
    tokio::spawn(async move {
        loop {
            match run_stream(&ws_url, &pubkey, &tx, label).await {
                Ok(()) => info!("laserstream[{}]: stream ended cleanly — reconnecting", label),
                Err(e) => warn!("laserstream[{}]: error: {:#} — reconnecting in 2s", label, e),
            }
            sleep(Duration::from_secs(2)).await;
        }
    });
    rx
}

// ── Internal stream loop ──────────────────────────────────────────────────────

async fn run_stream(
    ws_url: &str,
    pubkey: &Pubkey,
    tx:     &watch::Sender<Option<AccountUpdate>>,
    label:  &str,
) -> anyhow::Result<()> {
    // ws_url should already be wss:// or ws:// (provided by Config::ws_url)
    let ws_url = ws_url.to_string();
    info!("laserstream[{}]: connecting to {}", label, ws_url);

    let (mut ws, _) = connect_async(&ws_url).await
        .map_err(|e| anyhow::anyhow!("WS connect: {e}"))?;

    // ── Subscribe ─────────────────────────────────────────────────────────────
    let sub_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "accountSubscribe",
        "params": [
            pubkey.to_string(),
            { "encoding": "base64", "commitment": "confirmed" }
        ]
    });
    ws.send(Message::Text(sub_msg.to_string())).await
        .map_err(|e| anyhow::anyhow!("WS send subscribe: {e}"))?;

    // ── Message loop ──────────────────────────────────────────────────────────
    let mut subscription_id: Option<u64> = None;

    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| anyhow::anyhow!("WS recv: {e}"))?;
        let text = match msg {
            Message::Text(t)  => t,
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Ping(p)  => { ws.send(Message::Pong(p)).await.ok(); continue; }
            Message::Close(_) => return Ok(()),
            _                 => continue,
        };

        let raw: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v)  => v,
            Err(e) => { warn!("laserstream[{}]: JSON parse error: {e}", label); continue; }
        };

        // ── Subscription confirmation ─────────────────────────────────────────
        if raw.get("id") == Some(&serde_json::json!(1)) {
            if let Some(id) = raw["result"].as_u64() {
                subscription_id = Some(id);
                info!("laserstream[{}]: subscribed (id={})", label, id);
            } else {
                error!("laserstream[{}]: subscribe failed: {}", label, raw);
                return Err(anyhow::anyhow!("subscribe failed"));
            }
            continue;
        }

        // ── Account notification ───────────────────────────────────────────────
        if raw.get("method").and_then(|m| m.as_str()) != Some("accountNotification") {
            continue;
        }

        let params = &raw["params"];
        let sub = params["subscription"].as_u64();
        if sub != subscription_id {
            debug!("laserstream[{}]: ignoring notification for sub {:?}", label, sub);
            continue;
        }

        let result = &params["result"];
        let slot     = result["context"]["slot"].as_u64().unwrap_or(0);
        let lamports = result["value"]["lamports"].as_u64().unwrap_or(0);

        // data is ["<base64_string>", "base64"]
        let data_b64 = match result["value"]["data"][0].as_str() {
            Some(s) => s,
            None    => { warn!("laserstream[{}]: no data field", label); continue; }
        };
        let data = match base64::engine::general_purpose::STANDARD.decode(data_b64) {
            Ok(b)  => b,
            Err(e) => { warn!("laserstream[{}]: base64 decode: {e}", label); continue; }
        };

        debug!("laserstream[{}]: slot={} len={}", label, slot, data.len());
        let _ = tx.send(Some(AccountUpdate { slot, lamports, data }));
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert an HTTP(S) RPC URL to a WebSocket URL.
/// e.g. "https://api.devnet.solana.com" → "wss://api.devnet.solana.com"
///      "http://api.devnet.solana.com"  → "ws://api.devnet.solana.com"
fn to_ws_url(url: &str) -> String {
    if url.starts_with("https://") {
        url.replacen("https://", "wss://", 1)
    } else if url.starts_with("http://") {
        url.replacen("http://", "ws://", 1)
    } else {
        url.to_string()
    }
}

// ── Pool-specific helper ──────────────────────────────────────────────────────

use crate::risk::PoolSnapshot;

/// Spawn a stream for our PMM pool and return a receiver of parsed snapshots.
/// Returns `None` when no notification has arrived yet.
pub fn spawn_pool_stream(
    ws_url:   &str,
    pubkey:   &Pubkey,
) -> watch::Receiver<Option<PoolSnapshot>> {
    let raw_rx = spawn(ws_url.to_string(), *pubkey, "pmm_pool");
    let (snap_tx, snap_rx) = watch::channel::<Option<PoolSnapshot>>(None);

    tokio::spawn(async move {
        let mut raw = raw_rx;
        loop {
            if raw.changed().await.is_err() { break; }
            if let Some(update) = raw.borrow().clone() {
                if let Some(snap) = PoolSnapshot::from_bytes(&update.data) {
                    let _ = snap_tx.send(Some(snap));
                }
            }
        }
    });

    snap_rx
}

/// Spawn a stream for any arbitrary account and return a raw byte receiver.
/// Useful for HumidiFi pool where we don't have a known schema.
pub fn spawn_raw_stream(
    ws_url: &str,
    pubkey: &Pubkey,
    label:  &'static str,
) -> AccountRx {
    spawn(ws_url.to_string(), *pubkey, label)
}
