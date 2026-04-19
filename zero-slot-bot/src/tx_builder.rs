//! Transaction builder for the UPDATE_ORACLE instruction.
//!
//! ## Latency budget (what this module controls)
//!
//!   Signal (Hermes/LaserStream)          : 50–600ms  [not our problem here]
//!   ├─ getLatestBlockhash (cached 30s)   : 0ms  (cache hit) / 150ms (miss)
//!   ├─ getAccountInfo pool reserves      : 150–300ms  (parallel with above)
//!   ├─ sign + bincode                    : <1ms
//!   └─ sendTransaction                   : 200–500ms
//!   Total here                           : 200–800ms
//!
//! ## Blockhash cache
//!   Solana blockhashes are valid for 150 slots (~60s).  We cache for 30s so
//!   we never waste a round-trip on a redundant `getLatestBlockhash`.  The
//!   cache is an `Arc<BlockhashCache>` threaded in from main.
//!
//! ## Dynamic priority fee
//!   Priority fee scales with market volatility.  At low conf (stable market)
//!   we pay the minimum; at high conf we ramp up to max.  Formula:
//!     fee_per_cu = BASE_FEE + (MAX_FEE - BASE_FEE) * min(conf_bps, 500) / 500
//!   This means we spend more when the oracle update is most valuable (volatile
//!   market = biggest arb window to close).
//!
//! ## CU budget
//!   UPDATE_ORACLE on the Pinocchio PMM is ~3000–6000 actual CUs (no Anchor
//!   overhead).  We cap at 10_000 with a 60% safety margin.  Prior value of
//!   50_000 was paying 5–16x too much priority fee.
//!
//! ## Cost gate
//!   We only fire the transaction if the expected value of closing the arb
//!   window exceeds the transaction cost.  Expected value is approximated as:
//!     ev_lamports ≈ price_move_bps × reserve_b_lamports / 10_000
//!   If ev < tx_cost, we skip and wait for the next signal.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::json;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    pubkey,
    signature::Signer,
    transaction::Transaction,
};
use std::str::FromStr;
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::{signal::PriceSignal, Config};

// ── Well-known program IDs ────────────────────────────────────────────────────

const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";

// ── CU constants ─────────────────────────────────────────────────────────────

/// Tight CU limit for UPDATE_ORACLE.  Pinocchio programs are lean; measured
/// usage is 3000–6000 CUs.  10_000 gives 60% headroom without wasting fee.
const UPDATE_ORACLE_CU_LIMIT: u32 = 10_000;

// ── Dynamic fee thresholds ────────────────────────────────────────────────────

/// Minimum priority fee (micro-lamports per CU).  Used in calm markets.
/// 10_000 μL/CU × 10_000 CU = 100_000_000 μL = 100_000 lamports ≈ 0.0001 SOL (~$0.009)
const PRIORITY_FEE_BASE: u64  = 10_000;

/// Maximum priority fee. Used when conf_bps ≥ 500 (very volatile).
/// 100_000 μL/CU × 10_000 CU = 1_000_000_000 μL = 1_000_000 lamports ≈ 0.001 SOL (~$0.09)
const PRIORITY_FEE_MAX: u64   = 100_000;

/// conf_bps at which we hit PRIORITY_FEE_MAX.
const CONF_SCALE_CAP: u32 = 500;

// ── Pool state byte offsets (mirror of risk.rs) ───────────────────────────────

const OFF_RESERVE_A: usize = 48;
const OFF_RESERVE_B: usize = 56;

// ── Blockhash cache ───────────────────────────────────────────────────────────

/// Shared across all executor loop iterations.  Eliminates a 150ms RPC call
/// on every update when the market is printing fast (cache hit rate ~95%).
pub struct BlockhashCache {
    inner: Mutex<Option<(Hash, Instant)>>,
}

impl BlockhashCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(None) })
    }

    /// Return a cached blockhash if fresh, else fetch and cache a new one.
    /// TTL is 30s — well within the 60s Solana validity window.
    pub async fn get(
        &self,
        http: &reqwest::Client,
        rpc_url: &str,
    ) -> Result<Hash> {
        const TTL: Duration = Duration::from_secs(30);
        let mut guard = self.inner.lock().await;
        if let Some((hash, fetched_at)) = *guard {
            if fetched_at.elapsed() < TTL {
                debug!("blockhash cache hit ({:.1}s old)", fetched_at.elapsed().as_secs_f32());
                return Ok(hash);
            }
        }
        let hash = get_latest_blockhash(http, rpc_url).await?;
        *guard = Some((hash, Instant::now()));
        debug!("blockhash cache refreshed");
        Ok(hash)
    }

    /// Force-expire the cache (call after tx confirmation failure, so we don't
    /// retry with a potentially stale blockhash).
    pub async fn invalidate(&self) {
        *self.inner.lock().await = None;
    }
}

// ── Public interface ──────────────────────────────────────────────────────────

/// Compute the priority fee for this signal's volatility.
///
/// Returns `fee_per_cu` in micro-lamports.  Scales linearly from
/// `PRIORITY_FEE_BASE` (conf=0) to `PRIORITY_FEE_MAX` (conf≥500bps).
pub fn dynamic_priority_fee(conf_bps: u32) -> u64 {
    let factor = conf_bps.min(CONF_SCALE_CAP) as u64;
    PRIORITY_FEE_BASE + (PRIORITY_FEE_MAX - PRIORITY_FEE_BASE) * factor / CONF_SCALE_CAP as u64
}

/// Return the total transaction cost in lamports for a given priority fee.
pub fn tx_cost_lamports(fee_per_cu: u64) -> u64 {
    // base fee (5000 lamports) + priority fee
    5_000 + fee_per_cu * UPDATE_ORACLE_CU_LIMIT as u64 / 1_000
}

/// Build, sign, and base64-encode an UPDATE_ORACLE transaction.
///
/// Uses the blockhash cache for the blockhash fetch, and runs pool reserve
/// fetch in parallel.  Applies dynamic priority fee based on signal confidence.
pub async fn build_update_oracle_tx(
    cfg: &Arc<Config>,
    signal: &PriceSignal,
    http: &reqwest::Client,
    bh_cache: &Arc<BlockhashCache>,
) -> Result<String> {
    let rpc_url = if cfg.rpc_url.contains("helius-rpc.com") {
        format!("{}?api-key={}", cfg.rpc_url.trim_end_matches('/'), cfg.helius_api_key)
    } else {
        cfg.rpc_url.clone()
    };

    // ── Parallel: cached blockhash + pool reserves ────────────────────────────
    let (bh_res, pool_res) = tokio::join!(
        bh_cache.get(http, &rpc_url),
        get_pool_reserves(http, &rpc_url, &cfg.pool_pubkey),
    );

    let blockhash = bh_res.context("get blockhash")?;
    let (reserve_a, reserve_b) = pool_res.context("get pool reserves")?;

    // ── Dynamic priority fee ──────────────────────────────────────────────────
    let fee_per_cu = if cfg.priority_fee_micro_lamports > 0 {
        // Explicit override in config takes precedence
        cfg.priority_fee_micro_lamports
    } else {
        dynamic_priority_fee(signal.conf_bps)
    };

    let total_cost = tx_cost_lamports(fee_per_cu);

    debug!(
        "tx: blockhash={:.16} reserve_a={} reserve_b={} fee_per_cu={} cost={}L",
        blockhash, reserve_a, reserve_b, fee_per_cu, total_cost
    );

    // ── Compute targets ───────────────────────────────────────────────────────
    let target_a = reserve_a;
    let target_b = reserve_b;

    // ── vol_adj ───────────────────────────────────────────────────────────────
    let max_vol = cfg.max_spread_bps.saturating_sub(cfg.base_spread_bps as u32);
    let vol_adj = (signal.conf_bps).min(cfg.max_vol_factor).min(max_vol);

    // ── Build instructions ────────────────────────────────────────────────────
    let mut instructions: Vec<Instruction> = Vec::with_capacity(4);

    // 1. SetComputeUnitLimit — tight, saves priority fee
    instructions.push(set_compute_unit_limit(UPDATE_ORACLE_CU_LIMIT));

    // 2. SetComputeUnitPrice — dynamic, proportional to volatility
    instructions.push(set_compute_unit_price(fee_per_cu));

    // 3. Jito tip (mainnet only; skip if 0)
    if cfg.jito_tip_lamports > 0 {
        instructions.push(sol_transfer(
            &cfg.wallet.pubkey(),
            &cfg.jito_tip_account,
            cfg.jito_tip_lamports,
        ));
    }

    // 4. UPDATE_ORACLE
    instructions.push(update_oracle_ix(cfg, signal.price_1e9, vol_adj, target_a, target_b));

    // ── Sign and serialise ────────────────────────────────────────────────────
    let message = Message::new_with_blockhash(
        &instructions,
        Some(&cfg.wallet.pubkey()),
        &blockhash,
    );

    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[cfg.wallet.as_ref()], blockhash);

    let serialised = bincode::serialize(&tx).context("bincode serialise")?;

    info!(
        "tx built: CU={} fee={}μL/CU cost={}L vol_adj={} price={}",
        UPDATE_ORACLE_CU_LIMIT, fee_per_cu, total_cost, vol_adj, signal.price_1e9
    );

    Ok(B64.encode(&serialised))
}

// ── Instruction constructors ──────────────────────────────────────────────────

fn set_compute_unit_limit(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: Pubkey::from_str(COMPUTE_BUDGET_PROGRAM).unwrap(),
        accounts: vec![],
        data,
    }
}

fn set_compute_unit_price(micro_lamports: u64) -> Instruction {
    let mut data = vec![3u8];
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program_id: Pubkey::from_str(COMPUTE_BUDGET_PROGRAM).unwrap(),
        accounts: vec![],
        data,
    }
}

fn sol_transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = 2u32.to_le_bytes().to_vec();
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: pubkey!("11111111111111111111111111111111"),
        accounts: vec![
            AccountMeta::new(*from, true),
            AccountMeta::new(*to, false),
        ],
        data,
    }
}

/// UPDATE_ORACLE instruction (discriminant=1, 41 bytes total).
fn update_oracle_ix(
    cfg: &Config,
    oracle_price: u64,
    vol_adj: u32,
    target_a: u64,
    target_b: u64,
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
    debug_assert_eq!(data.len(), 41);

    Instruction {
        program_id: cfg.program_id,
        accounts: vec![
            AccountMeta::new(cfg.pool_pubkey, false),
            AccountMeta::new_readonly(cfg.wallet.pubkey(), true),
        ],
        data,
    }
}

// ── RPC helpers ───────────────────────────────────────────────────────────────

async fn get_latest_blockhash(http: &reqwest::Client, rpc_url: &str) -> Result<Hash> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getLatestBlockhash",
        "params": [{ "commitment": "confirmed" }]
    });

    let resp: serde_json::Value = http
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .context("getLatestBlockhash HTTP")?
        .json()
        .await
        .context("getLatestBlockhash JSON")?;

    if let Some(err) = resp.get("error") {
        bail!("getLatestBlockhash error: {}", err);
    }

    let hash_str = resp["result"]["value"]["blockhash"]
        .as_str()
        .context("blockhash missing in response")?;

    Hash::from_str(hash_str).context("parse blockhash")
}

async fn get_pool_reserves(
    http: &reqwest::Client,
    rpc_url: &str,
    pool_pubkey: &Pubkey,
) -> Result<(u64, u64)> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [
            pool_pubkey.to_string(),
            { "encoding": "base64", "commitment": "confirmed" }
        ]
    });

    let resp: serde_json::Value = http
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .context("getAccountInfo HTTP")?
        .json()
        .await
        .context("getAccountInfo JSON")?;

    if let Some(err) = resp.get("error") {
        bail!("getAccountInfo error: {}", err);
    }

    let encoded = resp["result"]["value"]["data"][0]
        .as_str()
        .context("pool account data missing")?;

    let data = B64.decode(encoded).context("pool data base64 decode")?;

    if data.len() < OFF_RESERVE_B + 8 {
        bail!(
            "pool account data too short: {} bytes (need >= {})",
            data.len(),
            OFF_RESERVE_B + 8
        );
    }

    let reserve_a =
        u64::from_le_bytes(data[OFF_RESERVE_A..OFF_RESERVE_A + 8].try_into().unwrap());
    let reserve_b =
        u64::from_le_bytes(data[OFF_RESERVE_B..OFF_RESERVE_B + 8].try_into().unwrap());

    Ok((reserve_a, reserve_b))
}
