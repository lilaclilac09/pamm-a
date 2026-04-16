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

// ── LP PnL book — updated every oracle cycle ──────────────────────────────────

/// Tracks LP position value, fee income, and IL since bot start.
/// All values are denominated in token-B units (same scale as reserve_b).
#[derive(Default, Clone)]
pub struct LpBook {
    // ── seeded at startup ────────────────────────────────────────────────────
    pub start_reserve_a:  u64,
    pub start_reserve_b:  u64,
    pub start_oracle:     u64,  // oracle_price_1e9 at first snapshot
    pub start_value_b:    f64,  // reserve_b + reserve_a * start_oracle / 1e9

    // ── live ─────────────────────────────────────────────────────────────────
    /// Pool value at current oracle price (reserves only, no fees)
    pub pool_value_b:     f64,
    /// Accumulated protocol fees expressed in B-equivalent
    pub fee_income_b:     f64,
    /// Hodl value: what you'd have if you never provided liquidity
    pub hodl_value_b:     f64,
    /// PnL = pool_value + fee_income - start_value  (in bps of start_value)
    pub pnl_bps:          i64,
    /// IL = pool_value - hodl_value  (in bps of hodl_value; negative = LP worse)
    pub il_bps:           i64,
    /// Total estimated swap volume since start (in B units)
    pub volume_b:         f64,
    /// Number of oracle updates processed
    pub updates:          u64,
    /// Smallest spread seen (bps)
    pub min_spread_bps:   u32,
    /// Largest spread seen (bps)
    pub max_spread_bps:   u32,
    /// Running average spread (ema)
    pub avg_spread_bps:   f64,
}

impl LpBook {
    /// Called by oracle arm after every successful UPDATE_ORACLE.
    ///
    /// `snap`          — latest on-chain pool state
    /// `oracle`        — current oracle price (1e-9 units)
    /// `spread_bps`    — effective spread used this cycle
    /// `prev_reserve_a/b` — previous reserves, used to estimate swap volume
    pub fn update(
        &mut self,
        snap:           &PoolSnapshot,
        oracle:         u64,
        spread_bps:     u32,
        prev_reserve_a: u64,
        prev_reserve_b: u64,
    ) {
        let ora = oracle as f64 / 1_000_000_000.0;
        let ra  = snap.reserve_a as f64;
        let rb  = snap.reserve_b as f64;
        let fa  = snap.fee_a as f64;
        let fb  = snap.fee_b as f64;

        // ── Seed on first call ────────────────────────────────────────────────
        if self.start_oracle == 0 && snap.oracle_price > 0 {
            self.start_reserve_a = snap.reserve_a;
            self.start_reserve_b = snap.reserve_b;
            self.start_oracle    = oracle;
            self.start_value_b   = rb + ra * ora;
            self.min_spread_bps  = u32::MAX;
        }
        if self.start_value_b == 0.0 { return; }

        // ── Pool value at current price ───────────────────────────────────────
        // pool_value_b: the "LP share" only (reserves excl. unswept fees)
        self.pool_value_b = rb + ra * ora;
        self.fee_income_b = fb + fa * ora;

        // ── Hodl value: held the same tokens since start ──────────────────────
        self.hodl_value_b = self.start_reserve_b as f64
            + self.start_reserve_a as f64 * ora;

        // ── PnL = (pool + fees) vs starting value ─────────────────────────────
        let total_value = self.pool_value_b + self.fee_income_b;
        self.pnl_bps = ((total_value - self.start_value_b) / self.start_value_b
            * 10_000.0) as i64;

        // ── IL = pool_value vs what you'd have by just holding ────────────────
        // Positive il_bps = hodl did better (pool lost to arbs).
        // Negative il_bps = pool did better (skew restored by flow).
        self.il_bps = if self.hodl_value_b > 0.0 {
            ((self.hodl_value_b - self.pool_value_b) / self.hodl_value_b
                * 10_000.0) as i64
        } else { 0 };

        // ── Volume estimate: absolute change in reserve_a + reserve_b ────────
        // Each swap moves both reserves; sum the abs change in B terms.
        let delta_a = (snap.reserve_a as i64 - prev_reserve_a as i64).unsigned_abs();
        let delta_b = (snap.reserve_b as i64 - prev_reserve_b as i64).unsigned_abs();
        self.volume_b += delta_b as f64 + delta_a as f64 * ora;

        // ── Spread stats ──────────────────────────────────────────────────────
        self.updates += 1;
        if spread_bps < self.min_spread_bps { self.min_spread_bps = spread_bps; }
        if spread_bps > self.max_spread_bps { self.max_spread_bps = spread_bps; }
        // EWMA avg spread (α=0.1)
        self.avg_spread_bps = 0.1 * spread_bps as f64
            + 0.9 * self.avg_spread_bps;
    }
}

// ── MM trading book — updated by trading arm after every Jupiter swap ─────────

/// Tracks active market-making PnL: Jupiter swaps, rebalancing, gas costs.
/// All value fields in token-B units (same scale as reserve_b / lamports).
#[derive(Default, Clone)]
pub struct MmBook {
    // ── trade counters ────────────────────────────────────────────────────────
    pub total_swaps:         u64,
    pub total_rebalances:    u64,   // ADD/REMOVE_LIQUIDITY calls

    // ── volume ────────────────────────────────────────────────────────────────
    /// Total B-equivalent volume routed through Jupiter (sum of value_in_b)
    pub volume_b:            f64,

    // ── PnL vs oracle ─────────────────────────────────────────────────────────
    /// Sum of (value_out_b - value_in_b) across all swaps.
    /// Positive = executed better than oracle. Negative = paid slippage.
    pub gross_pnl_b:         f64,

    /// Total Jito tips paid (lamports). Does not include on-chain compute fees.
    pub gas_lamports:        u64,

    // ── slippage ──────────────────────────────────────────────────────────────
    pub worst_slippage_bps:  i64,   // most negative (worst)
    pub best_slippage_bps:   i64,   // most positive (best, often ~0)
    pub avg_slippage_bps:    f64,   // EWMA α=0.1

    // ── last swap ─────────────────────────────────────────────────────────────
    pub last_in_amount:      u64,
    pub last_out_amount:     u64,
    pub last_oracle_price:   u64,
    pub last_slippage_bps:   i64,
    pub last_pnl_b:          f64,
}

impl MmBook {
    /// Record a completed Jupiter swap.
    ///
    /// `a_to_b`: true when selling A for B (overweight A), false when B→A.
    /// `in_amount`: token units spent.
    /// `out_amount`: token units received (from Jupiter quote).
    /// `oracle_price`: pool oracle price (1e9-scaled, A per B).
    /// `jito_tip_lamports`: gas cost for this bundle.
    pub fn record_swap(
        &mut self,
        a_to_b:            bool,
        in_amount:         u64,
        out_amount:        u64,
        oracle_price:      u64,
        jito_tip_lamports: u64,
    ) {
        if oracle_price == 0 { return; }
        let ora = oracle_price as f64 / 1_000_000_000.0;

        // Convert both legs to B-equivalent
        let value_in_b = if a_to_b {
            in_amount as f64 * ora          // selling A: in_amount of A → B value
        } else {
            in_amount as f64               // selling B: in_amount is already B
        };
        let value_out_b = if a_to_b {
            out_amount as f64              // got B
        } else {
            out_amount as f64 * ora        // got A: convert to B
        };

        let pnl_b = value_out_b - value_in_b;
        // slippage_bps: negative = paid slippage (we got less than oracle value)
        let slippage_bps = if value_in_b > 0.0 {
            (pnl_b / value_in_b * 10_000.0) as i64
        } else { 0 };

        self.total_swaps       += 1;
        self.volume_b          += value_in_b;
        self.gross_pnl_b       += pnl_b;
        self.gas_lamports      += jito_tip_lamports;

        if slippage_bps < self.worst_slippage_bps || self.total_swaps == 1 {
            self.worst_slippage_bps = slippage_bps;
        }
        if slippage_bps > self.best_slippage_bps || self.total_swaps == 1 {
            self.best_slippage_bps = slippage_bps;
        }
        // EWMA avg slippage
        self.avg_slippage_bps = 0.1 * slippage_bps as f64 + 0.9 * self.avg_slippage_bps;

        self.last_in_amount    = in_amount;
        self.last_out_amount   = out_amount;
        self.last_oracle_price = oracle_price;
        self.last_slippage_bps = slippage_bps;
        self.last_pnl_b        = pnl_b;
    }

    pub fn record_rebalance(&mut self) {
        self.total_rebalances += 1;
    }

    /// Net PnL after gas. Gas is converted from lamports to B using oracle price.
    /// Returns `None` if oracle price is zero (can't convert gas to B).
    pub fn net_pnl_b(&self, oracle_price: u64) -> Option<f64> {
        if oracle_price == 0 { return None; }
        // 1 SOL = 1e9 lamports; oracle_price is A (SOL lamports) per B
        // gas_cost_b = gas_lamports / oracle_price * 1e9
        // (if B is USDC, oracle_price = SOL/USD scaled; if B is SOL, oracle_price = 1e9)
        let gas_b = self.gas_lamports as f64 / oracle_price as f64 * 1_000_000_000.0;
        Some(self.gross_pnl_b - gas_b)
    }
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

    /// LP PnL book — updated by oracle arm on every pool snapshot.
    pub lp_book: RwLock<LpBook>,

    /// MM trading book — updated by trading arm after every Jupiter swap.
    pub mm_book: RwLock<MmBook>,
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
            lp_book:                RwLock::new(LpBook::default()),
            mm_book:                RwLock::new(MmBook::default()),
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
        // Check whether the pool has *any* observations at all.
        // If every slot is zero the oracle arm hasn't run yet — this is a fresh
        // pool in its warm-up period.  Allow trading rather than blocking forever.
        let any_populated = twap_obs.iter().any(|(p, _)| *p > 0);
        if !any_populated {
            tracing::info!("is_price_sane: TWAP warm-up (no observations yet) — allowing trade");
            return true;
        }
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
