//! Transaction builder for the UPDATE_ORACLE instruction.
//!
//! To minimise latency we fire two JSON-RPC calls concurrently (tokio::join!):
//!   1. `getLatestBlockhash`  — required to sign the transaction
//!   2. `getAccountInfo`      — reads pool reserves for target_a / target_b
//!
//! Then we assemble:
//!   [SetComputeUnitLimit]  ← keeps CU budget tight so validators prioritise us
//!   [SetComputeUnitPrice]  ← priority fee (micro-lamports per CU)
//!   [Jito tip transfer]    ← SOL tip to jito_tip_account (skip if 0)
//!   [UPDATE_ORACLE]        ← discriminant=1, 41-byte payload
//!
//! The resulting Transaction is bincode-serialised and base64-encoded for the
//! Helius Sender endpoint.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::json;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::Signer,
    pubkey,
    transaction::Transaction,
};
use std::str::FromStr;
use tracing::debug;

use crate::{signal::PriceSignal, Config};

// ── Well-known program IDs ────────────────────────────────────────────────────

const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";

// ── Pool state byte offsets (mirror of risk.rs in the main bot) ───────────────

const OFF_RESERVE_A: usize = 48;
const OFF_RESERVE_B: usize = 56;

// ── Public interface ──────────────────────────────────────────────────────────

/// Build, sign, and base64-encode an UPDATE_ORACLE transaction.
///
/// Makes two parallel RPC calls for the blockhash and pool state, then
/// constructs the full instruction set.
pub async fn build_update_oracle_tx(
    cfg: &Arc<Config>,
    signal: &PriceSignal,
    http: &reqwest::Client,
) -> Result<String> {
    // ── Parallel RPC fetches ──────────────────────────────────────────────────
    let rpc_url = format!(
        "https://devnet.helius-rpc.com/?api-key={}",
        cfg.helius_api_key
    );

    let (bh_res, pool_res) = tokio::join!(
        get_latest_blockhash(http, &rpc_url),
        get_pool_reserves(http, &rpc_url, &cfg.pool_pubkey),
    );

    let blockhash = bh_res.context("get blockhash")?;
    let (reserve_a, reserve_b) = pool_res.context("get pool reserves")?;

    debug!(
        "tx: blockhash={:.16} reserve_a={} reserve_b={}",
        blockhash, reserve_a, reserve_b
    );

    // ── Compute targets (use current reserves as equilibrium point) ───────────
    let target_a = reserve_a;
    let target_b = reserve_b;

    // ── vol_adj: use Pyth confidence as proxy for volatility (capped) ─────────
    let max_vol = cfg
        .max_spread_bps
        .saturating_sub(cfg.base_spread_bps as u32);
    let vol_adj = (signal.conf_bps).min(cfg.max_vol_factor).min(max_vol);

    // ── Build instructions ────────────────────────────────────────────────────
    let mut instructions: Vec<Instruction> = Vec::with_capacity(4);

    // 1. SetComputeUnitLimit
    instructions.push(set_compute_unit_limit(cfg.compute_units));

    // 2. SetComputeUnitPrice
    instructions.push(set_compute_unit_price(cfg.priority_fee_micro_lamports));

    // 3. Jito tip (skip on devnet / if tip is 0)
    if cfg.jito_tip_lamports > 0 {
        instructions.push(sol_transfer(
            &cfg.wallet.pubkey(),
            &cfg.jito_tip_account,
            cfg.jito_tip_lamports,
        ));
    }

    // 4. UPDATE_ORACLE
    instructions.push(update_oracle_ix(
        cfg,
        signal.price_1e9,
        vol_adj,
        target_a,
        target_b,
    ));

    // ── Sign and serialise ────────────────────────────────────────────────────
    let message = Message::new_with_blockhash(
        &instructions,
        Some(&cfg.wallet.pubkey()),
        &blockhash,
    );

    let mut tx = Transaction::new_unsigned(message);
    tx.sign(&[cfg.wallet.as_ref()], blockhash);

    let serialised = bincode::serialize(&tx).context("bincode serialise")?;
    Ok(B64.encode(&serialised))
}

// ── Instruction constructors ──────────────────────────────────────────────────

/// ComputeBudget: SetComputeUnitLimit  (discriminant = 2, payload = u32 LE)
fn set_compute_unit_limit(units: u32) -> Instruction {
    let mut data = vec![2u8]; // discriminant
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id: Pubkey::from_str(COMPUTE_BUDGET_PROGRAM).unwrap(),
        accounts: vec![],
        data,
    }
}

/// ComputeBudget: SetComputeUnitPrice  (discriminant = 3, payload = u64 LE)
fn set_compute_unit_price(micro_lamports: u64) -> Instruction {
    let mut data = vec![3u8]; // discriminant
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program_id: Pubkey::from_str(COMPUTE_BUDGET_PROGRAM).unwrap(),
        accounts: vec![],
        data,
    }
}

/// System program Transfer — used for Jito tips.
fn sol_transfer(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    // System instruction discriminant 2 = Transfer, payload = u64 LE lamports
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

/// UPDATE_ORACLE instruction (discriminant = 1, 41 bytes total).
///
/// Data layout:
///   [0]     discriminant = 1
///   [1..9]  oracle_price  u64 LE
///   [9..13]  spread_bps   u32 LE
///   [13..17] vol_adj      u32 LE
///   [17..21] k_param      u32 LE
///   [21..25] fee_bps      u32 LE
///   [25..33] target_a     u64 LE
///   [33..41] target_b     u64 LE
///
/// Accounts: [pool (writable), authority (signer)]
fn update_oracle_ix(
    cfg: &Config,
    oracle_price: u64,
    vol_adj: u32,
    target_a: u64,
    target_b: u64,
) -> Instruction {
    let mut data = Vec::with_capacity(41);
    data.push(1u8);                                          // discriminant
    data.extend_from_slice(&oracle_price.to_le_bytes());    // u64
    data.extend_from_slice(&cfg.base_spread_bps.to_le_bytes()); // u32
    data.extend_from_slice(&vol_adj.to_le_bytes());         // u32
    data.extend_from_slice(&cfg.k_param.to_le_bytes());     // u32
    data.extend_from_slice(&cfg.fee_bps.to_le_bytes());     // u32
    data.extend_from_slice(&target_a.to_le_bytes());        // u64
    data.extend_from_slice(&target_b.to_le_bytes());        // u64
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

/// JSON-RPC `getLatestBlockhash`.
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

/// JSON-RPC `getAccountInfo` to read pool reserves at the known offsets.
///
/// Returns `(reserve_a, reserve_b)` as raw token amounts.
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
