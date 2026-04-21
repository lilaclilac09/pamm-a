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
    jito, rpc,
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

    async fn refresh(&mut self, http: &reqwest::Client, url: &str) -> Result<()> {
        let bh = rpc::get_latest_blockhash(http, url).await.context("get_latest_blockhash")?;
        self.hash       = Some(bh);
        self.fetched_at = Some(Instant::now());
        Ok(())
    }

    fn get(&self) -> Result<Hash> {
        self.hash.ok_or_else(|| anyhow::anyhow!("blockhash not yet fetched"))
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(
    cfg:        Arc<Config>,
    state:      Arc<SharedState>,
    mut pool_rx: tokio::sync::watch::Receiver<Option<PoolSnapshot>>,
) {
    let http = crate::rpc::make_client();

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
        // Clamp so spread_bps + vol_adj never exceeds on-chain MAX_SPREAD_BPS (2000)
        const MAX_SPREAD_BPS: u32 = 2000;
        let max_vol = MAX_SPREAD_BPS.saturating_sub(cfg.base_spread_bps);
        let vol_adj = (conf_bps as u32).max(hist_vol).min(cfg.max_vol_factor).min(max_vol);

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
        // Priority: LaserStream > RPC > cached state.pool (stale but usable).
        let prev_reserve_a = state.pool.read().await.reserve_a;
        let prev_reserve_b = state.pool.read().await.reserve_b;

        let streamed_snap: Option<PoolSnapshot> = pool_rx.borrow().clone();
        let snap: PoolSnapshot = if let Some(s) = streamed_snap {
            s
        } else {
            // Try RPC; on failure reuse last known pool state so the oracle
            // price update still lands (stale reserves just means targets are
            // slightly wrong, far better than skipping the price update entirely).
            let pool_result = rpc::get_account_data(&http, &cfg.rpc_url, &cfg.pool_pubkey).await;
            if let Err(ref e) = pool_result {
                warn!("[oracle #{}] pool fetch error: {:#}", cycle, e);
            }
            match pool_result.ok().and_then(|d| PoolSnapshot::from_bytes(&d))
            {
                Some(s) => s,
                None => {
                    let cached = state.pool.read().await.clone();
                    if cached.oracle_price == 0 {
                        // Truly nothing yet — can't send without any pool context
                        handle_failure(&state, &cfg, cycle, "pool fetch failed, no cached state".into()).await;
                        continue;
                    }
                    warn!("[oracle #{}] pool RPC failed — using cached state", cycle);
                    cached
                }
            }
        };
        *state.pool.write().await = snap.clone();
        let (target_a, target_b) = target_from_env_or_reserves(&snap);

        // ── Build + send ──────────────────────────────────────────────────────
        if bh.needs_refresh() {
            if let Err(e) = bh.refresh(&http, &cfg.rpc_url).await {
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
        match jito::send_with_tip(
            &http, &cfg.rpc_url, &[ix], &cfg.wallet, blockhash,
            &cfg.jito_endpoint, cfg.jito_tip_lamports, cfg.priority_fee_micro_lamports,
        ).await.context("UPDATE_ORACLE send")
        {
            Ok(sig) => {
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
    } else {
        // Exponential backoff: 1s, 2s, 4s, … up to 30s.
        // Prevents hammering the RPC during transient outages.
        let backoff_secs = (1u64 << (failures - 1).min(5)).min(30);
        sleep(Duration::from_secs(backoff_secs)).await;
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
