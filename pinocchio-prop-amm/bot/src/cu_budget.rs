//! Compute-unit budgeting + pre-simulation guard.
//!
//! `send_budgeted` does three things in one call:
//!
//!   1. **Simulate** — runs the transaction against the current chain state.
//!      Returns `Err` immediately if simulation fails, saving the fee.
//!
//!   2. **Budget** — reads `units_consumed` from the simulation result,
//!      adds `HEADROOM_PCT`% headroom, prepends
//!      `ComputeBudgetInstruction::set_compute_unit_limit(budget)`.
//!      This cuts priority fees vs the default 200k CU budget.
//!
//!   3. **Send** — signs the budgeted transaction and submits it.
//!
//! Only use this for *our own* instructions (UPDATE_ORACLE, ADD_LIQUIDITY,
//! REMOVE_LIQUIDITY). Jupiter swap transactions already contain their own
//! CU budgets set by the Jupiter API.

use anyhow::{Context, Result};
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::RpcSimulateTransactionConfig,
};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::Instruction,
    message::Message,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use tracing::debug;

/// Headroom added on top of simulated CU consumption (percent).
const HEADROOM_PCT: u64 = 15;

/// Hard cap — Solana's per-transaction limit is 1.4M.
const CU_HARD_CAP: u32 = 1_400_000;

/// Minimum CU limit to set even if simulation reports very low usage.
/// Prevents edge cases where simulation underestimates.
const CU_MIN: u32 = 2_000;

/// Simulate `instructions`, compute an optimal CU limit, then build, sign,
/// and send the transaction.
///
/// Returns `(signature, units_used)` on success.
/// Returns `Err` if simulation reports a program error — the transaction is
/// NOT sent in that case, so no fee is wasted.
pub fn send_budgeted(
    client:       &RpcClient,
    instructions: &[Instruction],
    signer:       &Keypair,
    blockhash:    Hash,
) -> Result<(String, u32)> {
    // ── 1. Build a scratch tx for simulation (no CU budget ix yet) ────────────
    let sim_msg = Message::new(instructions, Some(&signer.pubkey()));
    let mut sim_tx = Transaction::new_unsigned(sim_msg);
    sim_tx.sign(&[signer], blockhash);

    // ── 2. Simulate ───────────────────────────────────────────────────────────
    let sim_result = client
        .simulate_transaction_with_config(
            &sim_tx,
            RpcSimulateTransactionConfig {
                sig_verify:            false, // skip sig check — we care about logic errors
                replace_recent_blockhash: true,
                ..RpcSimulateTransactionConfig::default()
            },
        )
        .context("simulate_transaction failed")?;

    if let Some(err) = &sim_result.value.err {
        anyhow::bail!("simulation error (not sending): {:?}", err);
    }

    let units_used = sim_result.value.units_consumed.unwrap_or(200_000);

    // ── 3. Compute CU budget ──────────────────────────────────────────────────
    let budget = ((units_used * (100 + HEADROOM_PCT)) / 100)
        .clamp(CU_MIN as u64, CU_HARD_CAP as u64) as u32;

    debug!("CU sim={} budget={}", units_used, budget);

    // ── 4. Rebuild with CU limit prepended ───────────────────────────────────
    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(budget);
    let mut all_ixs: Vec<Instruction> = Vec::with_capacity(instructions.len() + 1);
    all_ixs.push(cu_ix);
    all_ixs.extend_from_slice(instructions);

    let msg = Message::new(&all_ixs, Some(&signer.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.sign(&[signer], blockhash);

    // ── 5. Send ───────────────────────────────────────────────────────────────
    let sig = client
        .send_transaction(&tx)
        .context("send_transaction failed")?;

    Ok((sig.to_string(), budget))
}
