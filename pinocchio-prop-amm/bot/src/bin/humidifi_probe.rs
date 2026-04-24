//! HumidiFi pool-layout probe.
//!
//! HumidiFi's pool layout is closed-source (no public IDL). We can still
//! infer field offsets heuristically by taking two snapshots of the pool
//! account spaced a few hundred milliseconds apart and looking at which
//! u64/i64 windows changed, which look like unix timestamps, and which
//! look like SOL/USD prices at common scales.
//!
//! The output ranks candidate offsets for:
//!   - last_oracle_update (unix-ts i64, changes every ~80-200ms)
//!   - oracle_price       (u64 in the $100-$200 band at 1e8/1e9/1e10 scales)
//!   - reserve_a/b        (u64 that changed between snapshots)
//!
//! Run (from this package root):
//!   cargo run --bin humidifi-probe --release
//!
//! The default pool is WSOL-USDC (FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH).
//! Override with `HUMIDIFI_POOL_PUBKEY=<pubkey>`.
//!
//! Requires mainnet RPC access (RPC_URL env var, or defaults to a public node).
//!
//! Once you identify the last_oracle_update offset, set it as the
//! `HUMIDIFI_POOL_STALENESS_OFFSET` in humidifi_arm's config and wire
//! up an on-chain staleness filter there.

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::{str::FromStr, thread::sleep, time::Duration};

const DEFAULT_POOL: &str = "FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH";

// Unix-ts range for sanity: 2023-01-01 .. 2030-01-01
const TS_MIN: i64 = 1_672_531_200;
const TS_MAX: i64 = 1_893_456_000;

// SOL/USD price range across common scales (we'll try each scale).
const PRICE_MIN_USD: f64 =  50.0;
const PRICE_MAX_USD: f64 = 500.0;
const PRICE_SCALES: &[(u64, &str)] = &[
    (100_000_000,     "1e8 (Pyth legacy)"),
    (1_000_000_000,   "1e9"),
    (1_000_000_000_0, "1e10"),
    (1_000_000,       "1e6 (USDC units)"),
];

fn main() -> Result<()> {
    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".into());
    let pool_str = std::env::var("HUMIDIFI_POOL_PUBKEY")
        .unwrap_or_else(|_| DEFAULT_POOL.into());
    let pool = Pubkey::from_str(&pool_str).context("invalid pool pubkey")?;

    println!("probe target : {}", pool);
    println!("rpc          : {}\n", rpc_url);

    let client = RpcClient::new(rpc_url);

    let snap_a = client.get_account_data(&pool).context("snap A")?;
    sleep(Duration::from_millis(500));
    let snap_b = client.get_account_data(&pool).context("snap B")?;

    if snap_a.len() != snap_b.len() {
        anyhow::bail!("snapshot size mismatch: {} vs {}", snap_a.len(), snap_b.len());
    }
    let len = snap_a.len();
    println!("account size : {} bytes", len);
    println!("discriminator: {}\n", hex(&snap_a[..8.min(len)]));

    report_changed_offsets(&snap_a, &snap_b);
    report_timestamp_candidates(&snap_a);
    report_price_candidates(&snap_a);
    report_reserve_candidates(&snap_a, &snap_b);

    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
}

fn report_changed_offsets(a: &[u8], b: &[u8]) {
    println!("── changed bytes (A vs B) ─────────────────────────────────────────");
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in 0..a.len() {
        if a[i] != b[i] {
            run_start.get_or_insert(i);
        } else if let Some(s) = run_start.take() {
            runs.push((s, i - 1));
        }
    }
    if let Some(s) = run_start { runs.push((s, a.len() - 1)); }

    if runs.is_empty() {
        println!("  (no changes — pool was idle during probe window)\n");
    } else {
        for (s, e) in runs {
            println!("  offset {:>4}..{:<4}  A={} → B={}",
                s, e, hex(&a[s..=e]), hex(&b[s..=e]));
        }
        println!();
    }
}

fn report_timestamp_candidates(data: &[u8]) {
    println!("── i64 unix-timestamp candidates (in {}..{}) ─────────────", TS_MIN, TS_MAX);
    let mut any = false;
    for off in 0..=data.len().saturating_sub(8) {
        let v = i64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        if v >= TS_MIN && v <= TS_MAX {
            println!("  offset {:>4}  value={} ({} UTC)", off, v, ts_to_str(v));
            any = true;
        }
    }
    if !any { println!("  (no timestamp-shaped fields found)"); }
    println!();
}

fn report_price_candidates(data: &[u8]) {
    println!("── u64 SOL/USD price candidates (${} .. ${}) ────────────",
        PRICE_MIN_USD, PRICE_MAX_USD);
    let mut any = false;
    for off in 0..=data.len().saturating_sub(8) {
        let v = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        for (scale, label) in PRICE_SCALES {
            let usd = v as f64 / *scale as f64;
            if usd >= PRICE_MIN_USD && usd <= PRICE_MAX_USD {
                println!("  offset {:>4}  raw={} ≈ ${:.2} @ {}", off, v, usd, label);
                any = true;
            }
        }
    }
    if !any { println!("  (no price-shaped fields found at tested scales)"); }
    println!();
}

fn report_reserve_candidates(a: &[u8], b: &[u8]) {
    println!("── u64 reserve candidates (values that changed & are >1e6) ─────────");
    let mut any = false;
    for off in 0..=a.len().saturating_sub(8) {
        let va = u64::from_le_bytes(a[off..off + 8].try_into().unwrap());
        let vb = u64::from_le_bytes(b[off..off + 8].try_into().unwrap());
        if va == vb || va < 1_000_000 { continue; }
        let delta = if vb > va { vb - va } else { va - vb };
        println!("  offset {:>4}  A={} B={} Δ={}", off, va, vb, delta);
        any = true;
    }
    if !any { println!("  (no reserve-shaped deltas)"); }
    println!();
}

fn ts_to_str(ts: i64) -> String {
    // Manual YYYY-MM-DD from unix seconds so we don't pull in chrono.
    let days = ts / 86_400;
    let secs_of_day = ts % 86_400;
    let h = secs_of_day / 3_600;
    let m = (secs_of_day % 3_600) / 60;
    let s = secs_of_day % 60;
    let (y, mo, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, m, s)
}

fn days_to_ymd(mut days: i64) -> (i32, u32, u32) {
    // Days since 1970-01-01 → Y/M/D using Howard Hinnant's algorithm.
    days += 719_468;
    let era = if days >= 0 { days / 146_097 } else { (days - 146_096) / 146_097 };
    let doe = (days - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
