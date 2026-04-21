//! Compute-unit budgeting: simulate → budget → send.

use anyhow::{Context, Result};
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

use crate::rpc;

const HEADROOM_PCT: u64 = 15;
const CU_HARD_CAP:  u32 = 1_400_000;
const CU_MIN:       u32 = 2_000;

/// Simulate, compute optimal CU limit, then send.
/// Returns `(signature, units_budgeted)`.
pub async fn send_budgeted(
    http:         &reqwest::Client,
    rpc_url:      &str,
    instructions: &[Instruction],
    signer:       &Keypair,
    blockhash:    Hash,
    priority_fee: u64,
) -> Result<(String, u32)> {
    // Build scratch tx for simulation
    let sim_msg = Message::new(instructions, Some(&signer.pubkey()));
    let mut sim_tx = Transaction::new_unsigned(sim_msg);
    sim_tx.sign(&[signer], blockhash);

    let units_used = rpc::simulate(http, rpc_url, &sim_tx).await
        .context("simulate")?;

    let budget = ((units_used * (100 + HEADROOM_PCT)) / 100)
        .clamp(CU_MIN as u64, CU_HARD_CAP as u64) as u32;

    debug!("CU sim={} budget={} fee={}μL", units_used, budget, priority_fee);

    // Rebuild with budget ixs
    let mut all_ixs: Vec<Instruction> = Vec::with_capacity(instructions.len() + 2);
    all_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(budget));
    if priority_fee > 0 {
        all_ixs.push(ComputeBudgetInstruction::set_compute_unit_price(priority_fee));
    }
    all_ixs.extend_from_slice(instructions);

    let msg = Message::new(&all_ixs, Some(&signer.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.sign(&[signer], blockhash);

    let sig = rpc::send_transaction(http, rpc_url, &tx).await
        .context("send_transaction")?;

    Ok((sig, budget))
}
