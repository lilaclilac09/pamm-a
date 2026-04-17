//! Helius fast-sender submission.
//!
//! POST a signed, base64-encoded transaction to the Helius Sender endpoint
//! (`/fast`), which simultaneously fans out to:
//!   • Staked Connections (SWQoS) — ~30ms via high-priority stake-weighted path
//!   • Jito Block Engine           — MEV auction / bundle relay
//!
//! The endpoint expects a standard Solana JSON-RPC `sendTransaction` request.
//! We set `skipPreflight: true` (no simulation on the sender node) and
//! `maxRetries: 0` (we re-fire on the next signal rather than retrying stale
//! transactions — stale txns waste block space).

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::json;
use tracing::debug;

use crate::Config;

/// Submit a base64-encoded, signed transaction to the Helius fast sender.
///
/// Returns the transaction signature string on success.
pub async fn submit(
    cfg: &Arc<Config>,
    http: &reqwest::Client,
    tx_b64: &str,
) -> Result<String> {
    // Append API key as query param
    let url = format!(
        "{}?api-key={}",
        cfg.sender_endpoint, cfg.helius_api_key
    );

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [
            tx_b64,
            {
                "encoding":      "base64",
                "skipPreflight": true,
                "maxRetries":    0
            }
        ]
    });

    debug!("sender: POST {}", cfg.sender_endpoint);

    let resp: serde_json::Value = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("sender HTTP send")?
        .json()
        .await
        .context("sender JSON decode")?;

    if let Some(err) = resp.get("error") {
        bail!("sender RPC error: {}", err);
    }

    let sig = resp
        .get("result")
        .and_then(|v| v.as_str())
        .context("sender: missing 'result' in response")?
        .to_string();

    Ok(sig)
}
