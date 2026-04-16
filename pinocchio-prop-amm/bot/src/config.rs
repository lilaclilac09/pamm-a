//! Unified bot configuration — loaded once from env at startup.

use anyhow::{Context, Result};
use solana_sdk::{pubkey::Pubkey, signature::Keypair};
use std::str::FromStr;

pub struct Config {
    // ── Network ──────────────────────────────────────────────────────────────
    pub rpc_url: String,
    pub wallet:  Keypair,

    // ── Program / pool accounts ───────────────────────────────────────────
    pub program_id:     Pubkey,
    pub pool_pubkey:    Pubkey,
    pub pool_auth:      Pubkey,
    pub vault_a:        Pubkey,
    pub vault_b:        Pubkey,
    pub lp_mint:        Pubkey,
    pub mint_a:         Pubkey,
    pub mint_b:         Pubkey,
    pub user_a:         Pubkey,  // user's token-A SPL account
    pub user_b:         Pubkey,  // user's token-B SPL account
    pub user_lp:        Pubkey,  // user's LP token account
    pub dead_lp_account: Pubkey,

    // ── Oracle arm ────────────────────────────────────────────────────────
    pub pyth_feed_id:      String,
    pub update_interval_ms: u64,
    pub base_spread_bps:   u32,
    pub k_param:           u32,
    pub fee_bps:           u32,
    pub max_vol_factor:    u32,
    /// Skip UPDATE_ORACLE if price moved less than this many bps since last send.
    /// 0 = always update. Default: 5 (0.05%). Cuts oracle tx count ~90% in calm markets.
    pub min_oracle_update_bps: u32,
    /// Force UPDATE_ORACLE even if price is stable, after this many seconds.
    /// Keeps the on-chain TWAP fresh. Default: 30s.
    pub max_oracle_silence_secs: u64,

    // ── Trading arm ──────────────────────────────────────────────────────
    pub trade_interval_ms:       u64,
    pub max_trade_lamports:      u64,  // max per Jupiter swap
    pub rebalance_high:          f64,  // e.g. 0.72 — remove liq above this ratio
    pub rebalance_low:           f64,  // e.g. 0.38 — add liq below this ratio
    pub jito_tip_lamports:       u64,
    pub jito_endpoint:           String,
    pub jito_tip_account:        Pubkey,
    pub max_price_deviation_bps: u32,  // TWAP sanity filter
    pub max_twap_age_secs:       i64,  // reject TWAP if newest obs is older than this
    pub slippage_bps:            u64,

    // ── Risk ─────────────────────────────────────────────────────────────
    pub max_consecutive_failures: u32,
    pub max_daily_loss_pct:       f64,  // e.g. 0.05 = 5%

    // ── Metrics ──────────────────────────────────────────────────────────
    pub metrics_port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenv::dotenv().ok();

        // Wallet
        let wallet = {
            let key = std::env::var("PRIVATE_KEY").context("PRIVATE_KEY not set")?;
            let bytes = bs58::decode(&key).into_vec().context("invalid base58 key")?;
            Keypair::from_bytes(&bytes).context("invalid keypair bytes")?
        };

        // Helper: required pubkey from env
        let p = |name: &str| -> Result<Pubkey> {
            Pubkey::from_str(
                &std::env::var(name).context(format!("{name} not set"))?,
            )
            .context(format!("invalid pubkey for {name}"))
        };
        // Helper: optional pubkey with fallback
        let p_opt = |name: &str, fallback: &str| -> Pubkey {
            std::env::var(name)
                .ok()
                .and_then(|v| Pubkey::from_str(&v).ok())
                .unwrap_or_else(|| Pubkey::from_str(fallback).unwrap())
        };
        // Helper: u64 with default
        let u = |name: &str, default: u64| -> u64 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        // Helper: f64 with default
        let f = |name: &str, default: f64| -> f64 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };

        Ok(Config {
            rpc_url: std::env::var("RPC_URL")
                .unwrap_or_else(|_| "https://api.devnet.solana.com".into()),
            wallet,

            program_id:      p("PROGRAM_ID")?,
            pool_pubkey:     p("POOL_PUBKEY")?,
            pool_auth:       p("POOL_AUTH")?,
            vault_a:         p("VAULT_A")?,
            vault_b:         p("VAULT_B")?,
            lp_mint:         p("LP_MINT")?,
            mint_a:          p("MINT_A")?,
            mint_b:          p("MINT_B")?,
            user_a:          p("USER_A")?,
            user_b:          p("USER_B")?,
            user_lp:         p("USER_LP")?,
            dead_lp_account: p("DEAD_LP_ACCOUNT")?,

            pyth_feed_id: std::env::var("PYTH_FEED_ID").unwrap_or_else(|_| {
                "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d".into()
            }),
            update_interval_ms: u("UPDATE_INTERVAL_MS", 500),
            // BASE_SPREAD_BPS=0: spread = vol_adj only (= Pyth confidence + EWMA vol).
            // In calm SOL market this gives ~1-3bps effective spread.
            // Fee is separate and goes directly to LPs.
            base_spread_bps:    u("BASE_SPREAD_BPS", 0) as u32,
            k_param:            u("K_PARAM", 500) as u32,
            // 2bp fee: competitive with Uniswap v3 1bp tier but with oracle protection.
            fee_bps:            u("FEE_BPS", 2) as u32,
            max_vol_factor:          u("MAX_VOL_FACTOR", 200) as u32,
            // Update oracle on any 1bp move so the pool is always priced fresh.
            min_oracle_update_bps:   u("MIN_ORACLE_UPDATE_BPS", 1) as u32,
            // 10s TWAP freshness guarantee for trading arm.
            max_oracle_silence_secs: u("MAX_ORACLE_SILENCE_SECS", 10),

            trade_interval_ms:   u("TRADE_INTERVAL_MS", 5_000),
            max_trade_lamports:  u("MAX_TRADE_LAMPORTS", 100_000_000), // 0.1 SOL
            rebalance_high:      f("REBALANCE_HIGH", 0.72),
            rebalance_low:       f("REBALANCE_LOW", 0.38),
            jito_tip_lamports:   u("JITO_TIP_LAMPORTS", 10_000),
            jito_endpoint: std::env::var("JITO_ENDPOINT").unwrap_or_else(|_| {
                "https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles".into()
            }),
            jito_tip_account: p_opt(
                "JITO_TIP_ACCOUNT",
                "96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b",
            ),
            max_price_deviation_bps: u("MAX_PRICE_DEVIATION_BPS", 200) as u32,
            max_twap_age_secs:       u("MAX_TWAP_AGE_SECS", 10) as i64,
            slippage_bps:            u("SLIPPAGE_BPS", 80),

            max_consecutive_failures: u("MAX_CONSECUTIVE_FAILURES", 5) as u32,
            max_daily_loss_pct:       f("MAX_DAILY_LOSS_PCT", 0.05),

            metrics_port: u("METRICS_PORT", 3000) as u16,
        })
    }
}
