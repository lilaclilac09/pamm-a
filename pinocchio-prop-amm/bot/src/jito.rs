//! Jito bundle submission.
//!
//! On mainnet, wrap every oracle-update and swap transaction in a Jito bundle:
//!   tx[0] = tip transfer (SOL → jito_tip_account)
//!   tx[1] = your instruction (UPDATE_ORACLE or SWAP)
//!
//! Why bundles protect the LP:
//!   Without Jito, an oracle update sits in the public mempool for ~400ms.
//!   An MEV bot sees it, frontruns with a swap at the stale price, your update
//!   lands, price moves, the bot backtrades. The LP eats IL on every oracle cycle.
//!
//!   With a Jito bundle the tip + tx[1] are committed atomically in the same
//!   slot. No gap for frontrun. The MEV bot never sees the oracle update in the
//!   pending pool.
//!
//! Devnet behaviour:
//!   Jito doesn't operate on devnet. Set JITO_TIP_LAMPORTS=0 (default) and
//!   send_bundle falls back to a regular RPC send_transaction. Set
//!   JITO_TIP_LAMPORTS > 0 only on mainnet.
//!
//! Bundle format (Jito docs):
//!   POST /api/v1/bundles
//!   body: { "jsonrpc":"2.0","id":1,"method":"sendBundle",
//!           "params":[ ["<base64_tx_1>", "<base64_tx_2>"] ] }
//!
//!   Each element is a base64-encoded, fully-signed serialised Transaction.
//!   Max 5 transactions per bundle. First tx should be the tip transfer.
//!
//! Tip accounts (any one works — we rotate to reduce contention):
//!   96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b  (default)
//!   HFqU5x63VTqvB6pMFVWm9qMez95KBHMxMR8mNNmPkRF6
//!   Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY
//!   ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1IfygX5TPMe

use anyhow::{Context, Result};
use base64::Engine;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};
use tracing::{debug, info};

use crate::cu_budget::send_budgeted;

/// Jito tip accounts — rotate across them to spread load.
static TIP_ACCOUNTS: &[&str] = &[
    "96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b",
    "HFqU5x63VTqvB6pMFVWm9qMez95KBHMxMR8mNNmPkRF6",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1IfygX5TPMe",
];

static TIP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_tip_account() -> Pubkey {
    let i = TIP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as usize;
    TIP_ACCOUNTS[i % TIP_ACCOUNTS.len()].parse().unwrap()
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Send `instructions` either as a Jito bundle (tip_lamports > 0) or as a
/// plain `send_transaction` (tip_lamports == 0 / devnet).
///
/// Returns the transaction signature string.
pub async fn send_with_tip(
    http:          &reqwest::Client,
    rpc_url:       &str,
    instructions:  &[Instruction],
    signer:        &Keypair,
    blockhash:     Hash,
    jito_endpoint: &str,
    tip_lamports:  u64,
    priority_fee:  u64,
) -> Result<String> {
    if tip_lamports == 0 {
        // Devnet / no-Jito path — plain send via native-tls reqwest
        let (sig, _) = send_budgeted(http, rpc_url, instructions, signer, blockhash, priority_fee)
            .await.context("send_budgeted")?;
        return Ok(sig);
    }

    // ── Mainnet Jito path ─────────────────────────────────────────────────────

    // tx[0]: tip transfer
    let tip_account = next_tip_account();
    let tip_ix = system_instruction::transfer(&signer.pubkey(), &tip_account, tip_lamports);
    let tip_tx = {
        let msg = solana_sdk::message::Message::new(&[tip_ix], Some(&signer.pubkey()));
        let mut tx = Transaction::new_unsigned(msg);
        tx.sign(&[signer], blockhash);
        tx
    };

    // tx[1]: the actual instruction (with CU budget from send_budgeted simulation)
    // We simulate first to get optimal CU budget, then bundle both.
    let (_, budget) = send_budgeted(http, rpc_url, instructions, signer, blockhash, priority_fee)
        .await.context("simulate for CU budget")?;
    // Rebuild with exact budget
    let cu_ix = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(budget);
    let mut all_ixs = vec![cu_ix];
    if priority_fee > 0 {
        all_ixs.push(solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_price(priority_fee));
    }
    all_ixs.extend_from_slice(instructions);
    let payload_tx = {
        let msg = solana_sdk::message::Message::new(&all_ixs, Some(&signer.pubkey()));
        let mut tx = Transaction::new_unsigned(msg);
        tx.sign(&[signer], blockhash);
        tx
    };

    // Serialise both transactions to base64
    let enc = base64::engine::general_purpose::STANDARD;
    let tip_b64  = enc.encode(bincode::serialize(&tip_tx).context("serialise tip tx")?);
    let main_b64 = enc.encode(bincode::serialize(&payload_tx).context("serialise payload tx")?);

    // POST bundle to Jito block engine
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendBundle",
        "params": [[tip_b64, main_b64]]
    });

    let resp = http
        .post(jito_endpoint)
        .json(&body)
        .send().await
        .context("Jito sendBundle request")?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        anyhow::bail!("Jito bundle rejected {}: {}", status, text);
    }

    let json: serde_json::Value = serde_json::from_str(&text)
        .context("Jito response parse")?;

    // Jito returns the bundle UUID, not the tx signature.
    // The tx signature of the payload is what we need for confirmation.
    let bundle_id = json["result"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();

    let sig = payload_tx.signatures.first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".into());

    info!("jito: bundle={} sig={}", bundle_id, &sig[..16]);

    // Optional: poll bundle status for confirmation
    // In practice, if the bundle landed, the sig will be confirmed on-chain.
    // poll_bundle_status(http, jito_endpoint, &bundle_id).await?;

    Ok(sig)
}

// ── Bundle status poll (optional) ────────────────────────────────────────────

#[allow(dead_code)]
async fn poll_bundle_status(
    http:      &reqwest::Client,
    endpoint:  &str,
    bundle_id: &str,
) -> Result<()> {
    // Derive the status URL from the bundle endpoint
    let status_url = endpoint.replace("/bundles", "/getBundleStatuses");

    for attempt in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getBundleStatuses",
            "params": [[bundle_id]]
        });

        let resp = http.post(&status_url).json(&body)
            .send().await.context("getBundleStatuses")?;
        let json: serde_json::Value = resp.json().await.context("parse getBundleStatuses")?;

        let status = json["result"]["value"][0]["confirmation_status"]
            .as_str().unwrap_or("unknown");

        debug!("jito bundle {} status: {} (attempt {})", bundle_id, status, attempt);

        if status == "confirmed" || status == "finalized" {
            return Ok(());
        }
        if status == "Failed" {
            anyhow::bail!("bundle {} failed", bundle_id);
        }
    }

    anyhow::bail!("bundle {} not confirmed after 5s", bundle_id)
}

// ── Tip amount guide ──────────────────────────────────────────────────────────
//
//  Competitive tip sizing (mainnet):
//
//  The MEV bot that would frontrun your oracle update earns:
//    arb_profit = pool_size * oracle_move_bps / 10_000
//
//  Example: pool has $500K liquidity, oracle moves 10bps per cycle:
//    arb = $500K * 0.001 = $50 per oracle cycle
//    Your tip must beat what the leader earns from including the MEV tx.
//    Starting tip: $0.10 - $1.00 (1K - 10K lamports at $200/SOL)
//
//  On devnet: JITO_TIP_LAMPORTS=0 (Jito doesn't exist on devnet)
//  On mainnet: JITO_TIP_LAMPORTS=10000  (~$0.002 per oracle update)

pub fn tip_sol(tip_lamports: u64) -> f64 {
    tip_lamports as f64 / LAMPORTS_PER_SOL as f64
}
