//! Pyth Hermes price fetch — shared by oracle arm and trading arm.

use anyhow::{Context, Result};
use std::collections::VecDeque;

// ── HTTP response structs ─────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct HermesResponse {
    pub parsed: Vec<HermesParsed>,
}

#[derive(serde::Deserialize)]
pub struct HermesParsed {
    pub price: HermesPrice,
}

#[derive(serde::Deserialize)]
pub struct HermesPrice {
    pub price: String,
    pub conf:  String,
    pub expo:  i32,
}

// ── Price history (short-term volatility estimate) ───────────────────────────

pub struct PriceHistory {
    prices: VecDeque<u64>,
    capacity: usize,
}

impl PriceHistory {
    pub fn new(capacity: usize) -> Self {
        PriceHistory { prices: VecDeque::with_capacity(capacity), capacity }
    }

    pub fn push(&mut self, price: u64) {
        if self.prices.len() == self.capacity { self.prices.pop_front(); }
        self.prices.push_back(price);
    }

    /// Historical volatility in bps (0..10000). Returns 4500 if < 3 samples.
    pub fn vol_bps(&self) -> u32 {
        let n = self.prices.len();
        if n < 3 { return 4500; }
        let mean = self.prices.iter().copied().sum::<u64>() as f64 / n as f64;
        let variance = self.prices.iter()
            .map(|&p| (p as f64 - mean).powi(2))
            .sum::<f64>() / (n as f64 - 1.0);
        let rel_vol = variance.sqrt() / mean;
        (rel_vol * 10_000.0).min(10_000.0) as u32
    }
}

// ── Fetch ─────────────────────────────────────────────────────────────────────

/// Returns `(oracle_price_1e9, confidence_bps)`.
///
/// `oracle_price_1e9` is the Pyth price normalized to 1e9 precision.
/// `confidence_bps` is the confidence interval as a fraction of price in bps.
pub async fn fetch_pyth_price(http: &reqwest::Client, feed_id: &str) -> Result<(u64, u32)> {
    let url = format!(
        "https://hermes.pyth.network/v2/updates/price/latest?ids[]={}&parsed=true",
        feed_id
    );

    let resp: HermesResponse = http
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send().await.context("Hermes request failed")?
        .json().await.context("Hermes JSON parse failed")?;

    let parsed = resp.parsed.first().context("no price in Hermes response")?;
    let price_int: i64 = parsed.price.price.parse().context("invalid price")?;
    let conf_int:  u64 = parsed.price.conf.parse().context("invalid conf")?;
    let expo = parsed.price.expo;

    if price_int <= 0 { anyhow::bail!("non-positive Pyth price: {}", price_int); }

    // Normalize to 1e9 scale
    let oracle_price: u64 = if expo >= 0 {
        (price_int as u64)
            .saturating_mul(10u64.pow((expo as u32).saturating_add(9)))
    } else {
        let div = 10u64.pow(expo.unsigned_abs());
        (price_int as u64)
            .saturating_mul(1_000_000_000)
            .checked_div(div)
            .unwrap_or(0)
    };

    // Confidence as fraction of price in bps
    let conf_bps: u32 = if price_int > 0 && conf_int > 0 {
        let rel = conf_int as f64 / price_int.unsigned_abs() as f64;
        (rel * 10_000.0).min(10_000.0) as u32
    } else {
        0
    };

    Ok((oracle_price, conf_bps))
}
