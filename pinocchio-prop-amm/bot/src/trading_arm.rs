//! Trading arm — runs every TRADE_INTERVAL_MS.
//!
//! Each cycle:
//!   1. Read pool snapshot from SharedState (no extra RPC call)
//!   2. Fetch Jupiter quote for the desired trade direction
//!   3. TWAP sanity check via is_price_sane()
//!   4. Decision: rebalance via ADD/REMOVE_LIQUIDITY or active Jupiter swap
//!   5. Check dual-token balance before ADD_LIQUIDITY (critical fix from CEO review)
//!   6. Submit signed bundle to Jito

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    signer::Signer,
    system_instruction,
    transaction::Transaction,
};
use std::{sync::Arc, time::Instant};
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};

use crate::{
    config::Config,
    risk::{is_price_sane, SharedState},
};

// ── Blockhash cache (same pattern as oracle arm) ──────────────────────────────

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
    let http   = reqwest::Client::new();
    let mut bh = BhCache::new();
    let mut cycle: u64 = 0;

    info!("trading_arm: starting (interval={}ms)", cfg.trade_interval_ms);

    loop {
        if state.is_halted() {
            info!("trading_arm: halted — pausing");
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        cycle += 1;

        match run_cycle(&cfg, &client, &http, &mut bh, &state).await {
            Ok(action) => {
                info!("[trade #{}] {}", cycle, action);
                state.trade_failures.store(0, std::sync::atomic::Ordering::Relaxed);
                let mut m = state.metrics.write().await;
                m.trade_cycles = cycle;
                m.trade_errors = 0;
            }
            Err(e) => {
                let failures = state.trade_failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                error!("[trade #{}] error ({}/{}): {:#}", cycle, failures,
                    cfg.max_consecutive_failures, e);

                let mut m = state.metrics.write().await;
                m.trade_errors = failures;

                if failures >= cfg.max_consecutive_failures {
                    state.halt(&format!("trading arm: {} consecutive failures", failures));
                    return;
                }
            }
        }

        sleep(Duration::from_millis(cfg.trade_interval_ms)).await;
    }
}

async fn run_cycle(
    cfg:    &Config,
    client: &RpcClient,
    http:   &reqwest::Client,
    bh:     &mut BhCache,
    state:  &SharedState,
) -> Result<String> {
    let snap = state.pool.read().await.clone();
    if snap.oracle_price == 0 {
        return Ok("skip: oracle not yet updated".into());
    }

    // ── 1. Jupiter quote (A→B direction to probe the price) ──────────────────
    let mint_a = cfg.mint_a.to_string();
    let mint_b = cfg.mint_b.to_string();

    // Use 1/100 of max_trade_lamports as the probe amount for TWAP check
    let probe_amount = cfg.max_trade_lamports / 100;
    let quote = jupiter_quote(http, &mint_a, &mint_b, probe_amount, cfg.slippage_bps)
        .await.context("jupiter quote")?;

    let out_amount: u64 = quote["outAmount"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Implied price: lamports of A in / units of B out, scaled to 1e9
    // (assumes tokenB is SOL in lamports; adjust if different)
    let jupiter_price: u64 = if out_amount > 0 {
        probe_amount.saturating_mul(1_000_000_000) / out_amount
    } else {
        return Ok("skip: zero outAmount from Jupiter".into());
    };

    // ── 2. TWAP sanity check (Phase 3) ────────────────────────────────────────
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if !is_price_sane(
        jupiter_price,
        &snap.twap_obs,
        cfg.max_price_deviation_bps,
        cfg.max_twap_age_secs,
        now,
    ) {
        warn!("trade skipped: price sanity check failed (jupiter={} oracle={})",
            jupiter_price, snap.oracle_price);
        return Ok("skip: TWAP sanity check failed".into());
    }

    // ── 3. Inventory ratio (token A value / total value) ─────────────────────
    let value_a = snap.reserve_a as f64 * snap.oracle_price as f64 / 1e9;
    let value_b = snap.reserve_b as f64;
    let total   = value_a + value_b;
    let inv_ratio = if total > 0.0 { value_a / total } else { 0.5 };

    {
        let mut m = state.metrics.write().await;
        m.inventory_ratio = inv_ratio;
    }

    if bh.stale() { bh.refresh(client)?; }
    let blockhash = bh.get()?;

    // ── 4. Action decision ────────────────────────────────────────────────────
    if inv_ratio > cfg.rebalance_high {
        // Overweight token A — remove liquidity, pull both tokens, let oracle arm rebalance
        let lp_to_burn = estimate_lp_to_burn(&snap, inv_ratio, cfg.rebalance_high);
        if lp_to_burn > 0 {
            info!("rebalancing: remove {} LP (inv_ratio={:.3})", lp_to_burn, inv_ratio);
            send_remove_liquidity(cfg, client, blockhash, lp_to_burn, 0, 0)?;
            return Ok(format!("remove_liquidity: burned {} LP", lp_to_burn));
        }
    } else if inv_ratio < cfg.rebalance_low {
        // Underweight token A — try to add liquidity if we have both tokens
        let amount_a = cfg.max_trade_lamports / 2;
        let amount_b = cfg.max_trade_lamports / 2;
        let bal_a = get_token_balance(client, &cfg.user_a).context("balance A")?;
        let bal_b = get_token_balance(client, &cfg.user_b).context("balance B")?;

        if bal_a >= amount_a && bal_b >= amount_b {
            info!("rebalancing: add liquidity {}A + {}B (inv_ratio={:.3})",
                amount_a, amount_b, inv_ratio);
            // min_lp_out = 0 for now; TODO: derive from expected amounts
            send_add_liquidity(cfg, client, blockhash, amount_a, amount_b, 0)?;
            return Ok(format!("add_liquidity: {}A + {}B", amount_a, amount_b));
        } else {
            // Fall through to Jupiter swap to acquire needed tokens
            info!("rebalancing: insufficient balance for ADD_LIQUIDITY (A={} B={}) — falling back to Jupiter swap",
                bal_a, bal_b);
        }
    }

    // ── 5. Active Jupiter swap (spread capture) ───────────────────────────────
    // Direction: sell A for B if overweight, buy A with B if underweight
    let (in_mint, out_mint, amount) = if inv_ratio > 0.5 {
        (mint_a.clone(), mint_b.clone(), cfg.max_trade_lamports)
    } else {
        (mint_b.clone(), mint_a.clone(), cfg.max_trade_lamports)
    };

    let trade_quote = jupiter_quote(http, &in_mint, &out_mint, amount, cfg.slippage_bps)
        .await.context("jupiter trade quote")?;
    let swap_tx_b64 = jupiter_swap_tx(http, trade_quote, &cfg.wallet.pubkey().to_string())
        .await.context("jupiter swap tx")?;

    let sig = submit_jito_bundle(cfg, client, &http, blockhash, &swap_tx_b64).await?;
    Ok(format!("jupiter_swap: sig={} dir={}/{}", sig, in_mint, out_mint))
}

// ── LP burn estimate ──────────────────────────────────────────────────────────

/// Estimate how many LP tokens to burn to bring inventory back to `target_ratio`.
/// Burns the fraction of LP supply proportional to the excess value.
fn estimate_lp_to_burn(snap: &crate::risk::PoolSnapshot, current_ratio: f64, target_ratio: f64) -> u64 {
    if snap.lp_supply == 0 { return 0; }
    let excess = (current_ratio - target_ratio).max(0.0);
    let burn_fraction = (excess / current_ratio).min(0.5); // never burn more than 50%
    (snap.lp_supply as f64 * burn_fraction) as u64
}

// ── Token balance ─────────────────────────────────────────────────────────────

fn get_token_balance(client: &RpcClient, token_account: &solana_sdk::pubkey::Pubkey) -> Result<u64> {
    let acc = client
        .get_token_account_balance(token_account)
        .context("get_token_account_balance")?;
    Ok(acc.amount.parse::<u64>().unwrap_or(0))
}

// ── Jupiter API ───────────────────────────────────────────────────────────────

async fn jupiter_quote(
    http:         &reqwest::Client,
    input_mint:   &str,
    output_mint:  &str,
    amount:       u64,
    slippage_bps: u64,
) -> Result<serde_json::Value> {
    let url = format!(
        "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps={}",
        input_mint, output_mint, amount, slippage_bps
    );
    let resp: serde_json::Value = http
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send().await.context("Jupiter quote request")?
        .json().await.context("Jupiter quote parse")?;

    if resp.get("error").is_some() {
        anyhow::bail!("Jupiter quote error: {}", resp["error"]);
    }
    Ok(resp)
}

async fn jupiter_swap_tx(
    http:          &reqwest::Client,
    quote:         serde_json::Value,
    user_pubkey:   &str,
) -> Result<String> {
    let body = serde_json::json!({
        "quoteResponse": quote,
        "userPublicKey": user_pubkey,
        "wrapAndUnwrapSol": true,
        "dynamicComputeUnitLimit": true,
        "prioritizationFeeLamports": "auto",
    });
    let resp: serde_json::Value = http
        .post("https://quote-api.jup.ag/v6/swap")
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send().await.context("Jupiter swap request")?
        .json().await.context("Jupiter swap parse")?;

    resp["swapTransaction"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no swapTransaction in response: {}", resp))
}

// ── Transaction signing ───────────────────────────────────────────────────────

/// Sign a Jupiter VersionedTransaction.  Returns the signed bytes.
fn sign_versioned_tx(
    wallet: &solana_sdk::signature::Keypair,
    base64_tx: &str,
) -> Result<Vec<u8>> {
    use solana_sdk::{signer::Signer, transaction::VersionedTransaction};

    let tx_bytes = B64.decode(base64_tx).context("base64 decode Jupiter tx")?;
    let mut tx: VersionedTransaction =
        bincode::deserialize(&tx_bytes).context("deserialize Jupiter tx")?;

    let msg_bytes = tx.message.serialize();
    let sig = wallet.sign_message(&msg_bytes);

    // Find wallet's position in the account key list
    let idx = tx
        .message
        .static_account_keys()
        .iter()
        .position(|k| k == &wallet.pubkey())
        .ok_or_else(|| anyhow::anyhow!("wallet key not found in Jupiter tx accounts"))?;

    if idx >= tx.signatures.len() {
        anyhow::bail!("wallet index {} exceeds signatures array len {}", idx, tx.signatures.len());
    }
    tx.signatures[idx] = sig;

    bincode::serialize(&tx).context("serialize signed tx")
}

// ── Jito bundle ───────────────────────────────────────────────────────────────

async fn submit_jito_bundle(
    cfg:       &Config,
    client:    &RpcClient,
    http:      &reqwest::Client,
    blockhash: Hash,
    swap_b64:  &str,
) -> Result<String> {
    // 1. Sign Jupiter swap tx
    let signed_swap_bytes = sign_versioned_tx(&cfg.wallet, swap_b64)?;
    let signed_swap_b64 = B64.encode(&signed_swap_bytes);

    // 2. Build Jito tip transfer tx
    let tip_ix = system_instruction::transfer(
        &cfg.wallet.pubkey(),
        &cfg.jito_tip_account,
        cfg.jito_tip_lamports,
    );
    let tip_msg = Message::new(&[tip_ix], Some(&cfg.wallet.pubkey()));
    let tip_tx  = Transaction::new(&[&cfg.wallet], tip_msg, blockhash);
    let tip_bytes = bincode::serialize(&tip_tx).context("serialize tip tx")?;
    let tip_b64 = B64.encode(&tip_bytes);

    // 3. Submit bundle
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendBundle",
        "params": [[signed_swap_b64, tip_b64]],
    });

    let resp: serde_json::Value = http
        .post(&cfg.jito_endpoint)
        .json(&payload)
        .timeout(Duration::from_secs(10))
        .send().await.context("Jito bundle request")?
        .json().await.context("Jito bundle parse")?;

    if let Some(err) = resp.get("error") {
        anyhow::bail!("Jito bundle error: {}", err);
    }

    Ok(resp["result"].as_str().unwrap_or("unknown").to_string())
}

// ── ADD_LIQUIDITY instruction ──────────────────────────────────────────────────
//
// Accounts (10): pool, user_a, vault_a, user_b, vault_b, lp_mint, user_lp,
//                dead_lp_account, user(signer), pool_auth
// Data (25 bytes): [3] + amount_a(8) + amount_b(8) + min_lp_out(8)

fn send_add_liquidity(
    cfg:        &Config,
    client:     &RpcClient,
    blockhash:  Hash,
    amount_a:   u64,
    amount_b:   u64,
    min_lp_out: u64,
) -> Result<String> {
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
        ],
        data,
    };

    let mut tx = Transaction::new_unsigned(Message::new(&[ix], Some(&cfg.wallet.pubkey())));
    tx.sign(&[&cfg.wallet], blockhash);
    let sig = client.send_transaction(&tx).context("ADD_LIQUIDITY send")?;
    Ok(sig.to_string())
}

// ── REMOVE_LIQUIDITY instruction ───────────────────────────────────────────────
//
// Accounts (9): pool, vault_a, user_a, vault_b, user_b, lp_mint, user_lp,
//               pool_auth, user(signer)
// Data (25 bytes): [4] + lp_amount(8) + min_out_a(8) + min_out_b(8)

fn send_remove_liquidity(
    cfg:       &Config,
    client:    &RpcClient,
    blockhash: Hash,
    lp_amount: u64,
    min_out_a: u64,
    min_out_b: u64,
) -> Result<String> {
    let mut data = Vec::with_capacity(25);
    data.push(4u8);
    data.extend_from_slice(&lp_amount.to_le_bytes());
    data.extend_from_slice(&min_out_a.to_le_bytes());
    data.extend_from_slice(&min_out_b.to_le_bytes());

    let ix = Instruction {
        program_id: cfg.program_id,
        accounts: vec![
            AccountMeta::new(cfg.pool_pubkey, false),
            AccountMeta::new(cfg.vault_a,     false),
            AccountMeta::new(cfg.user_a,      false),
            AccountMeta::new(cfg.vault_b,     false),
            AccountMeta::new(cfg.user_b,      false),
            AccountMeta::new(cfg.lp_mint,     false),
            AccountMeta::new(cfg.user_lp,     false),
            AccountMeta::new_readonly(cfg.pool_auth,       false),
            AccountMeta::new_readonly(cfg.wallet.pubkey(), true),
        ],
        data,
    };

    let mut tx = Transaction::new_unsigned(Message::new(&[ix], Some(&cfg.wallet.pubkey())));
    tx.sign(&[&cfg.wallet], blockhash);
    let sig = client.send_transaction(&tx).context("REMOVE_LIQUIDITY send")?;
    Ok(sig.to_string())
}
