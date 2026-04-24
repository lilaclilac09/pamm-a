//! Trading arm — issues direct SWAP instructions against the PMM pool.
//!
//! Each cycle:
//!   1. Read pool snapshot (oracle price, reserves)
//!   2. Compute inventory ratio and pick direction: only trade when ratio is
//!      outside the rebalance band. A→B when overweight A, B→A when underweight.
//!   3. Build and send SWAP instruction directly to the PMM program
//!   4. Re-read pool reserves to measure the actual out_amount and record in MmBook

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
};
use std::{str::FromStr, sync::Arc, time::Instant};
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};

use crate::{
    config::Config,
    cu_budget::send_budgeted,
    risk::{SharedState, OFF_RESERVE_A, OFF_RESERVE_B},
};

const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

// ── Blockhash cache ───────────────────────────────────────────────────────────

struct BhCache { hash: Option<Hash>, fetched_at: Option<Instant> }

impl BhCache {
    fn new() -> Self { BhCache { hash: None, fetched_at: None } }
    fn stale(&self) -> bool {
        self.fetched_at.map_or(true, |t| t.elapsed().as_secs() >= 20)
    }
    fn refresh(&mut self, client: &RpcClient) -> Result<()> {
        let bh = client.get_latest_blockhash().context("get_latest_blockhash")?;
        self.hash = Some(bh);
        self.fetched_at = Some(Instant::now());
        Ok(())
    }
    fn get(&self) -> Result<Hash> {
        self.hash.ok_or_else(|| anyhow::anyhow!("blockhash not fetched"))
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(cfg: Arc<Config>, state: Arc<SharedState>) {
    let client = RpcClient::new(cfg.rpc_url.clone());
    let mut bh = BhCache::new();
    let mut cycle: u64 = 0;

    info!("trading_arm: starting (interval={}ms, size={}, band={:.2}-{:.2})",
        cfg.trade_interval_ms, cfg.max_trade_lamports,
        cfg.rebalance_low, cfg.rebalance_high);

    loop {
        if state.is_halted() {
            info!("trading_arm: halted — pausing");
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        cycle += 1;

        match run_cycle(&cfg, &client, &mut bh, &state).await {
            Ok(action) => {
                info!("[trade #{}] {}", cycle, action);
                state.trade_failures.store(0, std::sync::atomic::Ordering::Relaxed);

                let mut m = state.metrics.write().await;
                m.trade_cycles = cycle;
                m.trade_errors = 0;

                // Update inventory ratio in metrics
                let snap = state.pool.read().await.clone();
                if snap.oracle_price > 0 {
                    let value_a = snap.reserve_a as f64 * snap.oracle_price as f64 / 1e9;
                    let value_b = snap.reserve_b as f64;
                    let total   = value_a + value_b;
                    m.inventory_ratio = if total > 0.0 { value_a / total } else { 0.5 };
                }
            }
            Err(e) => {
                let failures = state.trade_failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                error!("[trade #{}] error ({}/{}): {:#}", cycle, failures,
                    cfg.max_consecutive_failures, e);

                let mut m = state.metrics.write().await;
                m.trade_errors = failures;
                m.trade_cycles = cycle;

                if failures >= cfg.max_consecutive_failures {
                    warn!("trading arm: {} consecutive failures — resetting counter", failures);
                    state.trade_failures.store(0, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        sleep(Duration::from_millis(cfg.trade_interval_ms)).await;
    }
}

// ── Cycle ─────────────────────────────────────────────────────────────────────

async fn run_cycle(
    cfg:       &Config,
    client:    &RpcClient,
    bh:        &mut BhCache,
    state:     &SharedState,
) -> Result<String> {
    let snap = state.pool.read().await.clone();
    if snap.oracle_price == 0 {
        return Ok("skip: oracle not yet updated".into());
    }
    if snap.reserve_a == 0 || snap.reserve_b == 0 {
        return Ok("skip: pool reserves empty".into());
    }

    // ── Inventory-driven direction ───────────────────────────────────────────
    // Value both reserves in B-units: value_a = reserve_a * oracle_price / 1e9.
    // Ratio = value_a / (value_a + value_b). Only trade when outside the band.
    let value_a = (snap.reserve_a as u128).saturating_mul(snap.oracle_price as u128) / 1_000_000_000u128;
    let value_b = snap.reserve_b as u128;
    let total   = value_a + value_b;
    if total == 0 {
        return Ok("skip: pool value zero".into());
    }
    let ratio = value_a as f64 / total as f64;

    let direction: u8 = if ratio > cfg.rebalance_high {
        0  // overweight A → sell A for B
    } else if ratio < cfg.rebalance_low {
        1  // underweight A → buy A with B
    } else {
        return Ok(format!("skip: inventory balanced (ratio={:.3} in [{:.2},{:.2}])",
            ratio, cfg.rebalance_low, cfg.rebalance_high));
    };

    // Pick trade size and compute expected output from oracle price
    // A→B: out_B = amount_a * 1e9 / oracle_price  (oracle_price is A-per-B scaled by 1e9)
    // B→A: out_A = amount_b * oracle_price / 1e9
    let (amount_in, expected_out) = if direction == 0 {
        let amount_a = cfg.max_trade_lamports;
        let exp_b = (amount_a as u128 * 1_000_000_000u128)
            .checked_div(snap.oracle_price as u128)
            .unwrap_or(0) as u64;
        (amount_a, exp_b)
    } else {
        let amount_b = cfg.max_trade_lamports;
        let exp_a = (amount_b as u128 * snap.oracle_price as u128)
            .checked_div(1_000_000_000u128)
            .unwrap_or(0) as u64;
        (amount_b, exp_a)
    };

    if amount_in == 0 {
        return Ok("skip: zero amount".into());
    }

    // Check we have enough tokens
    let user_in_bal = if direction == 0 {
        get_token_balance(client, &cfg.user_a)?
    } else {
        get_token_balance(client, &cfg.user_b)?
    };

    if user_in_bal < amount_in {
        warn!("trade: insufficient balance ({} < {}), skipping", user_in_bal, amount_in);
        return Ok(format!("skip: insufficient balance ({} < {})", user_in_bal, amount_in));
    }

    // Check pool has enough on output side
    let reserve_out = if direction == 0 { snap.reserve_b } else { snap.reserve_a };
    if expected_out > 0 && reserve_out < expected_out {
        return Ok(format!("skip: pool reserve_out too low ({} < {})", reserve_out, expected_out));
    }

    if bh.stale() { bh.refresh(client)?; }
    let blockhash = bh.get()?;

    // Capture the output-side reserve before sending so we can measure actual out.
    let reserve_out_before = reserve_out;

    let sig = send_swap(cfg, client, blockhash, direction, amount_in, 0)
        .context("SWAP send")?;

    // Measure actual out_amount from the on-chain reserve delta.
    let reserve_out_after = match read_pool_reserve_out(client, &cfg.pool_pubkey, direction) {
        Ok(v) => v,
        Err(e) => {
            warn!("trade: post-swap reserve read failed ({:#}) — recording expected_out", e);
            reserve_out_before.saturating_sub(expected_out)
        }
    };
    let out_amount = reserve_out_before.saturating_sub(reserve_out_after);

    // Record in MmBook (accounts for Jito tip if the bundle used one).
    {
        let oracle_price = snap.oracle_price;
        let mut mm = state.mm_book.write().await;
        mm.record_swap(direction == 0, amount_in, out_amount, oracle_price, cfg.jito_tip_lamports);
    }

    // Slippage vs oracle expectation, signed: positive = better than oracle.
    let slippage_bps = if expected_out > 0 {
        (out_amount as i64 - expected_out as i64) * 10_000 / expected_out as i64
    } else { 0 };

    Ok(format!(
        "swap: sig={} dir={} in={} out={} slippage={}bps",
        &sig[..16],
        if direction == 0 { "A→B" } else { "B→A" },
        amount_in,
        out_amount,
        slippage_bps,
    ))
}

/// Fetch the current output-side reserve from the pool account.
fn read_pool_reserve_out(client: &RpcClient, pool: &Pubkey, direction: u8) -> Result<u64> {
    let data = client.get_account_data(pool).context("get_account_data(pool)")?;
    let offset = if direction == 0 { OFF_RESERVE_B } else { OFF_RESERVE_A };
    if data.len() < offset + 8 {
        anyhow::bail!("pool account too small ({} bytes)", data.len());
    }
    Ok(u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap()))
}

// ── Token balance ─────────────────────────────────────────────────────────────

fn get_token_balance(client: &RpcClient, token_account: &Pubkey) -> Result<u64> {
    let acc = client
        .get_token_account_balance(token_account)
        .context("get_token_account_balance")?;
    Ok(acc.amount.parse::<u64>().unwrap_or(0))
}

// ── SWAP instruction ──────────────────────────────────────────────────────────
//
// Accounts (8):
//   0  pool      writable
//   1  user_in   writable
//   2  vault_in  writable
//   3  user_out  writable
//   4  vault_out writable
//   5  user      signer (readonly)
//   6  pool_auth readonly
//   7  TOKEN_PROGRAM_ID readonly
//
// Data (18 bytes):
//   [0]      discriminant = 2
//   [1..9]   amount_in  u64 le
//   [9..17]  min_out    u64 le
//   [17]     direction  u8 (0 = A→B, 1 = B→A)

fn send_swap(
    cfg:       &Config,
    client:    &RpcClient,
    blockhash: Hash,
    direction: u8,
    amount_in: u64,
    min_out:   u64,
) -> Result<String> {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID).unwrap();

    let (user_in, vault_in, user_out, vault_out) = if direction == 0 {
        (cfg.user_a, cfg.vault_a, cfg.user_b, cfg.vault_b)
    } else {
        (cfg.user_b, cfg.vault_b, cfg.user_a, cfg.vault_a)
    };

    let mut data = Vec::with_capacity(18);
    data.push(2u8); // SWAP discriminant
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_out.to_le_bytes());
    data.push(direction);

    let ix = Instruction {
        program_id: cfg.program_id,
        accounts: vec![
            AccountMeta::new(cfg.pool_pubkey, false),
            AccountMeta::new(user_in,         false),
            AccountMeta::new(vault_in,        false),
            AccountMeta::new(user_out,        false),
            AccountMeta::new(vault_out,       false),
            AccountMeta::new_readonly(cfg.wallet.pubkey(), true),
            AccountMeta::new_readonly(cfg.pool_auth,       false),
            AccountMeta::new_readonly(token_program,       false),
        ],
        data,
    };

    let (sig, _) = send_budgeted(client, &[ix], &cfg.wallet, blockhash)
        .context("SWAP send_budgeted")?;
    Ok(sig)
}

// ── ADD_LIQUIDITY instruction ──────────────────────────────────────────────────
//
// Accounts (11): pool, user_a, vault_a, user_b, vault_b, lp_mint, user_lp,
//                dead_lp_account, user(signer), pool_auth, TOKEN_PROGRAM_ID
// Data (25 bytes): [3] + amount_a(8) + amount_b(8) + min_lp_out(8)

#[allow(dead_code)]
fn send_add_liquidity(
    cfg:        &Config,
    client:     &RpcClient,
    blockhash:  Hash,
    amount_a:   u64,
    amount_b:   u64,
    min_lp_out: u64,
) -> Result<String> {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID).unwrap();

    let mut data = Vec::with_capacity(25);
    data.push(3u8);
    data.extend_from_slice(&amount_a.to_le_bytes());
    data.extend_from_slice(&amount_b.to_le_bytes());
    data.extend_from_slice(&min_lp_out.to_le_bytes());

    let ix = Instruction {
        program_id: cfg.program_id,
        accounts: vec![
            AccountMeta::new(cfg.pool_pubkey,     false),
            AccountMeta::new(cfg.user_a,          false),
            AccountMeta::new(cfg.vault_a,         false),
            AccountMeta::new(cfg.user_b,          false),
            AccountMeta::new(cfg.vault_b,         false),
            AccountMeta::new(cfg.lp_mint,         false),
            AccountMeta::new(cfg.user_lp,         false),
            AccountMeta::new(cfg.dead_lp_account, false),
            AccountMeta::new_readonly(cfg.wallet.pubkey(), true),
            AccountMeta::new_readonly(cfg.pool_auth,       false),
            AccountMeta::new_readonly(token_program,       false),
        ],
        data,
    };

    let (sig, _) = send_budgeted(client, &[ix], &cfg.wallet, blockhash)
        .context("ADD_LIQUIDITY send_budgeted")?;
    Ok(sig)
}
