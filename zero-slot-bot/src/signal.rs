//! Signal detection — two modes controlled by `SIGNAL_MODE` env var.
//!
//! ## `laserstream` (default, mainnet)
//!   Opens a Yellowstone gRPC subscription to `PYTH_SOL_USD_ACCOUNT` via the
//!   Helius LaserStream endpoint.  Each shred-level account change delivers a
//!   raw Pyth v2 price account update.  Parses price, confidence, status and
//!   emits a `PriceSignal` if status = 1 (Trading).
//!
//!   Pyth v2 binary layout:
//!     offset 20  → expo  (i32, e.g. -8)
//!     offset 208 → agg.price  (i64)
//!     offset 216 → agg.conf   (u64)
//!     offset 224 → agg.status (u32, 1 = Trading)
//!
//! ## `hermes` (devnet / fallback)
//!   Polls the Pyth Hermes REST API (`hermes.pyth.network/api/latest_price_feeds`)
//!   every 500 ms using the configured feed ID.  Works on devnet and in
//!   environments where LaserStream is unavailable or the Pyth push oracle is
//!   stale (Pyth v1 push oracle on Solana mainnet is deprecated).
//!
//!   Set `PYTH_FEED_ID` to the hex feed ID (without 0x prefix).
//!   Default: `ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d`
//!   (SOL/USD)

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use yellowstone_grpc_client::GeyserGrpcClient;
use yellowstone_grpc_proto::prelude::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterAccounts,
};

use crate::Config;

// ── Public types ──────────────────────────────────────────────────────────────

/// Price signal emitted whenever a significant price event is detected.
#[derive(Debug, Clone)]
pub struct PriceSignal {
    /// SOL/USD price in nano-USD (1 USD = 1_000_000_000 units).
    pub price_1e9: u64,
    /// Confidence as basis-points of price.
    pub conf_bps: u32,
    /// Solana slot this update was produced in (0 for hermes mode).
    pub slot: u64,
}

// ── Entry-point ───────────────────────────────────────────────────────────────

/// Dispatch to the correct signal source based on `cfg.signal_mode`.
pub async fn run(cfg: Arc<Config>, tx: mpsc::Sender<PriceSignal>) -> Result<()> {
    match cfg.signal_mode.as_str() {
        "hermes" => run_hermes(cfg, tx).await,
        _ => run_laserstream(cfg, tx).await,
    }
}

// ── Mode 1: LaserStream (mainnet gRPC) ────────────────────────────────────────

async fn run_laserstream(cfg: Arc<Config>, tx: mpsc::Sender<PriceSignal>) -> Result<()> {
    info!(
        "signal[laserstream]: connecting to {}",
        cfg.laserstream_endpoint
    );

    let mut client = GeyserGrpcClient::build_from_shared(cfg.laserstream_endpoint.clone())
        .context("build_from_shared")?
        .x_token(Some(cfg.helius_api_key.clone()))
        .context("x_token")?
        .connect()
        .await
        .context("connect")?;

    info!("signal[laserstream]: connected — watching {}", cfg.pyth_account);

    let mut filter = SubscribeRequestFilterAccounts::default();
    filter.account.push(cfg.pyth_account.to_string());

    let mut request = SubscribeRequest::default();
    request.accounts.insert("pyth".to_string(), filter);
    request.commitment = Some(CommitmentLevel::Processed as i32);

    let result = client
        .subscribe_with_request(Some(request))
        .await
        .context("subscribe")?;
    let mut stream = result.1;

    info!("signal[laserstream]: stream open — waiting for Pyth updates");

    while let Some(msg) = stream.next().await {
        match msg {
            Err(e) => return Err(anyhow!("stream error: {}", e)),
            Ok(update) => {
                if let Some(UpdateOneof::Account(acc_update)) = update.update_oneof {
                    let slot = acc_update.slot;
                    let account = match acc_update.account {
                        Some(a) => a,
                        None => continue,
                    };
                    match parse_pyth_price(&account.data) {
                        Some((price_1e9, conf_bps)) => {
                            debug!("pyth[ls]: price={} conf={}bps slot={}", price_1e9, conf_bps, slot);
                            if tx.send(PriceSignal { price_1e9, conf_bps, slot }).await.is_err() {
                                break;
                            }
                        }
                        None => warn!(
                            "signal[laserstream]: parse failed ({} bytes, slot={})",
                            account.data.len(), slot
                        ),
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Mode 2: Hermes HTTP polling (devnet / fallback) ───────────────────────────

async fn run_hermes(_cfg: Arc<Config>, tx: mpsc::Sender<PriceSignal>) -> Result<()> {
    let feed_id = std::env::var("PYTH_FEED_ID")
        .unwrap_or_else(|_| "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d".into());

    let url = format!(
        "https://hermes.pyth.network/api/latest_price_feeds?ids[]={}&binary=false",
        feed_id
    );

    info!("signal[hermes]: polling {} every 500ms", url);

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("build http client")?;

    let mut slot: u64 = 0;

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;

        match fetch_hermes_price(&http, &url).await {
            Ok((price_1e9, conf_bps)) => {
                slot += 1;
                debug!("pyth[hermes]: price={} conf={}bps", price_1e9, conf_bps);
                if tx.send(PriceSignal { price_1e9, conf_bps, slot }).await.is_err() {
                    break;
                }
            }
            Err(e) => warn!("signal[hermes]: fetch failed: {:#}", e),
        }
    }
    Ok(())
}

/// Fetch latest price from Pyth Hermes REST API.
/// Returns `(price_1e9, conf_bps)`.
async fn fetch_hermes_price(http: &reqwest::Client, url: &str) -> Result<(u64, u32)> {
    let resp: serde_json::Value = http
        .get(url)
        .send()
        .await
        .context("hermes GET")?
        .json()
        .await
        .context("hermes JSON")?;

    let feed = resp
        .as_array()
        .and_then(|a| a.first())
        .context("hermes: empty response")?;

    let price_obj = feed.get("price").context("missing price")?;

    let price_str = price_obj["price"].as_str().context("price not string")?;
    let conf_str  = price_obj["conf"].as_str().context("conf not string")?;
    let expo: i32 = price_obj["expo"].as_i64().context("expo not int")? as i32;

    let price_raw: i64 = price_str.parse().context("price parse")?;
    let conf_raw:  u64 = conf_str.parse().context("conf parse")?;

    if price_raw <= 0 {
        return Err(anyhow!("non-positive price"));
    }

    // price_1e9 = price_raw * 10^(expo + 9)
    let shift = expo + 9;
    let price_1e9: u64 = if shift >= 0 {
        (price_raw as u64).saturating_mul(10u64.pow(shift as u32))
    } else {
        let d = 10u64.pow((-shift) as u32);
        if d == 0 { return Err(anyhow!("divisor zero")); }
        (price_raw as u64) / d
    };

    if price_1e9 == 0 {
        return Err(anyhow!("price_1e9 is zero"));
    }

    let conf_bps = (conf_raw.saturating_mul(10_000) / (price_raw as u64)) as u32;

    Ok((price_1e9, conf_bps))
}

// ── Pyth v2 on-chain account parser (used by laserstream mode) ────────────────

fn parse_pyth_price(data: &[u8]) -> Option<(u64, u32)> {
    if data.len() < 228 { return None; }

    let expo   = i32::from_le_bytes(data[20..24].try_into().ok()?);
    let price  = i64::from_le_bytes(data[208..216].try_into().ok()?);
    let conf   = u64::from_le_bytes(data[216..224].try_into().ok()?);
    let status = u32::from_le_bytes(data[224..228].try_into().ok()?);

    if status != 1 || price <= 0 { return None; }

    let shift = expo + 9;
    let price_1e9: u64 = if shift >= 0 {
        (price as u64).saturating_mul(10u64.pow(shift as u32))
    } else {
        let d = 10u64.pow((-shift) as u32);
        if d == 0 { return None; }
        (price as u64) / d
    };

    if price_1e9 == 0 { return None; }

    let conf_bps = (conf.saturating_mul(10_000) / (price as u64)) as u32;
    Some((price_1e9, conf_bps))
}
