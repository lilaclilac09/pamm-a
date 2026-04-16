//! HumidiFi oracle-latency arbitrage arm.
//!
//! Strategy: HumidiFi is a Prop AMM. It posts on-chain oracle updates every
//! ~80-200ms. Between Pyth moving and HumidiFi catching up, their pool is
//! mispriced. We trade against it via Jupiter (which routes through HumidiFi).
//!
//! Architecture:
//!   1. Every slot (~400ms): poll HumidiFi's SOL/USDC pool account via RPC.
//!      Parse their last_oracle_update timestamp to detect staleness.
//!   2. On each poll: request a Jupiter quote forced through HumidiFi.
//!   3. Compare Jupiter's execution price vs Pyth price (from SharedState).
//!   4. If delta > ARB_THRESHOLD_BPS: execute the swap via Jupiter.
//!
//! Why Jupiter instead of direct HumidiFi CPI?
//!   HumidiFi's swap instruction layout is proprietary/obfuscated. Jupiter
//!   integrates them and handles all instruction construction. We force the
//!   route via `dexes[]=Humidifi` in the quote request.
//!
//!                    ┌─────────────────────────────────────┐
//!   DATA FLOW:       │                                     │
//!                    │  oracle_arm keeps SharedState fresh  │
//!                    │  oracle_price = Pyth SOL/USDC price  │
//!                    └─────────────────────────────────────┘
//!                                      │
//!   every 400ms                        ▼
//!   poll RPC ──► HumidiFi pool ──► staleness check
//!                                      │
//!                                 Jupiter quote
//!                                 (HumidiFi only)
//!                                      │
//!                              exec_price vs pyth_price
//!                                      │
//!                          delta > threshold? ──► execute swap

use anyhow::{Context, Result};
use serde::Deserialize;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signer::Signer,
    transaction::VersionedTransaction,
};
use std::{str::FromStr, sync::Arc, time::{Duration, Instant}};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::{config::Config, risk::SharedState};

// ── Constants ─────────────────────────────────────────────────────────────────

/// HumidiFi WSOL-USDC pool account (from Solscan: solscan.io/amm/humidifi).
const HUMIDIFI_POOL: &str = "FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH";

const WSOL_MINT:  &str = "So11111111111111111111111111111111111111112";
const USDC_MINT:  &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

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

    let pool_pubkey = match Pubkey::from_str(HUMIDIFI_POOL) {
        Ok(p) => p,
        Err(e) => { error!("humidifi_arm: invalid pool pubkey: {}", e); return; }
    };

    info!("humidifi_arm: starting (pool={:.8}, threshold={}bps, size={}SOL)",
        HUMIDIFI_POOL, ARB_THRESHOLD_BPS, TRADE_SIZE_LAMPORTS / 1_000_000_000,
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

        // ── Pool staleness check (optional signal quality filter) ─────────────
        // HumidiFi's pool account layout is unknown, so we skip byte parsing.
        // Instead we rely entirely on Jupiter's execution price as the signal.
        // If you reverse-engineer their layout later, add a timestamp check here.
        let _ = pool_pubkey; // pool account fetching reserved for future use

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
    // If HumidiFi is selling SOL cheap relative to Pyth → buy.
    let usdc_in = usdc_for_sol(TRADE_SIZE_LAMPORTS, pyth_price_1e9);
    let (buy_quote, buy_bps) = jupiter_quote(http, USDC_MINT, WSOL_MINT, usdc_in).await?;

    // buy_bps < 10_000 means we received MORE SOL per USDC than Pyth fair value.
    // Equivalently: HumidiFi SOL price < Pyth price.
    if 10_000u64.saturating_sub(buy_bps) >= ARB_THRESHOLD_BPS {
        let profit_bps = 10_000 - buy_bps;
        info!("[humidifi #{}] BUY SOL arb: +{}bps (humidifi cheap)", cycle, profit_bps);
        execute_jup_swap(http, client, cfg, &buy_quote).await
            .context("execute buy SOL")?;
        return Ok(Some(profit_bps));
    }

    // ── Direction B: sell SOL to HumidiFi (WSOL → USDC)
    // If HumidiFi is buying SOL expensive relative to Pyth → sell.
    let (sell_quote, sell_bps) = jupiter_quote(http, WSOL_MINT, USDC_MINT, TRADE_SIZE_LAMPORTS).await?;

    // sell_bps > 10_000 means we received MORE USDC per SOL than Pyth fair value.
    // Equivalently: HumidiFi SOL price > Pyth price.
    if sell_bps.saturating_sub(10_000) >= ARB_THRESHOLD_BPS {
        let profit_bps = sell_bps - 10_000;
        info!("[humidifi #{}] SELL SOL arb: +{}bps (humidifi expensive)", cycle, profit_bps);
        execute_jup_swap(http, client, cfg, &sell_quote).await
            .context("execute sell SOL")?;
        return Ok(Some(profit_bps));
    }

    Ok(None)
}

// ── Jupiter quote (HumidiFi route only) ──────────────────────────────────────
//
// Returns (raw quote JSON, exec_price_bps).
//
// exec_price_bps is the price you actually got as a fraction of fair value,
// in basis points where 10_000 = exactly fair (Pyth price).
//
//   > 10_000: you got better than fair (sell direction arb)
//   < 10_000: you paid less than fair (buy direction arb)
//
// We derive this from Jupiter's priceImpactPct:
//   A negative impact means the pool is mispriced IN YOUR FAVOR.
//   Positive impact = you moved the price against yourself (normal).

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

    // priceImpactPct: positive = you moved price against yourself (normal trade)
    //                 negative = pool is mispriced in your favor (arb!)
    let impact_pct: f64 = raw["priceImpactPct"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    // Convert to basis points, flipped so that arb opportunity = value > 10_000
    let impact_bps = (impact_pct * 100.0) as i64;
    let exec_bps = (10_000i64 - impact_bps).clamp(0, 30_000) as u64;

    debug!("jup quote {}->{}: amount={} impact={:.4}% exec_bps={}",
        &input_mint[..6], &output_mint[..6], amount, impact_pct, exec_bps);

    Ok((raw, exec_bps))
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

    // Sign: Jupiter returns a partially-signed tx, add our signature at slot 0
    let our_sig = cfg.wallet.sign_message(&tx.message.serialize());
    if !tx.signatures.is_empty() {
        tx.signatures[0] = our_sig;
    }

    let legacy: solana_sdk::transaction::Transaction = tx.into_legacy_transaction()
        .context("humidifi tx is not a legacy transaction — cannot sign directly")?;

    let sig = client
        .send_and_confirm_transaction(&legacy)
        .context("send_and_confirm")?;

    info!("humidifi_arm: swap confirmed {}", sig);
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
