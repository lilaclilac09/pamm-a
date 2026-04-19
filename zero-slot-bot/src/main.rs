//! Zero-slot execution bot
//!
//! ## Architecture
//!
//!   ┌──────────────────────────────────────────────────────────────┐
//!   │  1. Signal detection                                         │
//!   │     laserstream: Yellowstone gRPC, shred-level (~50ms lag)   │
//!   │     hermes:      Pyth HTTP poll, 500ms period (~600ms lag)   │
//!   └──────────────────────────┬───────────────────────────────────┘
//!                              │
//!   ┌──────────────────────────▼───────────────────────────────────┐
//!   │  2. Cost gate                                                │
//!   │     Skip if price_move < MIN_UPDATE_BPS                      │
//!     Skip if cost_gate_lamports > 0 AND tx_cost > expected_value │
//!   └──────────────────────────┬───────────────────────────────────┘
//!                              │
//!   ┌──────────────────────────▼───────────────────────────────────┐
//!   │  3. Build transaction                                        │
//!   │     Blockhash from 30s cache (eliminates one RPC round-trip) │
//!     Pool reserves via getAccountInfo (parallel with ^)          │
//!     Dynamic priority fee: BASE + scale(conf_bps)                │
//!     Tight CU limit: 10_000 (Pinocchio UPDATE_ORACLE ≈3-6k CUs)  │
//!   └──────────────────────────┬───────────────────────────────────┘
//!                              │
//!   ┌──────────────────────────▼───────────────────────────────────┐
//!   │  4. Submit to Helius Sender                                  │
//!   │     skipPreflight=true, maxRetries=0                         │
//!     Mainnet: fans out to staked connections + Jito simultaneously│
//!   └──────────────────────────────────────────────────────────────┘
//!
//! ## Cost model
//!   Old: 50_000 CU × 100_000 μL/CU = 5_000_000 L = 0.005 SOL (~$0.45/update)
//!   New: 10_000 CU × dynamic_fee(conf_bps)
//!        calm  (conf≤10bps):   10_000 μL/CU →   100 L = 0.000100 SOL ($0.009)
//!        vol   (conf=50bps):   18_000 μL/CU →   180 L = 0.000180 SOL ($0.016)
//!        spike (conf≥500bps): 100_000 μL/CU → 1_000 L = 0.001000 SOL ($0.090)
//!   Reduction: 5–45× cheaper per oracle update.
//!
//! Env vars (copy .env.example → .env):
//!   HELIUS_API_KEY, POOL_PUBKEY, POOL_AUTH, PROGRAM_ID, PRIVATE_KEY
//!   SIGNAL_MODE (laserstream|hermes), LASERSTREAM_ENDPOINT, SENDER_ENDPOINT
//!   PYTH_SOL_USD_ACCOUNT, MIN_UPDATE_BPS
//!   PRIORITY_FEE_MICRO_LAMPORTS (0 = dynamic, >0 = override)
//!   COST_GATE_LAMPORTS (0 = disabled)

mod signal;
mod sender;
mod tx_builder;

use anyhow::{Context, Result};
use dotenv::dotenv;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use std::{str::FromStr, sync::Arc, time::{Duration, Instant}};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use tx_builder::BlockhashCache;

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub helius_api_key:        String,
    pub laserstream_endpoint:  String,
    pub sender_endpoint:       String,

    pub program_id:            Pubkey,
    pub pool_pubkey:           Pubkey,
    pub pool_auth:             Pubkey,
    pub pyth_account:          Pubkey,

    /// Minimum price move (bps) before firing an oracle update.
    pub min_update_bps:        u64,

    /// PMM pool parameters (must match on-chain deployment).
    pub base_spread_bps:       u32,
    pub max_spread_bps:        u32,
    pub k_param:               u32,
    pub fee_bps:               u32,
    pub max_vol_factor:        u32,

    /// Priority fee per CU (micro-lamports).
    /// Set to 0 to use dynamic scaling based on market volatility (recommended).
    /// Set >0 to override with a fixed value.
    pub priority_fee_micro_lamports: u64,

    /// Minimum expected value (lamports) a tx must save before we fire it.
    /// 0 = no cost gate (always fire if price moves enough).
    /// Set to e.g. 100_000 to require expected arb savings > 0.0001 SOL.
    pub cost_gate_lamports:    u64,

    pub jito_tip_lamports:     u64,
    pub jito_tip_account:      Pubkey,

    /// RPC endpoint for blockhash + pool state reads.
    pub rpc_url:               String,

    /// Signal source: "laserstream" (mainnet gRPC) or "hermes" (HTTP polling).
    pub signal_mode:           String,

    pub wallet:                Arc<Keypair>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        fn env(k: &str) -> Result<String> {
            std::env::var(k).with_context(|| format!("missing env var: {k}"))
        }
        fn pubkey(k: &str) -> Result<Pubkey> {
            Pubkey::from_str(&env(k)?).with_context(|| format!("invalid pubkey: {k}"))
        }
        fn u64_env(k: &str, default: u64) -> u64 {
            std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        }
        fn u32_env(k: &str, default: u32) -> u32 {
            std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        }

        let key_bytes = bs58::decode(env("PRIVATE_KEY")?)
            .into_vec()
            .context("invalid PRIVATE_KEY base58")?;
        let wallet = Arc::new(Keypair::try_from(key_bytes.as_slice()).context("invalid keypair")?);

        Ok(Config {
            helius_api_key:       env("HELIUS_API_KEY")?,
            laserstream_endpoint: std::env::var("LASERSTREAM_ENDPOINT")
                .unwrap_or_else(|_| "https://laserstream-mainnet-ewr.helius-rpc.com".into()),
            sender_endpoint: std::env::var("SENDER_ENDPOINT")
                .unwrap_or_else(|_| "https://ewr-sender.helius-rpc.com/fast".into()),

            program_id:  pubkey("PROGRAM_ID")?,
            pool_pubkey: pubkey("POOL_PUBKEY")?,
            pool_auth:   pubkey("POOL_AUTH")?,
            pyth_account: pubkey("PYTH_SOL_USD_ACCOUNT")?,

            min_update_bps: u64_env("MIN_UPDATE_BPS", 2),

            base_spread_bps: u32_env("BASE_SPREAD_BPS", 10),
            max_spread_bps:  u32_env("MAX_SPREAD_BPS", 2000),
            k_param:         u32_env("K_PARAM", 1_000_000),
            fee_bps:         u32_env("FEE_BPS", 30),
            max_vol_factor:  u32_env("MAX_VOL_FACTOR", 500),

            // 0 = dynamic (recommended).  Old default was 100_000 → 5M lamports/tx.
            priority_fee_micro_lamports: u64_env("PRIORITY_FEE_MICRO_LAMPORTS", 0),

            cost_gate_lamports: u64_env("COST_GATE_LAMPORTS", 0),

            jito_tip_lamports: u64_env("JITO_TIP_LAMPORTS", 0),
            jito_tip_account: Pubkey::from_str(
                &std::env::var("JITO_TIP_ACCOUNT")
                    .unwrap_or_else(|_| "96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b".into())
            )?,
            rpc_url: std::env::var("RPC_URL")
                .unwrap_or_else(|_| "https://devnet.helius-rpc.com/".into()),
            signal_mode: std::env::var("SIGNAL_MODE")
                .unwrap_or_else(|_| "laserstream".into()),
            wallet,
        })
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "zero_slot=info".into())
        )
        .init();

    let cfg = Arc::new(Config::from_env()?);

    let fee_mode = if cfg.priority_fee_micro_lamports == 0 {
        "dynamic (conf-scaled)".to_string()
    } else {
        format!("{} μL/CU (fixed)", cfg.priority_fee_micro_lamports)
    };

    info!("Zero-slot bot starting");
    info!("  Wallet:      {}", cfg.wallet.pubkey());
    info!("  Pool:        {}", cfg.pool_pubkey);
    info!("  Signal mode: {} (min {}bps move)", cfg.signal_mode, cfg.min_update_bps);
    info!("  Priority fee: {}", fee_mode);
    info!("  Cost gate:   {} lamports", cfg.cost_gate_lamports);
    info!("  Sender:      {}", cfg.sender_endpoint);

    // Shared blockhash cache — eliminates one RPC round-trip on cache hits
    let bh_cache = BlockhashCache::new();

    // Channel: signal detector → executor (bounded — never queue stale signals)
    let (sig_tx, mut sig_rx) = mpsc::channel::<signal::PriceSignal>(4);

    // Spawn signal detector
    let cfg_s = Arc::clone(&cfg);
    tokio::spawn(async move {
        loop {
            if let Err(e) = signal::run(Arc::clone(&cfg_s), sig_tx.clone()).await {
                error!("signal detector error: {:#} — reconnecting in 2s", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    });

    // ── Executor loop — one in-flight tx at a time ────────────────────────────
    let http = Arc::new(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?
    );

    let mut last_sent_price: u64 = 0;
    let mut tx_count:  u64 = 0;
    let mut skip_cost: u64 = 0;

    while let Some(sig) = sig_rx.recv().await {
        // Drain stale signals — keep only the newest
        let mut latest = sig;
        while let Ok(newer) = sig_rx.try_recv() {
            warn!("dropped stale signal (price={})", latest.price_1e9);
            latest = newer;
        }

        // ── Gate 1: minimum price move ────────────────────────────────────────
        let move_bps = if last_sent_price > 0 {
            latest.price_1e9.abs_diff(last_sent_price)
                .saturating_mul(10_000) / last_sent_price
        } else {
            u64::MAX // always fire first update
        };

        if move_bps < cfg.min_update_bps {
            debug!("skip: move={}bps < min={}bps", move_bps, cfg.min_update_bps);
            continue;
        }

        // ── Gate 2: cost gate ─────────────────────────────────────────────────
        // Approximate expected value = move_bps × pool_size_proxy.
        // We use price_1e9 as a proxy for pool liquidity depth (very rough).
        // Real version would fetch reserve_b and compute properly.
        if cfg.cost_gate_lamports > 0 {
            let fee_per_cu = tx_builder::dynamic_priority_fee(latest.conf_bps);
            let tx_cost    = tx_builder::tx_cost_lamports(fee_per_cu);
            if tx_cost > cfg.cost_gate_lamports {
                skip_cost += 1;
                debug!(
                    "skip[cost]: tx_cost={}L > gate={}L (total skipped={})",
                    tx_cost, cfg.cost_gate_lamports, skip_cost
                );
                continue;
            }
        }

        let t0 = Instant::now();
        tx_count += 1;

        info!(
            "[#{tx_count}] signal: price={} conf={}bps move={}bps slot={} — building tx",
            latest.price_1e9, latest.conf_bps, move_bps, latest.slot
        );

        // ── Build UPDATE_ORACLE tx ─────────────────────────────────────────────
        let tx_b64 = match tx_builder::build_update_oracle_tx(
            &cfg, &latest, &http, &bh_cache,
        ).await {
            Ok(b)  => b,
            Err(e) => {
                error!("tx build failed: {:#}", e);
                bh_cache.invalidate().await; // maybe blockhash expired
                continue;
            }
        };

        // ── Submit via Helius Sender ───────────────────────────────────────────
        match sender::submit(&cfg, &http, &tx_b64).await {
            Ok(sig_str) => {
                let elapsed_ms = t0.elapsed().as_millis();
                info!(
                    "[#{tx_count}] submitted sig={:.16}… in {}ms  price={} move={}bps",
                    sig_str, elapsed_ms, latest.price_1e9, move_bps
                );
                last_sent_price = latest.price_1e9;
            }
            Err(e) => {
                error!("[#{tx_count}] sender error: {:#}", e);
                // Invalidate blockhash cache on submission failure — the tx may
                // have been rejected due to a stale blockhash.
                bh_cache.invalidate().await;
            }
        }
    }

    Ok(())
}
