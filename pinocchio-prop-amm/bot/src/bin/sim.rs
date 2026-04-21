//! PMM parameter sweeper — pure Rust, no RPC, no wallet needed.
//!
//! Mirrors the on-chain PMM pricing math from lib.rs and simulates arb
//! behaviour across a grid of (base_spread_bps × k_param) combinations.
//!
//! Run:
//!   cargo run --bin sim
//!   cargo run --bin sim -- --rounds 500 --price-vol 80
//!
//! Output (sorted by fee/IL ratio):
//!   spread  k      fees_a   fees_b   pnl_bps  il_bps  arb_cnt  ratio


// ── Price series generator ────────────────────────────────────────────────────

/// Simple seeded LCG for deterministic runs.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn next_f64(&mut self) -> f64 { (self.next() >> 11) as f64 / (1u64 << 53) as f64 }
    /// Box–Muller: returns a standard-normal draw.
    fn randn(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-15);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Generate a price series (in oracle 1e-9 units, i.e. lamports per token_b).
/// vol_bps: annualised vol in bps. dt: time step in seconds.
fn price_series(start: u64, rounds: usize, vol_bps: u32, dt_secs: f64, seed: u64) -> Vec<u64> {
    let mut rng = Lcg(seed);
    let sigma_per_step = (vol_bps as f64 / 10_000.0) * (dt_secs / 31_536_000.0_f64).sqrt();
    let mut price = start as f64;
    let mut series = Vec::with_capacity(rounds + 1);
    series.push(start);
    for _ in 0..rounds {
        price *= (sigma_per_step * rng.randn()).exp();
        price = price.max(1.0);
        series.push(price as u64);
    }
    series
}

// ── PMM pricing mirror (from lib.rs swap()) ───────────────────────────────────

fn pmm_quote_a_to_b(
    reserve_a: u64, reserve_b: u64,
    target_a: u64, target_b: u64,
    oracle_price: u64,
    base_spread_bps: u32, vol_adj: u32, k_param: u32, fee_bps: u32,
    amount_in: u64,
) -> Option<(u64, u64)> {
    if oracle_price == 0 || reserve_a == 0 && reserve_b == 0 { return None; }

    let effective_spread = base_spread_bps + vol_adj;

    let value_a: u128 = (reserve_a as u128 * oracle_price as u128) / 1_000_000_000;
    let value_b: u128 = reserve_b as u128;
    let total_value = value_a + value_b;

    let current_ratio: i64 = if total_value == 0 { 5000 }
        else { (value_a * 10_000 / total_value) as i64 };

    let target_value_a: u128 = (target_a as u128 * oracle_price as u128) / 1_000_000_000;
    let target_value_b: u128 = target_b as u128;
    let target_total = target_value_a + target_value_b;

    let target_ratio: i64 = if target_total == 0 { 5000 }
        else { (target_value_a * 10_000 / target_total) as i64 };

    let deviation: i64 = current_ratio - target_ratio;
    let skew_mag = ((deviation * deviation / 8000) as u64)
        .min(effective_spread as u64) as u32;

    let ask_spread = effective_spread + skew_mag;

    let impact_bps = if reserve_a == 0 { 0u32 } else {
        ((k_param as u128 * amount_in as u128 / reserve_a as u128).min(10_000)) as u32
    };

    let ask_price = (oracle_price as u128)
        .saturating_mul(10_000 + ask_spread as u128) / 10_000;
    let ask_impact = ask_price
        .saturating_mul(10_000 + impact_bps as u128) / 10_000;
    if ask_impact == 0 { return None; }

    let out_raw = (amount_in as u128 * 1_000_000_000) / ask_impact;
    let fee = out_raw * fee_bps as u128 / 10_000;
    let actual = out_raw.saturating_sub(fee);
    Some((actual as u64, fee as u64))
}

fn pmm_quote_b_to_a(
    reserve_a: u64, reserve_b: u64,
    target_a: u64, target_b: u64,
    oracle_price: u64,
    base_spread_bps: u32, vol_adj: u32, k_param: u32, fee_bps: u32,
    amount_in: u64,
) -> Option<(u64, u64)> {
    if oracle_price == 0 || reserve_a == 0 && reserve_b == 0 { return None; }

    let effective_spread = base_spread_bps + vol_adj;

    let value_a: u128 = (reserve_a as u128 * oracle_price as u128) / 1_000_000_000;
    let value_b: u128 = reserve_b as u128;
    let total_value = value_a + value_b;

    let current_ratio: i64 = if total_value == 0 { 5000 }
        else { (value_a * 10_000 / total_value) as i64 };

    let target_value_a: u128 = (target_a as u128 * oracle_price as u128) / 1_000_000_000;
    let target_value_b: u128 = target_b as u128;
    let target_total = target_value_a + target_value_b;

    let target_ratio: i64 = if target_total == 0 { 5000 }
        else { (target_value_a * 10_000 / target_total) as i64 };

    let deviation: i64 = current_ratio - target_ratio;
    let skew_mag = ((deviation * deviation / 8000) as u64)
        .min(effective_spread as u64) as u32;

    let bid_spread = effective_spread.saturating_sub(skew_mag);
    let bid_safe = bid_spread.min(10_000);

    let impact_bps = if reserve_b == 0 { 0u32 } else {
        ((k_param as u128 * amount_in as u128 / reserve_b as u128).min(10_000)) as u32
    };

    let bid_price = (oracle_price as u128)
        .saturating_mul(10_000u128.saturating_sub(bid_safe as u128)) / 10_000;
    let bid_impact = bid_price
        .saturating_mul(10_000u128.saturating_sub(impact_bps as u128)) / 10_000;

    let out_raw = (amount_in as u128 * bid_impact) / 1_000_000_000;
    let fee = out_raw * fee_bps as u128 / 10_000;
    let actual = out_raw.saturating_sub(fee);
    Some((actual as u64, fee as u64))
}

// ── MEV auction ───────────────────────────────────────────────────────────────
//
// Models the Jito tip auction that fires every time oracle_arm is about to
// update the pool price.  Timeline per round:
//
//   slot N:   Pyth publishes new market_price (all searchers see it)
//   slot N+1: oracle_arm tx lands → pool_oracle updated
//
// During the gap (slot N), N_SEARCHERS each independently compute:
//   gross_profit_b = pmm_quote(stale pool) - fair_value(market_price)
//
// Each searcher bids:
//   tip = aggressiveness_i * gross_profit_b
// where aggressiveness_i ~ Uniform[0.5, 0.95] (rational: keep some profit)
//
// Winner = highest tip.  Winner executes the swap against the stale pool.
// tip goes to validator (not LP) → tracked as mev_tips_b.
// LP suffers the IL without capturing the tip.
//
// Swap execution = same as before: update reserves in memory (no RPC).

fn mev_auction(
    reserve_a: &mut u64,
    reserve_b: &mut u64,
    fees_a: &mut u64,
    fees_b: &mut u64,
    target_a: u64,
    target_b: u64,
    stale_oracle: u64,   // what the pool still thinks the price is
    market_price: u64,   // what Pyth just published (not yet on-chain)
    spread_bps: u32,
    vol_adj: u32,
    k_param: u32,
    fee_bps: u32,
    swap_size_bps: u32,
    n_searchers: u32,
    rng: &mut Lcg,
) -> Option<MevResult> {
    if stale_oracle == 0 || market_price == 0 { return None; }

    let price_delta_bps = market_price.abs_diff(stale_oracle)
        .saturating_mul(10_000) / stale_oracle;

    // No profitable arb if move is within the spread
    if price_delta_bps <= spread_bps as u64 { return None; }

    let swap_a = (*reserve_a as u128 * swap_size_bps as u128 / 10_000).max(1) as u64;
    let swap_b = (*reserve_b as u128 * swap_size_bps as u128 / 10_000).max(1) as u64;

    // oracle_price = A-per-B (scaled 1e9).
    //
    // MEV window: pool still uses STALE oracle, market has already moved.
    //
    // market > stale: stale oracle is LOW → pool quotes A→B using cheap stale price
    //   → pool gives arber MORE B than fair → arb sells A, gets cheap B (A→B)
    //
    // market < stale: stale oracle is HIGH → pool quotes B→A using expensive stale price
    //   → pool gives arber MORE A than fair → arb sells B, gets cheap A (B→A)
    let (direction, gross_profit_b) = if market_price > stale_oracle {
        // A→B: pool's stale oracle undervalues B → arb buys B cheaply
        let Some((pmm_b, _fee)) = pmm_quote_a_to_b(
            *reserve_a, *reserve_b, target_a, target_b,
            stale_oracle, spread_bps, vol_adj, k_param, fee_bps, swap_a,
        ) else { return None; };
        let fair_b = (swap_a as u128 * 1_000_000_000 / market_price as u128) as u64;
        if pmm_b <= fair_b { return None; }
        (0u8, (pmm_b - fair_b) as f64)
    } else {
        // B→A: pool's stale oracle overvalues B → arb buys A cheaply
        let Some((pmm_a, _fee)) = pmm_quote_b_to_a(
            *reserve_a, *reserve_b, target_a, target_b,
            stale_oracle, spread_bps, vol_adj, k_param, fee_bps, swap_b,
        ) else { return None; };
        let fair_a = (swap_b as u128 * stale_oracle as u128 / market_price as u128) as u64;
        if pmm_a <= fair_a { return None; }
        // profit in B units: extra A received × market price
        let profit_b = (pmm_a - fair_a) as f64 * market_price as f64 / 1_000_000_000.0;
        (1u8, profit_b)
    };

    if gross_profit_b <= 0.0 { return None; }

    // Each searcher bids aggressiveness_i × gross_profit
    // aggressiveness ~ Uniform[0.50, 0.95]: rational actors keep some upside
    let mut best_tip = 0.0f64;
    for _ in 0..n_searchers {
        let aggr = 0.50 + rng.next_f64() * 0.45; // [0.50, 0.95)
        let bid = aggr * gross_profit_b;
        if bid > best_tip { best_tip = bid; }
    }

    // Winner executes swap against the stale pool
    let (executed, fee_captured) = if direction == 0 {
        match pmm_quote_a_to_b(
            *reserve_a, *reserve_b, target_a, target_b,
            stale_oracle, spread_bps, vol_adj, k_param, fee_bps, swap_a,
        ) {
            Some((out_b, fee)) if out_b > 0 && out_b < *reserve_b => {
                *reserve_a += swap_a;
                *reserve_b = reserve_b.saturating_sub(out_b);
                *fees_b += fee;
                (true, fee as f64)
            }
            _ => (false, 0.0),
        }
    } else {
        match pmm_quote_b_to_a(
            *reserve_a, *reserve_b, target_a, target_b,
            stale_oracle, spread_bps, vol_adj, k_param, fee_bps, swap_b,
        ) {
            Some((out_a, fee)) if out_a > 0 && out_a < *reserve_a => {
                *reserve_b += swap_b;
                *reserve_a = reserve_a.saturating_sub(out_a);
                *fees_a += fee;
                (true, fee as f64 * stale_oracle as f64 / 1_000_000_000.0)
            }
            _ => (false, 0.0),
        }
    };

    if !executed { return None; }

    Some(MevResult {
        tip_b:          best_tip,
        searcher_net_b: gross_profit_b - best_tip,
        gross_profit_b,
        fee_to_lp_b:    fee_captured,
        n_searchers,
    })
}

#[derive(Default, Clone)]
struct MevResult {
    tip_b:          f64,  // paid to validator
    searcher_net_b: f64,  // searcher kept
    gross_profit_b: f64,  // total arb value extracted
    fee_to_lp_b:    f64,  // fee portion that does go to LP
    n_searchers:    u32,
}

// ── Sandwich MEV ──────────────────────────────────────────────────────────────
//
// Models Jito-bundle sandwiches around uninformed retail flow.  Timeline:
//
//   tx[0] frontrun:  sandwicher trades SAME direction as victim, nudging
//                    price against them (observed in pending bundle).
//   tx[1] victim:    retail order executes at the worse price.
//   tx[2] backrun:   sandwicher immediately reverses, harvesting the spread.
//
// Profit = backrun_proceeds - frontrun_cost (zero-fee at the margin for arber
//          because Jito bundles don't pay per-swap fees to LP on their legs
//          — but here we model both legs paying the normal pool fee so the LP
//          actually gets MORE fee revenue while the victim is hurt).
//
// The sandwicher only executes if the round-trip is profitable.  If not, we
// snapshot-and-restore reserves and let the victim trade normally.

struct SandwichResult {
    profit_b:          f64,  // sandwicher net profit (B-denominated)
    victim_slippage_b: f64,  // extra output victim lost vs no-sandwich (B units)
}

fn sandwich_attack(
    reserve_a: &mut u64, reserve_b: &mut u64,
    fees_a: &mut u64, fees_b: &mut u64,
    target_a: u64, target_b: u64, oracle: u64,
    spread_bps: u32, vol_adj: u32, k_param: u32, fee_bps: u32,
    victim_amount: u64,
    is_atob: bool,
    rng: &mut Lcg,
) -> Option<SandwichResult> {
    // Frontrun size: random [0.3×, 1.5×] victim size — rational sizing
    let fr_mult = 0.30 + rng.next_f64() * 1.20;
    let snapshot = (*reserve_a, *reserve_b, *fees_a, *fees_b);

    let result = if is_atob {
        // Victim is buying B (A→B).  Frontrun: also buy B to push price up.
        let fr_a = ((victim_amount as f64 * fr_mult) as u64).max(1);

        // Baseline output victim would receive without sandwich
        let baseline_b = pmm_quote_a_to_b(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, victim_amount,
        )?.0;

        // ── frontrun ──────────────────────────────────────────────────────────
        let (fr_b, fr_fee) = pmm_quote_a_to_b(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, fr_a,
        ).filter(|(out, _)| *out > 0 && *out < *reserve_b)?;
        *reserve_a += fr_a;
        *reserve_b  = reserve_b.saturating_sub(fr_b);
        *fees_b    += fr_fee;

        // ── victim ────────────────────────────────────────────────────────────
        let (victim_b, v_fee) = pmm_quote_a_to_b(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, victim_amount,
        ).filter(|(out, _)| *out > 0 && *out < *reserve_b)?;
        *reserve_a += victim_amount;
        *reserve_b  = reserve_b.saturating_sub(victim_b);
        *fees_b    += v_fee;

        let victim_slippage_b = baseline_b.saturating_sub(victim_b) as f64;

        // ── backrun: sell fr_b back ───────────────────────────────────────────
        let (back_a, back_fee) = pmm_quote_b_to_a(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, fr_b,
        ).filter(|(out, _)| *out > 0 && *out < *reserve_a)?;
        *reserve_b += fr_b;
        *reserve_a  = reserve_a.saturating_sub(back_a);
        *fees_a    += back_fee;

        let profit_a = back_a as i64 - fr_a as i64;
        let profit_b = profit_a as f64 * oracle as f64 / 1_000_000_000.0;
        SandwichResult { profit_b, victim_slippage_b }
    } else {
        // Victim is buying A (B→A).  Frontrun: also buy A to push price up.
        let fr_b = ((victim_amount as f64 * fr_mult) as u64).max(1);

        let baseline_a = pmm_quote_b_to_a(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, victim_amount,
        )?.0;

        // ── frontrun ──────────────────────────────────────────────────────────
        let (fr_a, fr_fee) = pmm_quote_b_to_a(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, fr_b,
        ).filter(|(out, _)| *out > 0 && *out < *reserve_a)?;
        *reserve_b += fr_b;
        *reserve_a  = reserve_a.saturating_sub(fr_a);
        *fees_a    += fr_fee;

        // ── victim ────────────────────────────────────────────────────────────
        let (victim_a, v_fee) = pmm_quote_b_to_a(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, victim_amount,
        ).filter(|(out, _)| *out > 0 && *out < *reserve_a)?;
        *reserve_b += victim_amount;
        *reserve_a  = reserve_a.saturating_sub(victim_a);
        *fees_a    += v_fee;

        let victim_slippage_b = baseline_a.saturating_sub(victim_a) as f64
            * oracle as f64 / 1_000_000_000.0;

        // ── backrun: sell fr_a back ───────────────────────────────────────────
        let (back_b, back_fee) = pmm_quote_a_to_b(
            *reserve_a, *reserve_b, target_a, target_b,
            oracle, spread_bps, vol_adj, k_param, fee_bps, fr_a,
        ).filter(|(out, _)| *out > 0 && *out < *reserve_b)?;
        *reserve_a += fr_a;
        *reserve_b  = reserve_b.saturating_sub(back_b);
        *fees_b    += back_fee;

        let profit_b = back_b as i64 - fr_b as i64;
        SandwichResult { profit_b: profit_b as f64, victim_slippage_b }
    };

    if result.profit_b <= 0.0 {
        // Not profitable — undo everything and let victim trade normally
        (*reserve_a, *reserve_b, *fees_a, *fees_b) = snapshot;
        return None;
    }
    Some(result)
}

// ── Market simulation ─────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct SimResult {
    spread_bps: u32,
    k_param:    u32,
    fees_a:     u64,   // accrued token-A fees
    fees_b:     u64,   // accrued token-B fees
    arb_count:  u32,
    pnl_bps:    i64,   // (end_value - start_value) / start_value * 10_000
    il_bps:     i64,   // (hodl_value - pool_value) / hodl_value * 10_000 (positive = pool worse)
    // MEV oracle-lag auction fields
    mev_arb_count:      u32,
    mev_tips_b:         f64,  // total validator tips (value leaving the system)
    mev_searcher_net_b: f64,  // what searchers kept after tips
    mev_gross_b:        f64,  // total arb value extracted via MEV window
    // Sandwich fields
    sandwich_count:     u32,
    sandwich_profit_b:  f64,  // sandwicher net (B units)
    victim_slippage_b:  f64,  // extra slippage paid by victims (B units)
}

fn simulate(
    base_spread_bps: u32,
    k_param: u32,
    fee_bps: u32,
    vol_adj: u32,
    initial_oracle: u64,
    initial_reserve_a: u64,
    initial_reserve_b: u64,
    swap_size_bps: u32,
    price_series: &[u64],
    seed: u64,
    n_searchers: u32,   // 0 = no MEV model (old behaviour), >0 = auction
    oracle_lag: u32,    // oracle updates every N rounds (1 = every round, 3 = every 3rd, etc.)
) -> SimResult {
    let mut rng = Lcg(seed.wrapping_add(0xabc123));
    let mut reserve_a = initial_reserve_a;
    let mut reserve_b = initial_reserve_b;
    let target_a = initial_reserve_a;
    let target_b = initial_reserve_b;
    let mut fees_a = 0u64;
    let mut fees_b = 0u64;
    let mut arb_count = 0u32;

    let mut mev_arb_count      = 0u32;
    let mut mev_tips_b         = 0.0f64;
    let mut mev_searcher_net_b = 0.0f64;
    let mut mev_gross_b        = 0.0f64;
    let mut sandwich_count     = 0u32;
    let mut sandwich_profit_b  = 0.0f64;
    let mut victim_slippage_b  = 0.0f64;

    let start_val_b = reserve_b as f64
        + reserve_a as f64 * initial_oracle as f64 / 1_000_000_000.0;

    let hodl_a = initial_reserve_a as f64;
    let hodl_b = initial_reserve_b as f64;

    // pool_oracle = price the pool currently knows.
    // Updates only every oracle_lag rounds (simulates MIN_ORACLE_UPDATE_BPS filter
    // + tx confirmation latency — stale gap lets MEV arbs accumulate).
    let mut pool_oracle = initial_oracle;
    let mut round_idx: u32 = 0;

    for &market_price in price_series.iter() {
        round_idx += 1;
        if market_price == 0 { continue; }

        // ── Uninformed retail flow (70% probability, random direction) ─────────
        let flow_roll = rng.next() % 100;
        if flow_roll < 70 {
            let size_mult = 1 + (rng.next() % 10) as u32;
            let flow_size_bps = swap_size_bps * size_mult / 10;
            let flow_a = (reserve_a as u128 * flow_size_bps as u128 / 10_000).max(1) as u64;
            let flow_b = (reserve_b as u128 * flow_size_bps as u128 / 10_000).max(1) as u64;
            let is_atob = rng.next() % 2 == 0;

            // Sandwich: 40% of retail orders get targeted when MEV is enabled.
            // sandwich_attack() snapshot-restores if not profitable, then falls
            // through to the normal trade below.
            let sandwiched = n_searchers > 0 && rng.next() % 100 < 40 && {
                let victim_size = if is_atob { flow_a } else { flow_b };
                match sandwich_attack(
                    &mut reserve_a, &mut reserve_b, &mut fees_a, &mut fees_b,
                    target_a, target_b, pool_oracle,
                    base_spread_bps, vol_adj, k_param, fee_bps,
                    victim_size, is_atob, &mut rng,
                ) {
                    Some(sw) => {
                        sandwich_count    += 1;
                        sandwich_profit_b += sw.profit_b;
                        victim_slippage_b += sw.victim_slippage_b;
                        true  // victim trade already executed inside sandwich_attack
                    }
                    None => false,  // not profitable; execute normal below
                }
            };

            if !sandwiched {
                if is_atob {
                    if let Some((out_b, fee)) = pmm_quote_a_to_b(
                        reserve_a, reserve_b, target_a, target_b,
                        pool_oracle, base_spread_bps, vol_adj, k_param, fee_bps, flow_a,
                    ) {
                        if out_b > 0 && out_b < reserve_b {
                            reserve_a += flow_a;
                            reserve_b = reserve_b.saturating_sub(out_b);
                            fees_b += fee;
                        }
                    }
                } else {
                    if let Some((out_a, fee)) = pmm_quote_b_to_a(
                        reserve_a, reserve_b, target_a, target_b,
                        pool_oracle, base_spread_bps, vol_adj, k_param, fee_bps, flow_b,
                    ) {
                        if out_a > 0 && out_a < reserve_a {
                            reserve_b += flow_b;
                            reserve_a = reserve_a.saturating_sub(out_a);
                            fees_a += fee;
                        }
                    }
                }
            }
        }

        // ── Arb / MEV window ───────────────────────────────────────────────────
        //
        // pool_oracle is the STALE price (from last round).
        // market_price is what Pyth just published.
        //
        // MEV mode (n_searchers > 0):
        //   N searchers race to exploit the stale pool BEFORE oracle updates.
        //   Highest tip wins; tip goes to validator, not LP.
        //
        // No-MEV mode (n_searchers == 0):
        //   Single arber fires instantly when move > spread.
        //   Baseline for comparison.

        let price_delta_bps = market_price.abs_diff(pool_oracle)
            .saturating_mul(10_000) / pool_oracle.max(1);

        if price_delta_bps > base_spread_bps as u64 {
            if n_searchers > 0 {
                if let Some(mev) = mev_auction(
                    &mut reserve_a, &mut reserve_b,
                    &mut fees_a, &mut fees_b,
                    target_a, target_b,
                    pool_oracle, market_price,
                    base_spread_bps, vol_adj, k_param, fee_bps,
                    swap_size_bps, n_searchers, &mut rng,
                ) {
                    mev_arb_count      += 1;
                    mev_tips_b         += mev.tip_b;
                    mev_searcher_net_b += mev.searcher_net_b;
                    mev_gross_b        += mev.gross_profit_b;
                    arb_count          += 1;
                }
            } else {
                // Baseline: single arber, no tip competition
                let swap_a = (reserve_a as u128 * swap_size_bps as u128 / 10_000).max(1) as u64;
                let swap_b = (reserve_b as u128 * swap_size_bps as u128 / 10_000).max(1) as u64;

                if market_price > pool_oracle {
                    if let Some((out_a, fee)) = pmm_quote_b_to_a(
                        reserve_a, reserve_b, target_a, target_b,
                        pool_oracle, base_spread_bps, vol_adj, k_param, fee_bps, swap_b,
                    ) {
                        if out_a > 0 && out_a < reserve_a {
                            reserve_b += swap_b;
                            reserve_a = reserve_a.saturating_sub(out_a);
                            fees_a += fee;
                            arb_count += 1;
                        }
                    }
                } else {
                    if let Some((out_b, fee)) = pmm_quote_a_to_b(
                        reserve_a, reserve_b, target_a, target_b,
                        pool_oracle, base_spread_bps, vol_adj, k_param, fee_bps, swap_a,
                    ) {
                        if out_b > 0 && out_b < reserve_b {
                            reserve_a += swap_a;
                            reserve_b = reserve_b.saturating_sub(out_b);
                            fees_b += fee;
                            arb_count += 1;
                        }
                    }
                }
            }
        }

        // oracle updates only every oracle_lag rounds
        if round_idx % oracle_lag.max(1) == 0 {
            pool_oracle = market_price;
        }
    }

    let final_price = *price_series.last().unwrap_or(&initial_oracle);

    let pool_val = reserve_b as f64
        + reserve_a as f64 * final_price as f64 / 1_000_000_000.0;
    let pnl_bps = ((pool_val - start_val_b) / start_val_b * 10_000.0) as i64;

    let hodl_val = hodl_b + hodl_a * final_price as f64 / 1_000_000_000.0;
    let il_bps = if hodl_val > 0.0 {
        ((hodl_val - pool_val) / hodl_val * 10_000.0) as i64
    } else { 0 };

    SimResult {
        spread_bps: base_spread_bps, k_param,
        fees_a, fees_b, arb_count, pnl_bps, il_bps,
        mev_arb_count, mev_tips_b, mev_searcher_net_b, mev_gross_b,
        sandwich_count, sandwich_profit_b, victim_slippage_b,
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn run_sweep(
    label: &str,
    rounds: usize, price_vol: u32, fee_bps: u32, vol_adj: u32,
    swap_size: u32, dt_secs: f64, n_searchers: u32, oracle_lag: u32,
    spreads: &[u32], k_params: &[u32], seeds: &[u64],
    initial_oracle: u64, initial_reserve_a: u64, initial_reserve_b: u64,
) -> Vec<(f64, SimResult)> {
    let mut results: Vec<(f64, SimResult)> = Vec::new();

    for &spread in spreads {
        for &k in k_params {
            let mut total_fees_b         = 0u64;
            let mut total_fees_a         = 0u64;
            let mut total_arbs           = 0u32;
            let mut total_pnl            = 0i64;
            let mut total_il             = 0i64;
            let mut total_mev_arbs       = 0u32;
            let mut total_mev_tips       = 0.0f64;
            let mut total_mev_searcher   = 0.0f64;
            let mut total_mev_gross      = 0.0f64;
            let mut total_sw_count       = 0u32;
            let mut total_sw_profit      = 0.0f64;
            let mut total_sw_slippage    = 0.0f64;

            for &seed in seeds {
                let prices = price_series(initial_oracle, rounds, price_vol, dt_secs, seed);
                let r = simulate(
                    spread, k, fee_bps, vol_adj,
                    initial_oracle, initial_reserve_a, initial_reserve_b,
                    swap_size, &prices, seed, n_searchers, oracle_lag,
                );
                total_fees_a       += r.fees_a;
                total_fees_b       += r.fees_b;
                total_arbs         += r.arb_count;
                total_pnl          += r.pnl_bps;
                total_il           += r.il_bps;
                total_mev_arbs     += r.mev_arb_count;
                total_mev_tips     += r.mev_tips_b;
                total_mev_searcher += r.mev_searcher_net_b;
                total_mev_gross    += r.mev_gross_b;
                total_sw_count     += r.sandwich_count;
                total_sw_profit    += r.sandwich_profit_b;
                total_sw_slippage  += r.victim_slippage_b;
            }

            let n = seeds.len() as f64;
            let avg_fees_b = total_fees_b as f64 / n;
            let avg_pnl    = total_pnl as f64 / n;
            let avg_il     = total_il as f64 / n;
            let score      = (avg_fees_b / (avg_il.abs() + 1.0)) * (1.0 + avg_pnl / 1000.0);

            results.push((score, SimResult {
                spread_bps:         spread,
                k_param:            k,
                fees_a:             total_fees_a / seeds.len() as u64,
                fees_b:             total_fees_b / seeds.len() as u64,
                arb_count:          total_arbs   / seeds.len() as u32,
                pnl_bps:            total_pnl    / seeds.len() as i64,
                il_bps:             total_il     / seeds.len() as i64,
                mev_arb_count:      total_mev_arbs     / seeds.len() as u32,
                mev_tips_b:         total_mev_tips     / n,
                mev_searcher_net_b: total_mev_searcher / n,
                mev_gross_b:        total_mev_gross    / n,
                sandwich_count:     total_sw_count   / seeds.len() as u32,
                sandwich_profit_b:  total_sw_profit  / n,
                victim_slippage_b:  total_sw_slippage / n,
            }));
        }
    }

    results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    println!("\n{label}");
    if n_searchers > 0 {
        println!("{:<8} {:<6} {:>9} {:>9} {:>9} {:>9} {:>10} {:>10} {:>10} {:>6} {:>10} {:>12}",
            "spread", "k", "fees_b", "il_bps", "pnl_bps", "arbs",
            "mev_tips", "srch_net", "mev_gross",
            "sw_cnt", "sw_prof_b", "vicslip_b");
        println!("{}", "─".repeat(118));
        for (_, r) in &results {
            println!("{:<8} {:<6} {:>9} {:>9} {:>9} {:>9} {:>10.0} {:>10.0} {:>10.0} {:>6} {:>10.0} {:>12.0}",
                r.spread_bps, r.k_param,
                r.fees_b, r.il_bps, r.pnl_bps, r.arb_count,
                r.mev_tips_b, r.mev_searcher_net_b, r.mev_gross_b,
                r.sandwich_count, r.sandwich_profit_b, r.victim_slippage_b,
            );
        }
    } else {
        println!("{:<8} {:<6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
            "spread", "k", "fees_b", "il_bps", "pnl_bps", "arbs", "fee/IL", "score");
        println!("{}", "─".repeat(75));
        for (score, r) in &results {
            let fee_il = if r.il_bps.abs() > 0 {
                r.fees_b as f64 / r.il_bps.abs() as f64
            } else { f64::INFINITY };
            println!("{:<8} {:<6} {:>9} {:>9} {:>9} {:>9} {:>9.2} {:>9.2}",
                r.spread_bps, r.k_param,
                r.fees_b, r.il_bps, r.pnl_bps, r.arb_count,
                fee_il, score,
            );
        }
    }

    results
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rounds      = parse_flag(&args, "--rounds",    365usize);
    let price_vol   = parse_flag(&args, "--price-vol", 200u32);
    let fee_bps     = parse_flag(&args, "--fee-bps",   5u32);
    let vol_adj     = parse_flag(&args, "--vol-adj",   30u32);
    let swap_size   = parse_flag(&args, "--swap-bps",  50u32);
    let dt_secs: f64 = parse_flag(&args, "--dt-secs", 86400u64) as f64;
    // --searchers N: how many MEV bots compete in the oracle update window
    // 0 = no MEV (baseline), default 5 = realistic Solana MEV competition
    let n_searchers = parse_flag(&args, "--searchers", 5u32);
    // --oracle-lag N: pool oracle updates every N rounds.
    // 1 = every round (ideal), 3 = realistic Solana (oracle_arm sends every ~3s at 1s dt),
    // 10 = slow oracle (e.g. MIN_ORACLE_UPDATE_BPS=5bps in calm market)
    let oracle_lag  = parse_flag(&args, "--oracle-lag", 3u32);

    let initial_oracle:    u64 = 160_000_000_000;
    let initial_reserve_a: u64 = 10_000_000_000;
    let initial_reserve_b: u64 = 1_600_000_000_000;

    let spreads:  &[u32] = &[0, 1, 2, 3, 5, 10, 20, 30, 50];
    let k_params: &[u32] = &[200, 500, 1000];
    let seeds = [42u64, 137, 9999, 12345, 7777];

    println!("PMM MEV sweep: {} rounds × {} seeds | vol={}bps/yr | searchers={} | oracle_lag={}",
        rounds, seeds.len(), price_vol, n_searchers, oracle_lag);

    // ── Baseline: no MEV competition (oracle_lag=1 = ideal) ──────────────────
    let baseline = run_sweep(
        "── BASELINE (no MEV, single arber, oracle_lag=1) ──",
        rounds, price_vol, fee_bps, vol_adj, swap_size, dt_secs, 0, 1,
        spreads, k_params, &seeds,
        initial_oracle, initial_reserve_a, initial_reserve_b,
    );

    // ── With MEV auction + realistic oracle lag ───────────────────────────────
    let mev = run_sweep(
        &format!("── MEV AUCTION ({n_searchers} searchers, oracle_lag={oracle_lag}) ──"),
        rounds, price_vol, fee_bps, vol_adj, swap_size, dt_secs, n_searchers, oracle_lag,
        spreads, k_params, &seeds,
        initial_oracle, initial_reserve_a, initial_reserve_b,
    );

    // ── Delta table: oracle MEV + sandwich impact vs baseline ────────────────
    println!("\n── MEV IMPACT: extra IL eaten by tip auction + sandwich vs baseline ──");
    println!("{:<8} {:<6} {:>10} {:>10} {:>14} {:>12} {:>10} {:>12}",
        "spread", "k", "il_base", "il_mev", "extra_il_bps", "tips_b", "sw_cnt", "sw_profit_b");
    println!("{}", "─".repeat(90));

    for (base_row, mev_row) in baseline.iter().zip(mev.iter()) {
        let b = &base_row.1;
        let m = &mev_row.1;
        let extra_il = m.il_bps - b.il_bps;
        println!("{:<8} {:<6} {:>10} {:>10} {:>14} {:>12.0} {:>10} {:>12.0}",
            b.spread_bps, b.k_param,
            b.il_bps, m.il_bps,
            extra_il,
            m.mev_tips_b,
            m.sandwich_count,
            m.sandwich_profit_b,
        );
    }

    println!("\nKey insights:");
    println!("  extra_il_bps  = additional LP loss from oracle-lag MEV");
    println!("  tips_b        = value captured by validators (not LPs)");
    println!("  sw_cnt        = retail orders sandwiched (40% targeted)");
    println!("  sw_profit_b   = sandwicher's net after paying 2× pool fee");
    println!("  vicslip_b     = total extra slippage paid by sandwich victims");
    println!("\nRun with different params:");
    println!("  cargo run --bin sim -- --searchers 1  --oracle-lag 3   # monopoly arber");
    println!("  cargo run --bin sim -- --searchers 5  --oracle-lag 3   # realistic (default)");
    println!("  cargo run --bin sim -- --searchers 20 --oracle-lag 10  # high MEV + slow oracle");
    println!("  cargo run --bin sim -- --vol-adj 0    --oracle-lag 5   # pure MEV, no vol-adj");
}

fn parse_flag<T: std::str::FromStr + Copy>(args: &[String], flag: &str, default: T) -> T {
    args.windows(2)
        .find(|w| w[0] == flag)
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(default)
}
