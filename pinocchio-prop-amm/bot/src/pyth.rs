//! Pyth price feed — streaming websocket + HTTP fallback.
//!
//! Architecture:
//!   `spawn_price_stream` runs a background task that connects to the Pyth
//!   Hermes websocket and writes every price update into a `watch::Sender`.
//!   The oracle arm reads from the `watch::Receiver` — always sees the freshest
//!   price without any polling delay.
//!
//!   `fetch_pyth_price` is kept as a one-shot HTTP fallback used on first boot
//!   before the websocket delivers its first message.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::watch;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

// ── Wire types ────────────────────────────────────────────────────────────────

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

#[derive(serde::Deserialize)]
struct WsEnvelope {
    #[serde(rename = "type")]
    kind: String,
    price_feed: Option<WsFeed>,
}

#[derive(serde::Deserialize)]
struct WsFeed {
    price: HermesPrice,
}

// ── Normalise Hermes price → (oracle_price_1e9, conf_bps) ────────────────────

pub fn parse_price(p: &HermesPrice) -> Result<(u64, u32)> {
    let price_int: i64 = p.price.parse().context("invalid price")?;
    let conf_int:  u64 = p.conf.parse().context("invalid conf")?;

    if price_int <= 0 { anyhow::bail!("non-positive Pyth price: {}", price_int); }

    let oracle_price: u64 = if p.expo >= 0 {
        (price_int as u64)
            .saturating_mul(10u64.pow((p.expo as u32).saturating_add(9)))
    } else {
        let div = 10u64.pow(p.expo.unsigned_abs());
        (price_int as u64)
            .saturating_mul(1_000_000_000)
            .checked_div(div)
            .unwrap_or(0)
    };

    let conf_bps: u32 = if price_int > 0 && conf_int > 0 {
        let rel = conf_int as f64 / price_int.unsigned_abs() as f64;
        (rel * 10_000.0).min(10_000.0) as u32
    } else {
        0
    };

    Ok((oracle_price, conf_bps))
}

// ── EWMA volatility estimator ─────────────────────────────────────────────────
//
// Uses exponentially-weighted variance of log returns.
// α = 0.06 ≈ 32-sample half-life — fast enough to catch vol spikes,
// slow enough to avoid noise-induced over-widening.

pub struct EwmaVol {
    ewma_var:   f64,  // current EWMA variance of log-returns
    last_price: f64,
    alpha:      f64,  // decay factor
    n:          u32,  // samples seen (warmup guard)
}

impl EwmaVol {
    pub fn new(alpha: f64) -> Self {
        EwmaVol { ewma_var: 0.0, last_price: 0.0, alpha, n: 0 }
    }

    pub fn update(&mut self, price: u64) {
        let p = price as f64;
        if self.last_price > 0.0 {
            let ret = (p / self.last_price).ln();
            self.ewma_var = self.alpha * ret * ret
                + (1.0 - self.alpha) * self.ewma_var;
            self.n += 1;
        }
        self.last_price = p;
    }

    /// Returns vol in bps. Returns 0 during warmup — conf_bps drives spread until EWMA is ready.
    pub fn vol_bps(&self) -> u32 {
        if self.n < 5 { return 0; }
        (self.ewma_var.sqrt() * 10_000.0).min(10_000.0) as u32
    }
}

// ── Websocket price stream ────────────────────────────────────────────────────

const HERMES_WS: &str = "wss://hermes.pyth.network/ws";

/// Spawns a background task that streams Pyth prices into `tx`.
/// Reconnects automatically on any error with exponential back-off.
pub fn spawn_price_stream(
    feed_id:    String,
    tx:         watch::Sender<Option<(u64, u32)>>,
) {
    tokio::spawn(async move {
        let mut delay_secs = 1u64;
        loop {
            info!("pyth_ws: connecting to {}", HERMES_WS);
            match stream_once(&feed_id, &tx).await {
                Ok(()) => {
                    info!("pyth_ws: stream ended cleanly — reconnecting in {}s", delay_secs);
                }
                Err(e) => {
                    warn!("pyth_ws: error ({}s backoff): {:#}", delay_secs, e);
                }
            }
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
            delay_secs = (delay_secs * 2).min(30); // cap at 30s
        }
    });
}

async fn stream_once(
    feed_id: &str,
    tx:      &watch::Sender<Option<(u64, u32)>>,
) -> Result<()> {
    let (ws_stream, _) = connect_async(HERMES_WS)
        .await
        .context("websocket connect failed")?;

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to the feed
    let sub_msg = serde_json::json!({
        "type": "subscribe",
        "ids":  [feed_id],
    });
    write.send(Message::Text(sub_msg.to_string())).await
        .context("subscribe send failed")?;

    info!("pyth_ws: subscribed to feed {}", &feed_id[..8]);

    // Heartbeat: send a ping every 20s so the connection stays alive
    let mut ping_interval = tokio::time::interval(Duration::from_secs(20));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                write.send(Message::Ping(vec![])).await
                    .context("ping failed")?;
            }
            msg = read.next() => {
                match msg {
                    None => return Ok(()), // stream ended
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(env) = serde_json::from_str::<WsEnvelope>(&text) {
                            if env.kind == "price_update" {
                                if let Some(feed) = env.price_feed {
                                    match parse_price(&feed.price) {
                                        Ok((p, c)) => {
                                            debug!("pyth_ws: price={} conf={}bps", p, c);
                                            let _ = tx.send(Some((p, c)));
                                        }
                                        Err(e) => warn!("pyth_ws: parse error: {:#}", e),
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Ping(d))) => {
                        write.send(Message::Pong(d)).await
                            .context("pong failed")?;
                    }
                    Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

// ── HTTP one-shot fallback ────────────────────────────────────────────────────

/// One-shot HTTP fetch. Used on startup before websocket delivers first price.
pub async fn fetch_pyth_price(http: &reqwest::Client, feed_id: &str) -> Result<(u64, u32)> {
    let url = format!(
        "https://hermes.pyth.network/v2/updates/price/latest?ids[]={}&parsed=true",
        feed_id
    );
    let resp: HermesResponse = http
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send().await.context("Hermes HTTP request failed")?
        .json().await.context("Hermes HTTP JSON parse failed")?;

    let parsed = resp.parsed.first().context("no price in Hermes response")?;
    parse_price(&parsed.price)
}
