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
        .route("/pnl",     get(pnl_handler))
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

async fn pnl_handler(State(ms): State<Arc<MetricsState>>) -> Json<serde_json::Value> {
    let book = ms.shared.lp_book.read().await;
    let mm   = ms.shared.mm_book.read().await;
    let pool = ms.shared.pool.read().await.clone();
    let uptime = ms.start_time.elapsed().as_secs();

    // Annualised fee yield estimate: fee_income / start_value * (31536000 / uptime)
    let annualised_fee_yield_bps = if uptime > 60 && book.start_value_b > 0.0 {
        (book.fee_income_b / book.start_value_b * 10_000.0
            * 31_536_000.0 / uptime as f64) as i64
    } else { 0 };

    // Market health: is the LP making money net of IL?
    let net_pnl_bps = book.pnl_bps;
    let health = if net_pnl_bps > 0 { "profitable" }
                 else if net_pnl_bps > -50 { "breakeven" }
                 else { "losing" };

    Json(serde_json::json!({
        "market_health": health,

        // ── LP position ───────────────────────────────────────────────────────
        "lp": {
            "start_value_b":    book.start_value_b,
            "pool_value_b":     book.pool_value_b,
            "fee_income_b":     book.fee_income_b,
            "total_value_b":    book.pool_value_b + book.fee_income_b,
            "hodl_value_b":     book.hodl_value_b,
        },

        // ── PnL breakdown ─────────────────────────────────────────────────────
        "pnl": {
            "net_pnl_bps":        net_pnl_bps,
            "il_bps":             book.il_bps,
            "fee_income_bps":     if book.start_value_b > 0.0 {
                                      (book.fee_income_b / book.start_value_b * 10_000.0) as i64
                                  } else { 0 },
            "annualised_fee_yield_bps": annualised_fee_yield_bps,
        },

        // ── Spread quality ────────────────────────────────────────────────────
        "spread": {
            "min_bps":  if book.min_spread_bps == u32::MAX { 0 } else { book.min_spread_bps },
            "max_bps":  book.max_spread_bps,
            "avg_bps":  format!("{:.2}", book.avg_spread_bps),
        },

        // ── Volume ────────────────────────────────────────────────────────────
        "volume": {
            "total_b":          book.volume_b,
            "oracle_updates":   book.updates,
            "uptime_s":         uptime,
            "vol_per_hour_b":   if uptime > 0 {
                                    book.volume_b / uptime as f64 * 3600.0
                                } else { 0.0 },
        },

        // ── Pool snapshot ─────────────────────────────────────────────────────
        "pool": {
            "reserve_a":     pool.reserve_a,
            "reserve_b":     pool.reserve_b,
            "fee_a":         pool.fee_a,
            "fee_b":         pool.fee_b,
            "oracle_price":  pool.oracle_price,
        },

        // ── MM trading arm ────────────────────────────────────────────────────
        "mm_trading": {
            "total_swaps":       mm.total_swaps,
            "total_rebalances":  mm.total_rebalances,
            "volume_b":          mm.volume_b,
            "gross_pnl_b":       mm.gross_pnl_b,
            "net_pnl_b":         mm.net_pnl_b(pool.oracle_price),
            "gas_lamports":      mm.gas_lamports,
            "slippage": {
                "avg_bps":   format!("{:.1}", mm.avg_slippage_bps),
                "worst_bps": mm.worst_slippage_bps,
                "best_bps":  mm.best_slippage_bps,
            },
            "last_swap": {
                "in_amount":    mm.last_in_amount,
                "out_amount":   mm.last_out_amount,
                "oracle_price": mm.last_oracle_price,
                "slippage_bps": mm.last_slippage_bps,
                "pnl_b":        mm.last_pnl_b,
            },
        },

        // ── Who makes money? ──────────────────────────────────────────────────
        "market_participants": {
            "traders": {
                "cost_bps":   format!("{:.2}", book.avg_spread_bps),
                "note": "pay avg_spread to swap — tighter than most CEX spot fees"
            },
            "lp": {
                "fee_income_bps": if book.start_value_b > 0.0 {
                                      (book.fee_income_b / book.start_value_b * 10_000.0) as i64
                                  } else { 0 },
                "il_bps": book.il_bps,
                "net_bps": net_pnl_bps,
                "note": "collect fee on every swap; IL from arbs is the cost"
            },
            "arbs": {
                "opportunity_bps": if book.il_bps > 0 { book.il_bps } else { 0 },
                "note": "profit from price moves within oracle update window"
            },
            "mm_bot": {
                "gross_pnl_b":  mm.gross_pnl_b,
                "net_pnl_b":    mm.net_pnl_b(pool.oracle_price),
                "swaps":        mm.total_swaps,
                "avg_slippage_bps": format!("{:.1}", mm.avg_slippage_bps),
                "note": "active inventory management via Jupiter; negative slippage = cost to rebalance"
            },
        },
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
