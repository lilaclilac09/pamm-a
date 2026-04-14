//! Oracle arm — runs every UPDATE_INTERVAL_MS.
//!
//! Each cycle:
//!   1. Fetch Pyth price + confidence
//!   2. Read pool reserves from RPC (warm-path: use cached snapshot)
//!   3. Compute vol_adj from Pyth conf vs. historical volatility
//!   4. Send UPDATE_ORACLE instruction
//!   5. Update shared PoolSnapshot from on-chain state

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    signer::Signer,
    transaction::Transaction,
};
use std::{sync::Arc, time::Instant};
use tokio::time::{sleep, Duration};
use tracing::{error, info};

use crate::{
    config::Config,
    pyth::{fetch_pyth_price, PriceHistory},
    risk::{PoolSnapshot, SharedState, POOL_SIZE},
};

// ── Blockhash cache ───────────────────────────────────────────────────────────

struct BlockhashCache {
    hash:        Option<Hash>,
    fetched_at:  Option<Instant>,
}

impl BlockhashCache {
    fn new() -> Self { BlockhashCache { hash: None, fetched_at: None } }

    fn needs_refresh(&self) -> bool {
        match self.fetched_at {
            None    => true,
            Some(t) => t.elapsed().as_secs() >= 20,
        }
    }

    fn refresh(&mut self, client: &RpcClient) -> Result<()> {
        let bh = client.get_latest_blockhash().context("get_latest_blockhash")?;
        self.hash        = Some(bh);
        self.fetched_at  = Some(Instant::now());
        Ok(())
    }

    fn get(&self) -> Result<Hash> {
        self.hash.ok_or_else(|| anyhow::anyhow!("blockhash not yet fetched"))
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(cfg: Arc<Config>, state: Arc<SharedState>) {
    let client   = RpcClient::new(cfg.rpc_url.clone());
    let http     = reqwest::Client::new();
    let mut bh   = BlockhashCache::new();
    let mut hist = PriceHistory::new(20);
    let mut cycle: u64 = 0;

    info!("oracle_arm: starting (interval={}ms)", cfg.update_interval_ms);

    loop {
        if state.is_halted() {
            info!("oracle_arm: halted — pausing");
            sleep(Duration::from_secs(5)).await;
            continue;
        }

        cycle += 1;

        match run_cycle(&cfg, &client, &http, &mut bh, &mut hist, &state).await {
            Ok(sig) => {
                info!("[oracle #{}] ok: {}", cycle, sig);
                state.oracle_failures.store(0, std::sync::atomic::Ordering::Relaxed);

                // Update metrics
                let mut m = state.metrics.write().await;
                m.oracle_cycles  = cycle;
                m.oracle_errors  = 0;
            }
            Err(e) => {
                let failures = state.oracle_failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                error!("[oracle #{}] error ({}/{}): {:#}", cycle, failures,
                    cfg.max_consecutive_failures, e);

                {
                    let mut m = state.metrics.write().await;
                    m.oracle_errors = failures;
                }

                if failures >= cfg.max_consecutive_failures {
                    state.halt(&format!(
                        "oracle arm: {} consecutive failures", failures
                    ));
                    return;
                }
            }
        }

        sleep(Duration::from_millis(cfg.update_interval_ms)).await;
    }
}

async fn run_cycle(
    cfg:    &Config,
    client: &RpcClient,
    http:   &reqwest::Client,
    bh:     &mut BlockhashCache,
    hist:   &mut PriceHistory,
    state:  &SharedState,
) -> Result<String> {
    // 1. Pyth price
    let (oracle_price, conf_bps) = fetch_pyth_price(http, &cfg.pyth_feed_id)
        .await.context("pyth fetch")?;

    hist.push(oracle_price);

    // 2. Pool reserves (used as targets; bot can override with TARGET_A/TARGET_B env)
    let pool_data = client
        .get_account_data(&cfg.pool_pubkey)
        .context("pool fetch")?;

    let snap = PoolSnapshot::from_bytes(&pool_data)
        .context("pool data too short")?;

    // Update the shared snapshot so trading arm has fresh data without an extra RPC call.
    *state.pool.write().await = snap.clone();

    let (target_a, target_b) = target_from_env_or_reserves(&snap);

    // 3. vol_adj
    let hist_vol = hist.vol_bps();
    let vol_adj  = conf_bps.max(hist_vol).min(cfg.max_vol_factor);

    info!(
        "price_1e9={} conf={}bps hist={}bps vol_adj={} ra={} rb={}",
        oracle_price, conf_bps, hist_vol, vol_adj, snap.reserve_a, snap.reserve_b
    );

    // 4. Blockhash
    if bh.needs_refresh() { bh.refresh(client)?; }
    let blockhash = bh.get()?;

    // 5. Send UPDATE_ORACLE
    let sig = send_update_oracle(cfg, client, blockhash, oracle_price,
        cfg.base_spread_bps, vol_adj, target_a, target_b)?;

    Ok(sig)
}

fn target_from_env_or_reserves(snap: &PoolSnapshot) -> (u64, u64) {
    let a = std::env::var("TARGET_A").ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(snap.reserve_a);
    let b = std::env::var("TARGET_B").ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(snap.reserve_b);
    (a, b)
}

// ── UPDATE_ORACLE instruction ─────────────────────────────────────────────────
//
// Data (41 bytes):
//   [0]       discriminant = 1
//   [1..9]    oracle_price  u64 le
//   [9..13]   spread_bps    u32 le
//   [13..17]  vol_adj       u32 le
//   [17..21]  k_param       u32 le
//   [21..25]  fee_bps       u32 le
//   [25..33]  target_a      u64 le
//   [33..41]  target_b      u64 le

fn send_update_oracle(
    cfg:          &Config,
    client:       &RpcClient,
    blockhash:    Hash,
    oracle_price: u64,
    spread_bps:   u32,
    vol_adj:      u32,
    target_a:     u64,
    target_b:     u64,
) -> Result<String> {
    let mut data = Vec::with_capacity(41);
    data.push(1u8);
    data.extend_from_slice(&oracle_price.to_le_bytes());
    data.extend_from_slice(&spread_bps.to_le_bytes());
    data.extend_from_slice(&vol_adj.to_le_bytes());
    data.extend_from_slice(&cfg.k_param.to_le_bytes());
    data.extend_from_slice(&cfg.fee_bps.to_le_bytes());
    data.extend_from_slice(&target_a.to_le_bytes());
    data.extend_from_slice(&target_b.to_le_bytes());

    let ix = Instruction {
        program_id: cfg.program_id,
        accounts: vec![
            AccountMeta::new(cfg.pool_pubkey, false),
            AccountMeta::new_readonly(cfg.wallet.pubkey(), true),
        ],
        data,
    };

    let mut tx = Transaction::new_unsigned(Message::new(&[ix], Some(&cfg.wallet.pubkey())));
    tx.sign(&[&cfg.wallet], blockhash);

    let sig = client.send_transaction(&tx).context("send_transaction")?;
    Ok(sig.to_string())
}
