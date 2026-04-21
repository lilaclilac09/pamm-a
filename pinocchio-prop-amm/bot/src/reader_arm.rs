//! Reader arm — momentum trading on oracle updates.
//!
//! Watches the pool account via the same LaserStream pool_rx that oracle_arm uses.
//! Every time oracle_price changes ≥ MOMENTUM_BPS, trades in the direction of the move.

use anyhow::{Context, Result};
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
};
use std::{str::FromStr, sync::Arc};
use tokio::time::{sleep, timeout, Duration};
use tracing::{debug, error, info, warn};

use crate::{
    config::Config,
    jito,
    risk::{ReaderTrade, SharedState},
};

const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Minimum oracle move (bps) to trigger a trade.
const MOMENTUM_BPS: u64 = 1;

/// Trade size (raw token units). Small on devnet to avoid draining reserves.
const TRADE_AMOUNT: u64 = 1_000;

pub async fn run(
    cfg:     Arc<Config>,
    state:   Arc<SharedState>,
    mut pool_rx: tokio::sync::watch::Receiver<Option<crate::risk::PoolSnapshot>>,
) {
    let http = crate::rpc::make_client();
    let mut cycle:       u64 = 0;
    let mut last_price:  u64 = 0;
    let mut last_update: i64 = 0;

    info!("reader_arm: starting (momentum≥{}bps size={})", MOMENTUM_BPS, TRADE_AMOUNT);

    loop {
        if state.is_halted() {
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        // Wait up to 1s for a LaserStream update; fall back to RPC poll on timeout.
        let _ = timeout(Duration::from_secs(1), pool_rx.changed()).await;

        // Explicitly drop the borrow guard before any await point.
        let cached: Option<crate::risk::PoolSnapshot> = pool_rx.borrow().clone();

        let snap = if let Some(s) = cached {
            s
        } else {
            match crate::rpc::get_account_data(&http, &cfg.rpc_url, &cfg.pool_pubkey).await
                .ok()
                .and_then(|d| crate::risk::PoolSnapshot::from_bytes(&d))
            {
                Some(s) => s,
                None => { sleep(Duration::from_millis(500)).await; continue; }
            }
        };

        // Skip if oracle hasn't ticked or pool uninitialised
        if snap.last_update == last_update || snap.oracle_price == 0 {
            continue;
        }
        last_update = snap.last_update;
        cycle += 1;

        // Seed on first tick
        if last_price == 0 {
            last_price = snap.oracle_price;
            info!("reader_arm: seeded oracle={:.6}", snap.oracle_price as f64 / 1e9);
            continue;
        }

        // Compute move in bps
        let price_move_bps = snap.oracle_price.abs_diff(last_price) * 10_000 / last_price;

        if price_move_bps < MOMENTUM_BPS {
            debug!("reader_arm [#{cycle}]: skip {}bps < {}bps", price_move_bps, MOMENTUM_BPS);
            last_price = snap.oracle_price;
            continue;
        }

        // direction=1 B→A (buy A) when price up, direction=0 A→B (sell A) when price down
        let direction: u8  = if snap.oracle_price > last_price { 1 } else { 0 };
        let dir_label      = if direction == 1 { "B→A" } else { "A→B" };

        info!("reader_arm [#{cycle}]: TRADE {dir_label}  move={price_move_bps}bps  oracle={:.6}",
              snap.oracle_price as f64 / 1e9);

        // Guard: reserve on output side must cover expected output.
        // For B→A: out_a ≈ amount_in_b * oracle / 1e9
        // For A→B: out_b ≈ amount_in_a * 1e9 / oracle
        let reserve_out = if direction == 1 { snap.reserve_a } else { snap.reserve_b };
        let expected_out = if direction == 1 {
            TRADE_AMOUNT * snap.oracle_price / 1_000_000_000
        } else if snap.oracle_price > 0 {
            TRADE_AMOUNT * 1_000_000_000 / snap.oracle_price
        } else { 0 };
        if reserve_out < expected_out || expected_out == 0 {
            warn!("reader_arm [#{cycle}]: skip {dir_label} reserve_out={} expected_out={}", reserve_out, expected_out);
            last_price = snap.oracle_price;
            continue;
        }

        match execute_swap(&cfg, &http, direction, TRADE_AMOUNT).await {
            Ok(sig) => {
                info!("reader_arm [#{cycle}]: ok sig={}", &sig[..16.min(sig.len())]);
                let trade = ReaderTrade {
                    cycle,
                    direction:    dir_label.to_string(),
                    amount_in:    TRADE_AMOUNT,
                    oracle_price: snap.oracle_price,
                    price_move:   price_move_bps,
                    sig,
                };
                state.reader_book.write().await.record(trade);
            }
            Err(e) => error!("reader_arm [#{cycle}]: swap failed: {:#}", e),
        }

        last_price = snap.oracle_price;
    }
}

// ── Swap execution ────────────────────────────────────────────────────────────

async fn execute_swap(
    cfg:       &Config,
    http:      &reqwest::Client,
    direction: u8,
    amount_in: u64,
) -> Result<String> {
    let bh = crate::rpc::get_latest_blockhash(http, &cfg.rpc_url)
        .await.context("blockhash")?;

    let sig = send_swap(cfg, http, bh, direction, amount_in)
        .await.context("send_swap")?;
    Ok(sig)
}

async fn send_swap(
    cfg:       &Config,
    http:      &reqwest::Client,
    blockhash: Hash,
    direction: u8,
    amount_in: u64,
) -> Result<String> {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID).unwrap();

    let (user_in, vault_in, user_out, vault_out) = if direction == 0 {
        (cfg.user_a, cfg.vault_a, cfg.user_b, cfg.vault_b)
    } else {
        (cfg.user_b, cfg.vault_b, cfg.user_a, cfg.vault_a)
    };

    let mut data = Vec::with_capacity(18);
    data.push(2u8);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // min_out=0 on devnet
    data.push(direction);

    let swap_ix = Instruction {
        program_id: cfg.program_id,
        accounts: vec![
            AccountMeta::new(cfg.pool_pubkey,          false),
            AccountMeta::new(user_in,                  false),
            AccountMeta::new(vault_in,                 false),
            AccountMeta::new(user_out,                 false),
            AccountMeta::new(vault_out,                false),
            AccountMeta::new_readonly(cfg.wallet.pubkey(), true),
            AccountMeta::new_readonly(cfg.pool_auth,   false),
            AccountMeta::new_readonly(token_program,   false),
        ],
        data,
    };

    // send_with_tip / send_budgeted prepends CU budget internally — don't add it here
    jito::send_with_tip(
        http, &cfg.rpc_url, &[swap_ix], &cfg.wallet, blockhash,
        &cfg.jito_endpoint, cfg.jito_tip_lamports,
        cfg.priority_fee_micro_lamports,
    ).await
}
