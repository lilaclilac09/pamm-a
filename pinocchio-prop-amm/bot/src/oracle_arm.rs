//! Oracle arm — event-driven, sub-second latency.
//!
//! Architecture:
//!   1. On startup: HTTP fetch to seed the first price immediately.
//!   2. Background task: Pyth websocket streams price updates into a watch channel.
//!   3. Main loop: `tokio::select!` on either
//!        a. watch channel changed  → new price arrived, check skip filter
//!        b. silence timer expired  → force send even if price stable
//!
//! Spread = BASE_SPREAD_BPS + vol_adj
//! vol_adj = max(pyth_conf_bps, ewma_vol_bps)  capped at MAX_VOL_FACTOR
//!
//! This means the pool spread tracks Pyth's own confidence interval at minimum,
//! widening automatically during volatile regimes — so you can safely set
//! BASE_SPREAD_BPS=2 and let the vol signal do the risk management.

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    signer::Signer,
};
use std::{sync::Arc, time::Instant};
use tokio::{sync::watch, time::{sleep, Duration, timeout}};
use tracing::{debug, error, info, warn};

use crate::{
    config::Config,
    cu_budget::send_budgeted,
    pyth::{fetch_pyth_price, spawn_price_stream, EwmaVol},
    risk::{PoolSnapshot, SharedState},
};

// ── Blockhash cache ───────────────────────────────────────────────────────────

struct BlockhashCache {
    hash:       Option<Hash>,
    fetched_at: Option<Instant>,
}

impl BlockhashCache {
    fn new() -> Self { BlockhashCache { hash: None, fetched_at: None } }

    fn needs_refresh(&self) -> bool {
        match self.fetched_at {
            None    => true,
            Some(t) => t.elapsed().as_secs() >= 20,
        }
    }

    fn refresh(&mut self, client: &RpcClient) -> Result<()> {
        let bh = client.get_latest_blockhash().context("get_latest_blockhash")?;
        self.hash       = Some(bh);
        self.fetched_at = Some(Instant::now());
        Ok(())
    }

    fn get(&self) -> Result<Hash> {
        self.hash.ok_or_else(|| anyhow::anyhow!("blockhash not yet fetched"))
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(cfg: Arc<Config>, state: Arc<SharedState>) {
    let client = RpcClient::new(cfg.rpc_url.clone());
    let http   = reqwest::Client::new();

    // ── Seed initial price via HTTP so first TX can go immediately ────────────
    let (price_tx, mut price_rx) = watch::channel::<Option<(u64, u32)>>(None);

    info!("oracle_arm: seeding initial price via HTTP...");
    match fetch_pyth_price(&http, &cfg.pyth_feed_id).await {
        Ok((p, c)) => {
            let _ = price_tx.send(Some((p, c)));
            info!("oracle_arm: seed price={} conf={}bps", p, c);
        }
        Err(e) => warn!("oracle_arm: HTTP seed failed (will wait for ws): {:#}", e),
    }

    // ── Start websocket stream (runs forever, reconnects on errors) ───────────
    spawn_price_stream(cfg.pyth_feed_id.clone(), price_tx);

    let mut bh            = BlockhashCache::new();
    let mut ewma          = EwmaVol::new(0.06); // α=0.06 → ~32-sample half-life
    let mut cycle:        u64 = 0;
    let mut last_sent_price: u64 = 0;
    let mut last_sent_at:    Instant = Instant::now()
        .checked_sub(Duration::from_secs(cfg.max_oracle_silence_secs + 1))
        .unwrap_or_else(Instant::now);

    info!(
        "oracle_arm: running (skip_threshold={}bps, max_silence={}s, base_spread={}bps)",
        cfg.min_oracle_update_bps, cfg.max_oracle_silence_secs, cfg.base_spread_bps,
    );

    loop {
        if state.is_halted() {
            info!("oracle_arm: halted — pausing");
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        // ── Wait for next event: price update OR silence deadline ─────────────
        let silence_remaining = {
            let elapsed = last_sent_at.elapsed().as_secs();
            cfg.max_oracle_silence_secs.saturating_sub(elapsed)
        };

        // wait at most `silence_remaining` seconds for a new price
        let got_new_price = timeout(
            Duration::from_secs(silence_remaining.max(1)),
            price_rx.changed(),
        ).await;

        // ── Read current price from channel ───────────────────────────────────
        let (oracle_price, conf_bps) = match *price_rx.borrow() {
            Some(v) => v,
            None => {
                debug!("oracle_arm: no price yet — waiting");
                continue;
            }
        };

        cycle += 1;

        // ── Compute vol_adj via EWMA ──────────────────────────────────────────
        ewma.update(oracle_price);
        let hist_vol = ewma.vol_bps();
        let vol_adj  = conf_bps.max(hist_vol).min(cfg.max_vol_factor);

        // ── Skip filter ───────────────────────────────────────────────────────
        let silence_expired = last_sent_at.elapsed().as_secs() >= cfg.max_oracle_silence_secs;
        let price_moved = if last_sent_price > 0 {
            let delta_bps = oracle_price.abs_diff(last_sent_price)
                .saturating_mul(10_000)
                / last_sent_price;
            delta_bps >= cfg.min_oracle_update_bps as u64
        } else {
            true // always send on first cycle
        };

        let timed_out = got_new_price.is_err(); // Err = timeout (no new price arrived)
        if !price_moved && !silence_expired {
            debug!(
                "[oracle #{}] skip: delta<{}bps, silence={}s, timed_out={}",
                cycle, cfg.min_oracle_update_bps,
                last_sent_at.elapsed().as_secs(), timed_out,
            );
            let mut m = state.metrics.write().await;
            m.oracle_cycles = cycle;
            continue;
        }

        info!(
            "[oracle #{}] sending: price={} conf={}bps ewma={}bps vol_adj={}",
            cycle, oracle_price, conf_bps, hist_vol, vol_adj,
        );

        // ── Pool snapshot → targets ───────────────────────────────────────────
        // Save previous reserves for volume delta calculation
        let prev_reserve_a = state.pool.read().await.reserve_a;
        let prev_reserve_b = state.pool.read().await.reserve_b;

        let pool_data = match client.get_account_data(&cfg.pool_pubkey) {
            Ok(d)  => d,
            Err(e) => {
                handle_failure(&state, &cfg, cycle, format!("pool fetch: {:#}", e)).await;
                continue;
            }
        };
        let snap = match PoolSnapshot::from_bytes(&pool_data) {
            Some(s) => s,
            None    => {
                handle_failure(&state, &cfg, cycle, "pool data too short".into()).await;
                continue;
            }
        };
        *state.pool.write().await = snap.clone();
        let (target_a, target_b) = target_from_env_or_reserves(&snap);

        // ── Build + send ──────────────────────────────────────────────────────
        if bh.needs_refresh() {
            if let Err(e) = bh.refresh(&client) {
                handle_failure(&state, &cfg, cycle, format!("blockhash: {:#}", e)).await;
                continue;
            }
        }
        let blockhash = match bh.get() {
            Ok(h)  => h,
            Err(e) => {
                handle_failure(&state, &cfg, cycle, format!("{:#}", e)).await;
                continue;
            }
        };

        let ix = build_update_oracle_ix(&cfg, oracle_price, vol_adj, target_a, target_b);
        match send_budgeted(&client, &[ix], &cfg.wallet, blockhash)
            .context("UPDATE_ORACLE send_budgeted")
        {
            Ok((sig, _cu)) => {
                let effective_spread = cfg.base_spread_bps
                    .saturating_add(vol_adj)
                    .min(cfg.max_vol_factor);
                info!("[oracle #{}] ok: {} spread={}bps", cycle, sig, effective_spread);

                state.oracle_failures.store(0, std::sync::atomic::Ordering::Relaxed);
                last_sent_price = oracle_price;
                last_sent_at    = Instant::now();

                // Update PnL book
                {
                    let mut book = state.lp_book.write().await;
                    book.update(&snap, oracle_price, effective_spread,
                                prev_reserve_a, prev_reserve_b);
                }

                let book = state.lp_book.read().await;
                let mut m = state.metrics.write().await;
                m.oracle_cycles   = cycle;
                m.oracle_errors   = 0;
                m.last_spread_bps = effective_spread;
                m.daily_pnl_pct   = book.pnl_bps as f64 / 100.0; // bps → pct×100 display
            }
            Err(e) => {
                handle_failure(&state, &cfg, cycle, format!("{:#}", e)).await;
            }
        }
    }
}

async fn handle_failure(state: &SharedState, cfg: &Config, cycle: u64, msg: String) {
    let failures = state.oracle_failures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    error!("[oracle #{}] error ({}/{}): {}", cycle, failures, cfg.max_consecutive_failures, msg);
    {
        let mut m = state.metrics.write().await;
        m.oracle_errors = failures;
    }
    if failures >= cfg.max_consecutive_failures {
        state.halt(&format!("oracle arm: {} consecutive failures", failures));
    }
}

fn target_from_env_or_reserves(snap: &PoolSnapshot) -> (u64, u64) {
    let a = std::env::var("TARGET_A").ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(snap.reserve_a);
    let b = std::env::var("TARGET_B").ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(snap.reserve_b);
    (a, b)
}

// ── UPDATE_ORACLE instruction ─────────────────────────────────────────────────
//
// Data (41 bytes):
//   [0]       discriminant = 1
//   [1..9]    oracle_price  u64 le
//   [9..13]   spread_bps    u32 le
//   [13..17]  vol_adj       u32 le
//   [17..21]  k_param       u32 le
//   [21..25]  fee_bps       u32 le
//   [25..33]  target_a      u64 le
//   [33..41]  target_b      u64 le

fn build_update_oracle_ix(
    cfg:          &Config,
    oracle_price: u64,
    vol_adj:      u32,
    target_a:     u64,
    target_b:     u64,
) -> Instruction {
    let mut data = Vec::with_capacity(41);
    data.push(1u8);
    data.extend_from_slice(&oracle_price.to_le_bytes());
    data.extend_from_slice(&cfg.base_spread_bps.to_le_bytes());
    data.extend_from_slice(&vol_adj.to_le_bytes());
    data.extend_from_slice(&cfg.k_param.to_le_bytes());
    data.extend_from_slice(&cfg.fee_bps.to_le_bytes());
    data.extend_from_slice(&target_a.to_le_bytes());
    data.extend_from_slice(&target_b.to_le_bytes());

    Instruction {
        program_id: cfg.program_id,
        accounts: vec![
            AccountMeta::new(cfg.pool_pubkey, false),
            AccountMeta::new_readonly(cfg.wallet.pubkey(), true),
        ],
        data,
    }
}
