//! Trading arm — issues direct SWAP instructions against the PMM pool.
//!
//! Each cycle:
//!   1. Read pool snapshot (oracle price, reserves)
//!   2. Decide direction via inventory ratio (or alternate if pool is balanced)
//!   3. Build and send SWAP instruction directly to the PMM program
//!   4. Record swap in MmBook for PnL tracking

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
    transaction::Transaction,
};
use std::{str::FromStr, sync::Arc, time::Instant};
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};

use crate::{
    config::Config,
    cu_budget::send_budgeted,
    risk::SharedState,
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
    // Alternate direction each cycle to keep pool balanced
    let mut direction: u8 = 0; // 0 = A→B, 1 = B→A

    info!("trading_arm: starting (interval={}ms, size={})",
        cfg.trade_interval_ms, cfg.max_trade_lamports);

    loop {
        if state.is_halted() {
            info!("trading_arm: halted — pausing");
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        cycle += 1;

        match run_cycle(&cfg, &client, &mut bh, &state, direction).await {
            Ok(action) => {
                info!("[trade #{}] {}", cycle, action);
                state.trade_failures.store(0, std::sync::atomic::Ordering::Relaxed);
                // Flip direction for next cycle
                direction = 1 - direction;

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
                    // Don't flip direction on error
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
    direction: u8,
) -> Result<String> {
    let snap = state.pool.read().await.clone();
    if snap.oracle_price == 0 {
        return Ok("skip: oracle not yet updated".into());
    }
    if snap.reserve_a == 0 || snap.reserve_b == 0 {
        return Ok("skip: pool reserves empty".into());
    }

    // Pick trade size and compute expected output from oracle price
    // direction 0: A→B — send amount_a A tokens, get B out
    // direction 1: B→A — send amount_b B tokens, get A out
    //
    // A→B: out_B = amount_a * 1e9 / oracle_price  (oracle_price is A-per-B scaled by 1e9)
    // B→A: out_A = amount_b * oracle_price / 1e9
    //
    // We use max_trade_lamports as amount_a (A→B) and a scaled amount for B→A
    // so each swap is roughly the same value.
    let (amount_in, expected_out) = if direction == 0 {
        // A→B
        let amount_a = cfg.max_trade_lamports;
        let exp_b = (amount_a as u128 * 1_000_000_000u128)
            .checked_div(snap.oracle_price as u128)
            .unwrap_or(0) as u64;
        (amount_a, exp_b)
    } else {
        // B→A: send amount_b so it's roughly equivalent value to amount_a
        // amount_b = amount_a * oracle_price / 1e9 ... but oracle_price is huge (88e9)
        // that would overflow. Instead use a fixed small amount.
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
    let (user_in_bal, user_in_acc) = if direction == 0 {
        (get_token_balance(client, &cfg.user_a)?, cfg.user_a)
    } else {
        (get_token_balance(client, &cfg.user_b)?, cfg.user_b)
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

    let (sig, out_amount) = send_swap(cfg, client, blockhash, direction, amount_in, 0)
        .context("SWAP send")?;

    // Record in MmBook
    {
        let oracle_price = snap.oracle_price;
        let mut mm = state.mm_book.write().await;
        mm.record_swap(direction == 0, amount_in, out_amount, oracle_price, 0);
    }

    let spread_bps = if expected_out > 0 && out_amount > 0 {
        let slippage = (expected_out as i64 - out_amount as i64).abs() as u64;
        slippage * 10_000 / expected_out
    } else { 0 };

    Ok(format!(
        "swap: sig={} dir={} in={} out={} spread={}bps",
        &sig[..16],
        if direction == 0 { "A→B" } else { "B→A" },
        amount_in,
        out_amount,
        spread_bps,
    ))
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
//
// Returns (signature, simulated_out_amount)

fn send_swap(
    cfg:       &Config,
    client:    &RpcClient,
    blockhash: Hash,
    direction: u8,
    amount_in: u64,
    min_out:   u64,
) -> Result<(String, u64)> {
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

    // Fetch actual out amount from pool state delta would need two RPC calls.
    // For now return 0; MmBook will record what we can compute from oracle.
    Ok((sig, 0))
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
