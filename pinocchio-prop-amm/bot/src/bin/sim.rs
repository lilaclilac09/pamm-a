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
}

fn simulate(
    base_spread_bps: u32,
    k_param: u32,
    fee_bps: u32,
    vol_adj: u32,
    initial_oracle: u64,
    initial_reserve_a: u64,
    initial_reserve_b: u64,
    swap_size_bps: u32,  // base swap size (bps of reserve_a)
    price_series: &[u64],
    seed: u64,
) -> SimResult {
    let mut rng = Lcg(seed.wrapping_add(0xabc123));
    let mut reserve_a = initial_reserve_a;
    let mut reserve_b = initial_reserve_b;
    let target_a = initial_reserve_a;
    let target_b = initial_reserve_b;
    let mut fees_a = 0u64;
    let mut fees_b = 0u64;
    let mut arb_count = 0u32;

    // Start value in token_b units
    let start_val_b = reserve_b as f64
        + reserve_a as f64 * initial_oracle as f64 / 1_000_000_000.0;

    // Hodl basket
    let hodl_a = initial_reserve_a as f64;
    let hodl_b = initial_reserve_b as f64;

    let mut prev_oracle = initial_oracle;

    for &oracle in price_series.iter() {
        if oracle == 0 { continue; }

        // ── Uninformed flow: random swap every round (70% probability) ────────
        // Represents real trading activity (not arb). Direction is random.
        // Size is random 0.1x to 2x of swap_size_bps.
        let flow_roll = rng.next() % 100;
        if flow_roll < 70 {
            let size_mult = 1 + (rng.next() % 10) as u32; // 1..10 × 0.1 = 0.1..1.0x
            let flow_size_bps = swap_size_bps * size_mult / 10;
            let flow_a = (reserve_a as u128 * flow_size_bps as u128 / 10_000).max(1) as u64;
            let flow_b = (reserve_b as u128 * flow_size_bps as u128 / 10_000).max(1) as u64;

            if rng.next() % 2 == 0 {
                // Buy B with A (A→B)
                if let Some((out_b, fee)) = pmm_quote_a_to_b(
                    reserve_a, reserve_b, target_a, target_b,
                    oracle, base_spread_bps, vol_adj, k_param, fee_bps, flow_a,
                ) {
                    if out_b > 0 && out_b < reserve_b {
                        reserve_a += flow_a;
                        reserve_b = reserve_b.saturating_sub(out_b);
                        fees_b += fee;
                    }
                }
            } else {
                // Buy A with B (B→A)
                if let Some((out_a, fee)) = pmm_quote_b_to_a(
                    reserve_a, reserve_b, target_a, target_b,
                    oracle, base_spread_bps, vol_adj, k_param, fee_bps, flow_b,
                ) {
                    if out_a > 0 && out_a < reserve_a {
                        reserve_b += flow_b;
                        reserve_a = reserve_a.saturating_sub(out_a);
                        fees_a += fee;
                    }
                }
            }
        }

        // ── Inventory rebalance arb: fires when pool drifts from target ───────
        // When price moves, pool's A value changes vs B value.
        // An arb bot restores balance by trading the overpriced side.
        // This is the main source of IL in a PMM.
        let price_delta_bps = if prev_oracle > 0 {
            oracle.abs_diff(prev_oracle).saturating_mul(10_000) / prev_oracle
        } else { 0 };

        // Only arb when price moved > spread (profitable after spread cost)
        if price_delta_bps > base_spread_bps as u64 {
            let swap_a = (reserve_a as u128 * swap_size_bps as u128 / 10_000).max(1) as u64;
            let swap_b = (reserve_b as u128 * swap_size_bps as u128 / 10_000).max(1) as u64;

            if oracle > prev_oracle {
                // Price went up: A is more valuable. Arb buys A (B→A) from pool.
                if let Some((out_a, fee)) = pmm_quote_b_to_a(
                    reserve_a, reserve_b, target_a, target_b,
                    oracle, base_spread_bps, vol_adj, k_param, fee_bps, swap_b,
                ) {
                    if out_a > 0 && out_a < reserve_a {
                        reserve_b += swap_b;
                        reserve_a = reserve_a.saturating_sub(out_a);
                        fees_a += fee;
                        arb_count += 1;
                    }
                }
            } else {
                // Price went down: B is more valuable. Arb buys B (A→B) from pool.
                if let Some((out_b, fee)) = pmm_quote_a_to_b(
                    reserve_a, reserve_b, target_a, target_b,
                    oracle, base_spread_bps, vol_adj, k_param, fee_bps, swap_a,
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

        prev_oracle = oracle;
    }

    let final_price = *price_series.last().unwrap_or(&initial_oracle);

    // Pool end value in token_b
    let pool_val = reserve_b as f64
        + reserve_a as f64 * final_price as f64 / 1_000_000_000.0;
    let pnl_bps = ((pool_val - start_val_b) / start_val_b * 10_000.0) as i64;

    // Hodl end value
    let hodl_val = hodl_b + hodl_a * final_price as f64 / 1_000_000_000.0;
    // IL = how much worse off the pool is vs. just holding
    let il_bps = if hodl_val > 0.0 {
        ((hodl_val - pool_val) / hodl_val * 10_000.0) as i64
    } else { 0 };

    SimResult { spread_bps: base_spread_bps, k_param, fees_a, fees_b, arb_count, pnl_bps, il_bps }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rounds    = parse_flag(&args, "--rounds",    365usize);
    // annualised vol in bps; ~80bps ≈ SOL in a quiet year, 300+ for high-vol regimes
    let price_vol = parse_flag(&args, "--price-vol", 200u32);
    let fee_bps   = parse_flag(&args, "--fee-bps",   5u32);
    let vol_adj   = parse_flag(&args, "--vol-adj",   30u32);
    let swap_size = parse_flag(&args, "--swap-bps",  50u32); // uninformed/arb size bps
    // dt_secs: simulation time step. Default 86400 = 1 day.
    // At 200bps/yr vol, daily sigma ≈ 10.5bps → arbs fire with spreads ≤ 10bps.
    let dt_secs: f64 = parse_flag(&args, "--dt-secs", 86400u64) as f64;

    // SOL/USD ~ $160, stored as lamports per 1 token_b.
    let initial_oracle:    u64 = 160_000_000_000;
    let initial_reserve_a: u64 = 10_000_000_000;  // 10 SOL
    let initial_reserve_b: u64 = 1_600_000_000_000; // 1600 USDC (scaled ×1e6)

    // Parameter grid — includes 0 and 1bp to model BASE_SPREAD_BPS=0 regime
    // (effective spread = vol_adj ≈ Pyth conf ~1-3bps, so 0/1bp base makes sense)
    let spreads: &[u32] = &[0, 1, 2, 3, 5, 10, 20, 30, 50];
    let k_params: &[u32] = &[200, 500, 1000];

    // Run with multiple seeds, average results
    let seeds = [42u64, 137, 9999, 12345, 7777];

    println!("\nPMM parameter sweep: {} rounds × {} seeds | vol={}bps/yr | arb_size={}bps",
        rounds, seeds.len(), price_vol, swap_size);
    println!("──────────────────────────────────────────────────────────────────────────");
    println!("{:<8} {:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "spread", "k", "fees_b", "arb_cnt", "pnl_bps", "il_bps", "fee/IL", "score");
    println!("──────────────────────────────────────────────────────────────────────────");

    let mut results: Vec<(f64, SimResult)> = Vec::new();

    for &spread in spreads {
        for &k in k_params {
            // Accumulate across seeds
            let mut total_fees_a = 0u64;
            let mut total_fees_b = 0u64;
            let mut total_arbs   = 0u32;
            let mut total_pnl    = 0i64;
            let mut total_il     = 0i64;

            for &seed in &seeds {
                let prices = price_series(
                    initial_oracle, rounds, price_vol, dt_secs, seed
                );
                let r = simulate(
                    spread, k, fee_bps, vol_adj,
                    initial_oracle,
                    initial_reserve_a, initial_reserve_b,
                    swap_size, &prices, seed,
                );
                total_fees_a += r.fees_a;
                total_fees_b += r.fees_b;
                total_arbs   += r.arb_count;
                total_pnl    += r.pnl_bps;
                total_il     += r.il_bps;
            }

            let n = seeds.len() as f64;
            let avg_fees_b = total_fees_b as f64 / n;
            let _avg_arbs  = total_arbs as f64 / n;
            let avg_pnl    = total_pnl as f64 / n;
            let avg_il     = total_il as f64 / n;

            // Score: higher fees is good, lower IL is good, fewer arbs = tighter spread (efficiency)
            // Simple ratio: avg_fees_b / (|avg_il| + 1) * (1 + avg_pnl/1000)
            let score = (avg_fees_b / (avg_il.abs() + 1.0)) * (1.0 + avg_pnl / 1000.0);

            results.push((score, SimResult {
                spread_bps: spread,
                k_param: k,
                fees_a: total_fees_a / seeds.len() as u64,
                fees_b: total_fees_b / seeds.len() as u64,
                arb_count: total_arbs / seeds.len() as u32,
                pnl_bps: (total_pnl / seeds.len() as i64),
                il_bps: (total_il / seeds.len() as i64),
            }));
        }
    }

    // Sort by score descending
    results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    for (score, r) in &results {
        let fee_il = if r.il_bps.abs() > 0 {
            r.fees_b as f64 / r.il_bps.abs() as f64
        } else { f64::INFINITY };
        println!("{:<8} {:<8} {:>10} {:>10} {:>10} {:>10} {:>10.2} {:>10.2}",
            r.spread_bps, r.k_param,
            r.fees_b, r.arb_count, r.pnl_bps, r.il_bps,
            fee_il, score,
        );
    }

    println!("──────────────────────────────────────────────────────────────────────────");

    if let Some((_, best)) = results.first() {
        println!("\nBest combo: spread={}bps  k={}",
            best.spread_bps, best.k_param);
        println!("  avg fees_b/round = {:.1}", best.fees_b as f64 / rounds as f64);
        println!("  avg arbs/round   = {:.2}", best.arb_count as f64 / rounds as f64);
        println!("  avg pnl          = {}bps", best.pnl_bps);
        println!("  avg IL           = {}bps", best.il_bps);
    }

    // Extra: show how oracle skip filter affects CU cost
    println!("\nOracle skip savings estimate:");
    println!("  UPDATE_ORACLE CU ≈ 5_000");
    println!("  Without skip (1s interval, 300 rounds): {}k CU",
        300 * 5_000 / 1000);
    let skipped = (rounds as f64 * 0.9) as u32;  // ~90% skip in calm market
    println!("  With skip (5bps threshold): ~{}k CU saved ({} skipped rounds)",
        skipped * 5_000 / 1000, skipped);
}

fn parse_flag<T: std::str::FromStr + Copy>(args: &[String], flag: &str, default: T) -> T {
    args.windows(2)
        .find(|w| w[0] == flag)
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(default)
}
