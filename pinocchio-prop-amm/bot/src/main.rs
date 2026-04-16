//! Unified Prop Trading Bot
//!
//! Spawns three concurrent tokio tasks:
//!   - oracle_arm  : fetches Pyth every UPDATE_INTERVAL_MS, calls UPDATE_ORACLE
//!   - trading_arm : Jupiter swaps + ADD/REMOVE rebalancing every TRADE_INTERVAL_MS
//!   - metrics     : HTTP /health /metrics on METRICS_PORT
//!
//! Both arms share a circuit breaker (SharedState::halted). When either arm
//! trips the breaker, both pause until the process is restarted.
//!
//! Graceful shutdown: SIGTERM or Ctrl-C sets `halted`, waits up to 10s for
//! in-flight cycles to finish (they check `is_halted()` at loop top), then exits.

mod config;
mod cu_budget;
mod humidifi_arm;
mod metrics;
mod oracle_arm;
mod pyth;
mod risk;
mod trading_arm;

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::signature::Signer;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Unified Prop Bot starting...");

    let cfg = Arc::new(config::Config::from_env().context("config load failed")?);

    // Seed daily-PnL tracking with current balance
    let client = RpcClient::new(cfg.rpc_url.clone());
    let start_lamports = client
        .get_balance(&cfg.wallet.pubkey())
        .context("initial balance fetch")?;

    let state = risk::SharedState::new(start_lamports);

    info!("RPC:          {}", cfg.rpc_url);
    info!("Pool:         {}", cfg.pool_pubkey);
    info!("Program:      {}", cfg.program_id);
    info!("Oracle every: {}ms  Trade every: {}ms",
        cfg.update_interval_ms, cfg.trade_interval_ms);
    info!("Metrics:      http://localhost:{}/metrics", cfg.metrics_port);

    let cfg_a = Arc::clone(&cfg);
    let cfg_t = Arc::clone(&cfg);
    let cfg_h = Arc::clone(&cfg);
    let cfg_m = Arc::clone(&cfg);
    let st_a  = Arc::clone(&state);
    let st_t  = Arc::clone(&state);
    let st_h  = Arc::clone(&state);
    let st_m  = Arc::clone(&state);

    let oracle   = tokio::spawn(async move { oracle_arm::run(cfg_a, st_a).await });
    let trading  = tokio::spawn(async move { trading_arm::run(cfg_t, st_t).await });
    let humidifi = tokio::spawn(async move { humidifi_arm::run(cfg_h, st_h).await });
    let metrx    = tokio::spawn(async move { metrics::serve(cfg_m, st_m).await });

    // Wait for a shutdown signal or for any task to exit unexpectedly.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("SIGINT received — shutting down gracefully...");
        }
        r = oracle   => { tracing::error!("oracle_arm exited unexpectedly: {:?}",   r); }
        r = trading  => { tracing::error!("trading_arm exited unexpectedly: {:?}",  r); }
        r = humidifi => { tracing::error!("humidifi_arm exited unexpectedly: {:?}", r); }
        r = metrx    => { tracing::error!("metrics exited unexpectedly: {:?}",      r); }
    }

    // Signal both arms to stop at their next loop iteration.
    state.halt("shutdown signal");

    // Give in-flight cycles up to 10s to finish cleanly.
    info!("waiting up to 10s for tasks to finish...");
    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
    info!("exiting.");
    Ok(())
}
