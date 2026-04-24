//! HumidiFi oracle-latency arbitrage arm.
//!
//! Strategy: HumidiFi is a Prop AMM. It posts on-chain oracle updates every
//! ~80-200ms. Between Pyth moving and HumidiFi catching up, their pool is
//! mispriced. We trade against it via Jupiter (which routes through HumidiFi).
//!
//! Detection: request a Jupiter quote forced through HumidiFi, then compare
//! the quote's `outAmount` against the fair output computed from Pyth. If
//! we'd receive more than fair by `ARB_THRESHOLD_BPS`, fire the swap.
//! `outAmount` already reflects HumidiFi's pool math (fees + price impact),
//! so the comparison is end-to-end honest.
//!
//! Why Jupiter instead of direct HumidiFi CPI?
//!   HumidiFi's swap instruction layout is proprietary/obfuscated. Jupiter
//!   integrates them and handles all instruction construction. We force the
//!   route via `dexes[]=Humidifi` in the quote request.

use anyhow::{Context, Result};
use serde::Deserialize;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    signer::Signer,
    transaction::VersionedTransaction,
};
use std::{sync::Arc, time::{Duration, Instant}};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::{config::Config, risk::SharedState};

// ── Constants ─────────────────────────────────────────────────────────────────

const WSOL_MINT:  &str = "So11111111111111111111111111111111111111112";
const USDC_MINT:  &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

const USDC_DECIMALS_FACTOR: u128 = 1_000_000;          // 1 USDC = 10^6 units
const SOL_DECIMALS_FACTOR:  u128 = 1_000_000_000;      // 1 SOL  = 10^9 lamports

/// Minimum price divergence to trade (bps).
///
/// Grounded in Jump Crypto's DFBA research (jumpcrypto.com/writing/dual-flow-batch-auction):
/// Expected latency drift per 400ms slot = vol_bps * sqrt(slot_ms / year_ms)
/// For SOL (annualised vol ~80%): ≈ 0.90 bps/slot.
/// Set threshold at 3x noise floor = ~3bps to filter spurious quotes.
/// Jupiter's fee overhead on HumidiFi routes is ~2-3bps, so effective min is 5bps.
const ARB_THRESHOLD_BPS: u64 = 5;

/// Notional per trade (SOL lamports).
///
/// Jump's invariance model: profit = size * drift_bps / 10_000.
/// At 5bps threshold, need ≥5 SOL to clear Jupiter fee (~$0.10 at $143).
/// Scale up after confirming route profitability in production.
const TRADE_SIZE_LAMPORTS: u64 = 5_000_000_000; // 5 SOL

/// Pause between arb cycles.
const POLL_INTERVAL_MS: u64 = 400; // one Solana slot

/// Cooldown after executing a trade.
const TRADE_COOLDOWN_MS: u64 = 800;

/// Circuit breaker: max trades per minute.
const MAX_TRADES_PER_MINUTE: u32 = 6;

// ── Jupiter API types ─────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct JupSwapResponse {
    swap_transaction: String, // base64-encoded VersionedTransaction
}

// ── Public entry point ────────────────────────────────────────────────────────

pub async fn run(cfg: Arc<Config>, state: Arc<SharedState>) {
    let client = RpcClient::new(cfg.rpc_url.clone());
    let http   = reqwest::Client::builder()
        .timeout(Duration::from_millis(400))
        .build()
        .expect("http client");

    info!("humidifi_arm: starting (threshold={}bps, size={}SOL)",
        ARB_THRESHOLD_BPS, TRADE_SIZE_LAMPORTS / 1_000_000_000,
    );

    let mut last_trade_at       = Instant::now()
        .checked_sub(Duration::from_secs(10))
        .unwrap_or_else(Instant::now);
    let mut trades_this_minute: u32 = 0;
    let mut minute_start        = Instant::now();
    let mut cycle: u64          = 0;

    loop {
        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

        if state.is_halted() {
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        cycle += 1;

        // Reset per-minute counter
        if minute_start.elapsed() > Duration::from_secs(60) {
            trades_this_minute = 0;
            minute_start = Instant::now();
        }

        if last_trade_at.elapsed() < Duration::from_millis(TRADE_COOLDOWN_MS) {
            continue;
        }
        if trades_this_minute >= MAX_TRADES_PER_MINUTE {
            debug!("[humidifi #{}] rate limit ({}/min)", cycle, trades_this_minute);
            continue;
        }

        // Pyth price from shared state (oracle_arm keeps this current)
        let pyth_price_1e9 = state.pool.read().await.oracle_price;
        if pyth_price_1e9 == 0 {
            debug!("[humidifi #{}] no pyth price yet", cycle);
            continue;
        }

        // ── Check arb, execute if profitable ──────────────────────────────────
        match check_arb_and_execute(cycle, pyth_price_1e9, &http, &client, &cfg).await {
            Ok(Some(profit_bps)) => {
                info!("[humidifi #{}] executed arb: ~{}bps profit", cycle, profit_bps);
                trades_this_minute += 1;
                last_trade_at = Instant::now();

                // Update trade metrics in shared state
                let mut m = state.metrics.write().await;
                m.trade_cycles += 1;
            }
            Ok(None) => {
                debug!("[humidifi #{}] no arb (pyth={})", cycle, pyth_price_1e9);
            }
            Err(e) => {
                warn!("[humidifi #{}] error: {:#}", cycle, e);
            }
        }
    }
}

// ── Arb detection + execution ─────────────────────────────────────────────────

async fn check_arb_and_execute(
    cycle:          u64,
    pyth_price_1e9: u64,
    http:           &reqwest::Client,
    client:         &RpcClient,
    cfg:            &Arc<Config>,
) -> Result<Option<u64>> {
    // ── Direction A: buy SOL from HumidiFi (USDC → WSOL)
    // Fair output for `usdc_in` USDC at Pyth price = usdc_in (6dec) * 1e9 / pyth_price_1e9 → SOL lamports
    let usdc_in        = usdc_for_sol(TRADE_SIZE_LAMPORTS, pyth_price_1e9);
    let (buy_quote, buy_out_sol) =
        jupiter_quote(http, USDC_MINT, WSOL_MINT, usdc_in).await?;
    let fair_out_sol   = ((usdc_in as u128) * SOL_DECIMALS_FACTOR
                          * SOL_DECIMALS_FACTOR
                          / (pyth_price_1e9 as u128)
                          / USDC_DECIMALS_FACTOR) as u64;
    let buy_edge_bps   = edge_bps(buy_out_sol, fair_out_sol);

    debug!("[humidifi #{}] buy: out={} fair={} edge={}bps",
        cycle, buy_out_sol, fair_out_sol, buy_edge_bps);

    if buy_edge_bps >= ARB_THRESHOLD_BPS as i64 {
        let profit_bps = buy_edge_bps as u64;
        info!("[humidifi #{}] BUY SOL arb: +{}bps (humidifi cheap)", cycle, profit_bps);
        execute_jup_swap(http, client, cfg, &buy_quote).await
            .context("execute buy SOL")?;
        return Ok(Some(profit_bps));
    }

    // ── Direction B: sell SOL to HumidiFi (WSOL → USDC)
    // Fair output for TRADE_SIZE_LAMPORTS lamports = sol * pyth_price_1e9 / 1e9 → USDC (6dec via /1e3)
    let (sell_quote, sell_out_usdc) =
        jupiter_quote(http, WSOL_MINT, USDC_MINT, TRADE_SIZE_LAMPORTS).await?;
    let fair_out_usdc  = ((TRADE_SIZE_LAMPORTS as u128) * (pyth_price_1e9 as u128)
                          * USDC_DECIMALS_FACTOR
                          / SOL_DECIMALS_FACTOR
                          / SOL_DECIMALS_FACTOR) as u64;
    let sell_edge_bps  = edge_bps(sell_out_usdc, fair_out_usdc);

    debug!("[humidifi #{}] sell: out={} fair={} edge={}bps",
        cycle, sell_out_usdc, fair_out_usdc, sell_edge_bps);

    if sell_edge_bps >= ARB_THRESHOLD_BPS as i64 {
        let profit_bps = sell_edge_bps as u64;
        info!("[humidifi #{}] SELL SOL arb: +{}bps (humidifi expensive)", cycle, profit_bps);
        execute_jup_swap(http, client, cfg, &sell_quote).await
            .context("execute sell SOL")?;
        return Ok(Some(profit_bps));
    }

    Ok(None)
}

/// Edge in basis points: how much `actual_out` beats `fair_out` (negative if it loses).
fn edge_bps(actual_out: u64, fair_out: u64) -> i64 {
    if fair_out == 0 { return i64::MIN; }
    ((actual_out as i128 - fair_out as i128) * 10_000 / fair_out as i128) as i64
}

// ── Jupiter quote (HumidiFi route only) ──────────────────────────────────────
//
// Returns (raw quote JSON, out_amount). `out_amount` is what the route would
// actually pay — already net of HumidiFi's pool fees and price impact, so the
// caller can compare it directly against an oracle-derived fair value.

async fn jupiter_quote(
    http:        &reqwest::Client,
    input_mint:  &str,
    output_mint: &str,
    amount:      u64,
) -> Result<(serde_json::Value, u64)> {
    let url = format!(
        "https://quote-api.jup.ag/v6/quote\
         ?inputMint={in_}\
         &outputMint={out}\
         &amount={amt}\
         &slippageBps=50\
         &dexes[]=Humidifi\
         &onlyDirectRoutes=true",
        in_  = input_mint,
        out  = output_mint,
        amt  = amount,
    );

    let resp = http.get(&url)
        .send().await.context("jupiter quote request")?;

    if !resp.status().is_success() {
        anyhow::bail!("jupiter quote {}: {}", resp.status(),
            resp.text().await.unwrap_or_default());
    }

    let raw: serde_json::Value = resp.json().await.context("jupiter quote JSON")?;

    // Verify HumidiFi is actually in the route
    let labels: Vec<String> = raw["routePlan"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|s| s["swapInfo"]["label"].as_str().map(|l| l.to_lowercase()))
        .collect();

    if !labels.iter().any(|l| l.contains("humidifi")) {
        anyhow::bail!("not routed through HumidiFi (got: {:?})", labels);
    }

    let out_amount: u64 = raw["outAmount"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("quote missing outAmount"))?;

    Ok((raw, out_amount))
}

// ── Execute Jupiter swap ──────────────────────────────────────────────────────

async fn execute_jup_swap(
    http:   &reqwest::Client,
    client: &RpcClient,
    cfg:    &Arc<Config>,
    quote:  &serde_json::Value,
) -> Result<()> {
    let body = serde_json::json!({
        "quoteResponse":    quote,
        "userPublicKey":    cfg.wallet.pubkey().to_string(),
        "wrapAndUnwrapSol": true,
        "dynamicComputeUnitLimit": true,
        "prioritizationFeeLamports": {
            "priorityLevelWithMaxLamports": {
                "priorityLevel": "high",
                "maxLamports":   500_000u64,
            }
        },
    });

    let swap_resp: JupSwapResponse = http
        .post("https://quote-api.jup.ag/v6/swap")
        .json(&body)
        .send().await.context("jupiter /swap request")?
        .json().await.context("jupiter /swap JSON")?;

    let tx_bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        &swap_resp.swap_transaction,
    ).context("base64 decode")?;

    let mut tx: VersionedTransaction =
        bincode::deserialize(&tx_bytes).context("deserialise VersionedTx")?;

    // Jupiter returns the tx pre-shaped for us as the fee payer at signature slot 0.
    // Sign the serialized message and place our signature at slot 0; preserve any
    // additional signatures Jupiter included (e.g. ephemeral session keys).
    let our_sig = cfg.wallet.sign_message(&tx.message.serialize());
    if tx.signatures.is_empty() {
        anyhow::bail!("jupiter swap tx has no signature slots");
    }
    tx.signatures[0] = our_sig;

    let sig = client
        .send_transaction(&tx)
        .context("send_transaction")?;

    info!("humidifi_arm: swap submitted {}", sig);
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Approximate USDC amount (6 decimals) needed to buy `sol_lamports` at oracle price.
/// oracle_price_1e9 = USDC_per_SOL * 1e9
fn usdc_for_sol(sol_lamports: u64, oracle_price_1e9: u64) -> u64 {
    // usdc_units = sol_lamports (1e9) * price (1e9) / 1e9 / 1e3
    //            = sol_lamports * price / 1e12
    ((sol_lamports as u128)
        .saturating_mul(oracle_price_1e9 as u128)
        / 1_000_000_000_000u128) as u64
}
