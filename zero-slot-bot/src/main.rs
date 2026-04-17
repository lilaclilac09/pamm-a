//! Zero-slot execution bot
//!
//! Implements the exact flow from the diagram:
//!
//!   ┌─────────────────────────────────────────────────────────┐
//!   │  1. Detect Signals   — LaserStream gRPC subscription    │
//!   │     Watch on-chain accounts/txns at shred-level speed   │
//!   └──────────────────────────┬──────────────────────────────┘
//!                              │ signal (account change / tx)
//!   ┌──────────────────────────▼──────────────────────────────┐
//!   │  2. Prepare Transaction  — compute budget + tip         │
//!   │     SetComputeUnitPrice + SetComputeUnitLimit            │
//!   │     Jito tip instruction (if using MEV path)            │
//!   └──────────────────────────┬──────────────────────────────┘
//!                              │ signed tx (base64)
//!   ┌──────────────────────────▼──────────────────────────────┐
//!   │  3. Submit to Helius Sender                             │
//!   │     POST /fast  — fans out simultaneously to:           │
//!   │      ├── Staked Connections (SWQoS, ~30ms)              │
//!   │      └── Jito Block Engine (MEV auction)                │
//!   └─────────────────────────────────────────────────────────┘
//!
//! Signal: LaserStream watches the Pyth SOL/USD price account.
//! Action: When price moves > MIN_UPDATE_BPS, fire UPDATE_ORACLE on
//!         the PMM pool — landing before the next slot closes.
//!
//! Env vars (copy .env.example → .env):
//!   HELIUS_API_KEY, POOL_PUBKEY, POOL_AUTH, PROGRAM_ID, PRIVATE_KEY
//!   LASERSTREAM_ENDPOINT, SENDER_ENDPOINT
//!   PYTH_SOL_USD_ACCOUNT, MIN_UPDATE_BPS

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
use tracing::{error, info, warn};

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

    /// Minimum price move (in bps) before firing an oracle update.
    pub min_update_bps:        u64,

    /// PMM pool parameters (must match on-chain deployment).
    pub base_spread_bps:       u32,
    pub max_spread_bps:        u32,   // on-chain MAX_SPREAD_BPS = 2000
    pub k_param:               u32,
    pub fee_bps:               u32,
    pub max_vol_factor:        u32,   // cap on vol_adj

    /// Transaction priority & MEV.
    pub priority_fee_micro_lamports: u64,
    pub compute_units:         u32,
    pub jito_tip_lamports:     u64,
    pub jito_tip_account:      Pubkey,

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

            priority_fee_micro_lamports: u64_env("PRIORITY_FEE_MICRO_LAMPORTS", 100_000),
            compute_units:              u32_env("COMPUTE_UNITS", 50_000),
            jito_tip_lamports:          u64_env("JITO_TIP_LAMPORTS", 10_000),
            jito_tip_account: Pubkey::from_str(
                &std::env::var("JITO_TIP_ACCOUNT")
                    .unwrap_or_else(|_| "96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b".into())
            )?,
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

    info!("Zero-slot bot starting");
    info!("  Wallet:    {}", cfg.wallet.pubkey());
    info!("  Pool:      {}", cfg.pool_pubkey);
    info!("  Signal:    Pyth {} (min {}bps move)", cfg.pyth_account, cfg.min_update_bps);
    info!("  LaserStream: {}", cfg.laserstream_endpoint);
    info!("  Sender:      {}", cfg.sender_endpoint);

    // Channel: signal detector → executor (bounded so we never queue stale signals)
    let (sig_tx, mut sig_rx) = mpsc::channel::<signal::PriceSignal>(4);

    // ── Spawn LaserStream signal detector ─────────────────────────────────────
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
    let mut tx_count: u64 = 0;

    while let Some(sig) = sig_rx.recv().await {
        // Drain stale signals — keep only the newest
        let mut latest = sig;
        while let Ok(newer) = sig_rx.try_recv() {
            warn!("dropped stale signal (price={})", latest.price_1e9);
            latest = newer;
        }

        // Skip if move is below threshold
        if last_sent_price > 0 {
            let move_bps = latest.price_1e9.abs_diff(last_sent_price)
                .saturating_mul(10_000) / last_sent_price;
            if move_bps < cfg.min_update_bps {
                continue;
            }
        }

        let t0 = Instant::now();
        tx_count += 1;

        info!(
            "[#{tx_count}] signal: price={} conf={}bps slot={} — building tx",
            latest.price_1e9, latest.conf_bps, latest.slot
        );

        // ── Build UPDATE_ORACLE transaction ───────────────────────────────────
        let tx_b64 = match tx_builder::build_update_oracle_tx(&cfg, &latest, &http).await {
            Ok(b)  => b,
            Err(e) => { error!("tx build failed: {:#}", e); continue; }
        };

        // ── Submit via Helius Sender (staked connections + Jito fanout) ───────
        match sender::submit(&cfg, &http, &tx_b64).await {
            Ok(sig) => {
                let elapsed_ms = t0.elapsed().as_millis();
                info!(
                    "[#{tx_count}] submitted sig={:.16}… in {}ms  price={}",
                    sig, elapsed_ms, latest.price_1e9
                );
                last_sent_price = latest.price_1e9;
            }
            Err(e) => error!("[#{tx_count}] sender error: {:#}", e),
        }
    }

    Ok(())
}
