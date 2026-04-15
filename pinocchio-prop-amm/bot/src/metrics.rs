//! HTTP metrics server — axum-based.
//!
//! GET /health  → {"status": "running"|"halted", "uptime_s": N, ...}
//! GET /metrics → full JSON snapshot of both arms
//!
//! Bind port from `cfg.metrics_port` (default 3000).

use axum::{
    extract::State,
    response::Json,
    routing::get,
    Router,
};
use solana_sdk::signature::Signer;
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tracing::info;

use crate::{config::Config, risk::SharedState};

pub struct MetricsState {
    pub shared:     Arc<SharedState>,
    pub start_time: Instant,
    pub wallet_key: String,
}

pub async fn serve(cfg: Arc<Config>, state: Arc<SharedState>) {
    let ms = Arc::new(MetricsState {
        shared:     Arc::clone(&state),
        start_time: Instant::now(),
        wallet_key: cfg.wallet.pubkey().to_string(),
    });

    let app = Router::new()
        .route("/health",  get(health_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(Arc::clone(&ms));

    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.metrics_port));
    info!("metrics server: http://localhost:{}/metrics", cfg.metrics_port);

    let listener = TcpListener::bind(addr).await.expect("metrics bind failed");
    axum::serve(listener, app).await.expect("metrics server failed");
}

async fn health_handler(State(ms): State<Arc<MetricsState>>) -> Json<serde_json::Value> {
    let halted = ms.shared.is_halted();
    Json(serde_json::json!({
        "status":   if halted { "halted" } else { "running" },
        "uptime_s": ms.start_time.elapsed().as_secs(),
    }))
}

async fn metrics_handler(State(ms): State<Arc<MetricsState>>) -> Json<serde_json::Value> {
    let pool = ms.shared.pool.read().await.clone();
    let m    = ms.shared.metrics.read().await;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let last_update_age = if pool.last_update == i64::MIN {
        -1i64 // never updated
    } else {
        now - pool.last_update
    };

    Json(serde_json::json!({
        "wallet":  ms.wallet_key,
        "halted":  ms.shared.is_halted(),
        "uptime_s": ms.start_time.elapsed().as_secs(),

        "oracle": {
            "cycles":            m.oracle_cycles,
            "consecutive_errors": m.oracle_errors,
            "oracle_price_1e9":  pool.oracle_price,
            "last_update_age_s": last_update_age,
        },

        "pool": {
            "reserve_a":   pool.reserve_a,
            "reserve_b":   pool.reserve_b,
            "lp_supply":   pool.lp_supply,
            "fee_a":       pool.fee_a,
            "fee_b":       pool.fee_b,
        },

        "trading": {
            "cycles":            m.trade_cycles,
            "consecutive_errors": m.trade_errors,
            "inventory_ratio":   m.inventory_ratio,
            "daily_pnl_pct":     m.daily_pnl_pct,
        },
    }))
}
