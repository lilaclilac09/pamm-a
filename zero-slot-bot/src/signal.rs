//! LaserStream signal detector.
//!
//! Opens a Yellowstone gRPC subscription to the Pyth SOL/USD price account
//! via the Helius LaserStream endpoint.  Each time the account data changes
//! (shred-level — one update per block produced) we parse the Pyth v2 price
//! account, validate that the status is Trading, and forward a `PriceSignal`
//! to the executor via the bounded mpsc channel.
//!
//! Pyth price-account v2 binary layout (relevant offsets):
//!   20  expo       i32  — typically -8 for SOL/USD
//!  208  agg.price  i64  — aggregate price (raw, apply expo to get USD)
//!  216  agg.conf   u64  — confidence interval (same scale as price)
//!  224  agg.status u32  — 1 = Trading, anything else → skip

use std::sync::Arc;

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

/// Price signal emitted whenever the Pyth account changes.
#[derive(Debug, Clone)]
pub struct PriceSignal {
    /// SOL/USD price in nano-USD (1 USD = 1_000_000_000 units).
    pub price_1e9: u64,
    /// Confidence as basis-points of price.
    pub conf_bps: u32,
    /// Solana slot this update was produced in.
    pub slot: u64,
}

// ── Entry-point ───────────────────────────────────────────────────────────────

/// Connect to LaserStream, subscribe to the Pyth account, and send
/// `PriceSignal`s down `tx`.  Returns on any stream error so the caller can
/// reconnect.
pub async fn run(cfg: Arc<Config>, tx: mpsc::Sender<PriceSignal>) -> Result<()> {
    info!(
        "signal: connecting to LaserStream at {}",
        cfg.laserstream_endpoint
    );

    // GeyserGrpcClient::build_from_shared → GeyserGrpcBuilderResult<Builder>
    // Builder::x_token  → GeyserGrpcBuilderResult<Builder>
    // Builder::connect  → GeyserGrpcBuilderResult<Client<impl Interceptor>>
    let mut client = GeyserGrpcClient::build_from_shared(cfg.laserstream_endpoint.clone())
        .context("laserstream build_from_shared")?
        .x_token(Some(cfg.helius_api_key.clone()))
        .context("laserstream x_token")?
        .connect()
        .await
        .context("laserstream connect")?;

    info!("signal: connected — subscribing to {}", cfg.pyth_account);

    let mut filter = SubscribeRequestFilterAccounts::default();
    filter.account.push(cfg.pyth_account.to_string());

    let mut request = SubscribeRequest::default();
    request.accounts.insert("pyth".to_string(), filter);
    request.commitment = Some(CommitmentLevel::Processed as i32);

    // Returns (sink, stream).  Keep only the stream side.
    let subscribe_result = client
        .subscribe_with_request(Some(request))
        .await
        .context("laserstream subscribe")?;
    let mut stream = subscribe_result.1;

    info!("signal: stream open — waiting for Pyth updates");

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
                            debug!(
                                "pyth: price={} conf={}bps slot={}",
                                price_1e9, conf_bps, slot
                            );
                            let sig = PriceSignal { price_1e9, conf_bps, slot };
                            if tx.send(sig).await.is_err() {
                                // executor dropped its receiver — stop
                                break;
                            }
                        }
                        None => {
                            warn!(
                                "signal: pyth parse failed ({} bytes, slot={})",
                                account.data.len(),
                                slot
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Pyth v2 account parser ────────────────────────────────────────────────────

/// Parse a Pyth v2 price account from raw bytes.
///
/// Returns `(price_1e9, conf_bps)` where:
///  - `price_1e9` is the SOL/USD price in nano-USD (1 USD = 1_000_000_000)
///  - `conf_bps`  is the confidence interval as basis-points of the price
///
/// Returns `None` if the data is too short, price is non-positive, or the
/// aggregate status is not 1 (Trading).
fn parse_pyth_price(data: &[u8]) -> Option<(u64, u32)> {
    // Need at least 228 bytes to reach agg.status at offset 224
    if data.len() < 228 {
        return None;
    }

    let expo = i32::from_le_bytes(data[20..24].try_into().ok()?);
    let price = i64::from_le_bytes(data[208..216].try_into().ok()?);
    let conf = u64::from_le_bytes(data[216..224].try_into().ok()?);
    let status = u32::from_le_bytes(data[224..228].try_into().ok()?);

    // Only emit signals when the feed is actively traded (status = 1)
    if status != 1 || price <= 0 {
        return None;
    }

    // price_1e9 = price * 10^(expo + 9)
    // SOL/USD expo = -8 → shift = +1 → multiply by 10
    let shift = expo + 9;
    let price_1e9: u64 = if shift >= 0 {
        (price as u64).saturating_mul(10u64.pow(shift as u32))
    } else {
        let divisor = 10u64.pow((-shift) as u32);
        if divisor == 0 {
            return None;
        }
        (price as u64) / divisor
    };

    if price_1e9 == 0 {
        return None;
    }

    // conf_bps = (conf / |price|) * 10_000
    // Both conf and price share the same exponent, so it cancels out.
    let conf_bps = (conf.saturating_mul(10_000) / (price as u64)) as u32;

    Some((price_1e9, conf_bps))
}
