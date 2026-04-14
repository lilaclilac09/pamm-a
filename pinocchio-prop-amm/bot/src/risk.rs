//! Shared runtime state — safe to pass across tokio tasks via Arc.

use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::RwLock;

// ── Pool layout constants (must match state.rs) ───────────────────────────────

pub const OFF_RESERVE_A:    usize = 48;
pub const OFF_RESERVE_B:    usize = 56;
pub const OFF_TARGET_A:     usize = 64;
pub const OFF_TARGET_B:     usize = 72;
pub const OFF_LP_SUPPLY:    usize = 80;
pub const OFF_FEE_A:        usize = 88;
pub const OFF_FEE_B:        usize = 96;
pub const OFF_ORACLE_PRICE: usize = 104;
pub const OFF_SPREAD_BPS:   usize = 112;
pub const OFF_LAST_UPDATE:  usize = 128;
pub const OFF_TWAP_CURSOR:  usize = 136;
pub const OFF_TWAP_OBS:     usize = 144;
pub const TWAP_SLOTS:       usize = 10;
pub const POOL_SIZE:        usize = 304;

// ── Pool snapshot written by oracle arm, read by trading arm ─────────────────

#[derive(Clone, Default)]
pub struct PoolSnapshot {
    pub reserve_a:    u64,
    pub reserve_b:    u64,
    pub lp_supply:    u64,
    pub fee_a:        u64,
    pub fee_b:        u64,
    pub oracle_price: u64,
    pub last_update:  i64,
    /// (price_1e9, unix_timestamp) — most recent TWAP_SLOTS oracle ticks
    pub twap_obs:     Vec<(u64, i64)>,
}

impl PoolSnapshot {
    /// Parse from raw on-chain account data.
    pub fn from_bytes(d: &[u8]) -> Option<Self> {
        if d.len() < POOL_SIZE { return None; }

        let r64 = |off: usize| u64::from_le_bytes(d[off..off+8].try_into().unwrap());
        let ri64 = |off: usize| i64::from_le_bytes(d[off..off+8].try_into().unwrap());

        let cursor = d[OFF_TWAP_CURSOR] as usize;
        let _ = cursor; // stored but not used for parsing — we read all slots
        let mut twap_obs = Vec::with_capacity(TWAP_SLOTS);
        for i in 0..TWAP_SLOTS {
            let base = OFF_TWAP_OBS + i * 16;
            twap_obs.push((r64(base), ri64(base + 8)));
        }

        Some(PoolSnapshot {
            reserve_a:    r64(OFF_RESERVE_A),
            reserve_b:    r64(OFF_RESERVE_B),
            lp_supply:    r64(OFF_LP_SUPPLY),
            fee_a:        r64(OFF_FEE_A),
            fee_b:        r64(OFF_FEE_B),
            oracle_price: r64(OFF_ORACLE_PRICE),
            last_update:  ri64(OFF_LAST_UPDATE),
            twap_obs,
        })
    }
}

// ── Metrics counters visible to /metrics endpoint ────────────────────────────

#[derive(Default)]
pub struct MetricsSnapshot {
    pub oracle_cycles:   u64,
    pub trade_cycles:    u64,
    pub oracle_errors:   u32,
    pub trade_errors:    u32,
    pub inventory_ratio: f64,
    pub last_spread_bps: u32,
    pub daily_pnl_pct:   f64,
}

// ── Shared state ──────────────────────────────────────────────────────────────

pub struct SharedState {
    /// Set to true when either circuit breaker trips. Both arms check this.
    pub halted: AtomicBool,

    /// Current pool view (oracle arm writes after every UPDATE_ORACLE).
    pub pool: RwLock<PoolSnapshot>,

    /// Metrics exposed at /metrics.
    pub metrics: RwLock<MetricsSnapshot>,

    /// Consecutive failures for oracle arm — reset on success.
    pub oracle_failures: AtomicU32,

    /// Consecutive failures for trading arm — reset on success.
    pub trade_failures: AtomicU32,

    /// Balance at bot start (lamports), for daily PnL tracking.
    pub start_balance_lamports: AtomicU64,
}

impl SharedState {
    pub fn new(start_balance: u64) -> Arc<Self> {
        Arc::new(SharedState {
            halted:                 AtomicBool::new(false),
            pool:                   RwLock::new(PoolSnapshot::default()),
            metrics:                RwLock::new(MetricsSnapshot::default()),
            oracle_failures:        AtomicU32::new(0),
            trade_failures:         AtomicU32::new(0),
            start_balance_lamports: AtomicU64::new(start_balance),
        })
    }

    pub fn is_halted(&self) -> bool {
        self.halted.load(Ordering::SeqCst)
    }

    pub fn halt(&self, reason: &str) {
        tracing::error!("HALT: {}", reason);
        self.halted.store(true, Ordering::SeqCst);
    }
}

// ── TWAP sanity check (Phase 3: manipulation filter) ─────────────────────────

/// Returns `true` if `jupiter_price` is within `max_deviation_bps` of the
/// TWAP built from observations that are less than `max_age_secs` old.
///
/// Returns `false` (skip trade) if:
/// - no valid (non-zero, non-stale) observations exist
/// - the most recent observation is older than `max_age_secs`
/// - deviation exceeds the threshold
pub fn is_price_sane(
    jupiter_price:       u64,
    twap_obs:            &[(u64, i64)],
    max_deviation_bps:   u32,
    max_age_secs:        i64,
    now:                 i64,
) -> bool {
    // Only observations that are fresh and have a non-zero price count.
    let valid: Vec<u64> = twap_obs
        .iter()
        .filter(|(p, ts)| *p > 0 && *ts > 0 && (now - ts).abs() < max_age_secs)
        .map(|(p, _)| *p)
        .collect();

    if valid.is_empty() {
        tracing::warn!("is_price_sane: no valid TWAP observations — skipping trade");
        return false;
    }

    // Check that the newest observation isn't stale.
    let newest_ts = twap_obs
        .iter()
        .filter(|(p, ts)| *p > 0 && *ts > 0)
        .map(|(_, ts)| *ts)
        .max()
        .unwrap_or(0);

    if now - newest_ts > max_age_secs {
        tracing::warn!(
            "is_price_sane: newest TWAP obs is {}s old (max {}) — skipping",
            now - newest_ts,
            max_age_secs
        );
        return false;
    }

    let twap = valid.iter().sum::<u64>() / valid.len() as u64;
    if twap == 0 { return false; }

    let deviation_bps = jupiter_price.abs_diff(twap) * 10_000 / twap;
    if deviation_bps > max_deviation_bps as u64 {
        tracing::warn!(
            "is_price_sane: Jupiter price {} deviates {}bps from TWAP {} (max {}bps)",
            jupiter_price, deviation_bps, twap, max_deviation_bps
        );
        return false;
    }

    true
}
