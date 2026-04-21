//! Thin async JSON-RPC client using reqwest.
//!
//! solana-client uses rustls; reqwest on macOS uses LibreSSL. Both reject the
//! Clash proxy's MITM cert (proxy CA not in the system keychain LibreSSL trusts).
//! `make_client()` creates a client with cert verification disabled — safe for
//! devnet-only use. On mainnet remove `danger_accept_invalid_certs` or configure
//! Clash to not intercept Solana RPC traffic.

use anyhow::{Context, Result};
use base64::Engine;
use solana_sdk::{hash::Hash, transaction::Transaction};
use std::str::FromStr;

/// Build a reqwest client that works through Clash's MITM proxy on devnet.
pub fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("reqwest client")
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn check(resp: &serde_json::Value) -> Result<()> {
    if !resp["error"].is_null() {
        anyhow::bail!("RPC error: {}", resp["error"]);
    }
    Ok(())
}

fn b64(tx: &Transaction) -> Result<String> {
    Ok(base64::engine::general_purpose::STANDARD
        .encode(bincode::serialize(tx).context("serialize tx")?))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Fetch raw account bytes (empty Vec if account not found).
pub async fn get_account_data(
    http: &reqwest::Client,
    url:  &str,
    addr: &solana_sdk::pubkey::Pubkey,
) -> Result<Vec<u8>> {
    let r: serde_json::Value = http.post(url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"getAccountInfo",
            "params":[addr.to_string(), {"encoding":"base64"}]
        }))
        .send().await.context("getAccountInfo")?
        .json().await.context("getAccountInfo parse")?;
    check(&r)?;
    let b64_str = r["result"]["value"]["data"][0].as_str().context("no account data")?;
    base64::engine::general_purpose::STANDARD.decode(b64_str).context("base64 decode")
}

/// Returns confirmed latest blockhash.
pub async fn get_latest_blockhash(http: &reqwest::Client, url: &str) -> Result<Hash> {
    let r: serde_json::Value = http.post(url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"getLatestBlockhash",
            "params":[{"commitment":"confirmed"}]
        }))
        .send().await.context("getLatestBlockhash")?
        .json().await.context("getLatestBlockhash parse")?;
    check(&r)?;
    let s = r["result"]["value"]["blockhash"].as_str().context("no blockhash")?;
    Hash::from_str(s).context("parse blockhash")
}

/// Simulate `tx`, return units consumed (0 if not reported).
/// Returns Err if simulation reports a program error.
pub async fn simulate(http: &reqwest::Client, url: &str, tx: &Transaction) -> Result<u64> {
    let r: serde_json::Value = http.post(url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"simulateTransaction",
            "params":[b64(tx)?, {"encoding":"base64","sigVerify":false,"replaceRecentBlockhash":true}]
        }))
        .send().await.context("simulateTransaction")?
        .json().await.context("simulateTransaction parse")?;
    check(&r)?;
    if !r["result"]["value"]["err"].is_null() {
        anyhow::bail!("sim error: {}", r["result"]["value"]["err"]);
    }
    Ok(r["result"]["value"]["unitsConsumed"].as_u64().unwrap_or(200_000))
}

/// Send a signed transaction, return base58 signature string.
/// skipPreflight=true: avoids BlockhashNotFound in send-node's simulation cache.
/// The blockhash validity is still checked when the tx lands in a block.
pub async fn send_transaction(http: &reqwest::Client, url: &str, tx: &Transaction) -> Result<String> {
    let r: serde_json::Value = http.post(url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"sendTransaction",
            "params":[b64(tx)?, {"encoding":"base64","skipPreflight":true,"preflightCommitment":"confirmed"}]
        }))
        .send().await.context("sendTransaction")?
        .json().await.context("sendTransaction parse")?;
    check(&r)?;
    r["result"].as_str().map(|s| s.to_string()).context("no signature in response")
}

/// Fetch SOL balance in lamports.
pub async fn get_balance(
    http: &reqwest::Client,
    url:  &str,
    addr: &solana_sdk::pubkey::Pubkey,
) -> Result<u64> {
    let r: serde_json::Value = http.post(url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"getBalance",
            "params":[addr.to_string(), {"commitment":"confirmed"}]
        }))
        .send().await.context("getBalance")?
        .json().await.context("getBalance parse")?;
    check(&r)?;
    r["result"]["value"].as_u64().context("no balance value")
}

/// Fetch SPL token account balance, return raw amount as u64.
pub async fn get_token_balance(
    http: &reqwest::Client,
    url:  &str,
    addr: &solana_sdk::pubkey::Pubkey,
) -> Result<u64> {
    let r: serde_json::Value = http.post(url)
        .json(&serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"getTokenAccountBalance",
            "params":[addr.to_string()]
        }))
        .send().await.context("getTokenAccountBalance")?
        .json().await.context("getTokenAccountBalance parse")?;
    check(&r)?;
    let amount = r["result"]["value"]["amount"].as_str().context("no amount")?;
    amount.parse::<u64>().context("parse token balance")
}
