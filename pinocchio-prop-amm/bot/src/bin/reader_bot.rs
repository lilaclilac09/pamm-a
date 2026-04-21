//! Reader Bot — subscribe to PMM pool oracle updates via LaserStream and trade momentum.
//!
//! HOW THE MM MAKES MONEY:
//!   1. Fee income (fee_bps per swap, collected into fee_a / fee_b, swept by admin)
//!   2. Spread capture: every swap pays oracle_mid ± (spread_bps + vol_adj)
//!   3. Volatility premium: EWMA vol widens spread in choppy markets automatically
//!   4. HumidiFi arb: humidifi_arm exploits HumidiFi's slower oracle via Jupiter
//!
//! HOW THIS READER BOT MAKES MONEY:
//!   Oracle price momentum strategy.
//!   The MM's oracle_arm updates pool price every ~500ms (or any 1bp Pyth move).
//!   We subscribe to the pool account via accountSubscribe WebSocket (LaserStream).
//!
//!   Oracle tick UP   ≥ MOMENTUM_BPS → buy A (B→A): price will keep rising
//!   Oracle tick DOWN ≥ MOMENTUM_BPS → sell A (A→B): price will keep falling
//!
//!   Edge = momentum_continuation - spread_cost_paid
//!   If two consecutive ticks go the same direction the trade is net positive.
//!
//!   On devnet: no Jito → oracle updates sit in mempool briefly.
//!   LaserStream fires within ~1 slot (400ms) of the oracle tx landing.
//!   Reader bot is the first to trade against the updated price.
//!
//! DASHBOARD:
//!   Exposes GET /feed as SSE on READER_PORT (default 3001).
//!   dashboard.html connects to this feed alongside the MM bot's feed on 3000.
//!
//! Run from pinocchio-prop-amm/bot/ with:
//!   cargo run --bin reader_bot

use anyhow::{Context, Result};
use async_stream::stream;
use axum::{
    http::Method,
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
    routing::get,
    Router,
};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use std::{
    convert::Infallible,
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{watch, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, warn};

// ── Pool layout constants ──────────────────────────────────────────────────────

const OFF_RESERVE_A:    usize = 48;
const OFF_RESERVE_B:    usize = 56;
const OFF_ORACLE_PRICE: usize = 104;
const OFF_SPREAD_BPS:   usize = 112;
const OFF_LAST_UPDATE:  usize = 128;
const POOL_SIZE:        usize = 304;
const TOKEN_PROGRAM_ID: &str  = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

// ── Strategy parameters ────────────────────────────────────────────────────────

const MOMENTUM_BPS: u64 = 5;
const TRADE_AMOUNT: u64 = 10_000_000; // 0.01 token-equivalent on devnet
const SUMMARY_INTERVAL: usize = 5;

// ── Pool snapshot ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct PoolState {
    reserve_a:    u64,
    reserve_b:    u64,
    oracle_price: u64,
    spread_bps:   u32,
    last_update:  i64,
}

impl PoolState {
    fn from_bytes(d: &[u8]) -> Option<Self> {
        if d.len() < POOL_SIZE { return None; }
        let r64  = |o: usize| u64::from_le_bytes(d[o..o+8].try_into().unwrap());
        let ri64 = |o: usize| i64::from_le_bytes(d[o..o+8].try_into().unwrap());
        let r32  = |o: usize| u32::from_le_bytes(d[o..o+4].try_into().unwrap());
        Some(PoolState {
            reserve_a:    r64(OFF_RESERVE_A),
            reserve_b:    r64(OFF_RESERVE_B),
            oracle_price: r64(OFF_ORACLE_PRICE),
            spread_bps:   r32(OFF_SPREAD_BPS),
            last_update:  ri64(OFF_LAST_UPDATE),
        })
    }
}

// ── Trade record ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Trade {
    cycle:        u64,
    direction:    &'static str, // "B→A" or "A→B"
    amount_in:    u64,
    oracle_price: u64,
    price_move:   u64, // bps that triggered trade
    sig:          String,
}

// ── Reader PnL book ────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
pub struct ReaderBook {
    pub trades:         Vec<Trade>,
    pub net_a:          f64,
    pub net_b:          f64,
    pub total_volume_b: f64,
    pub last_oracle:    u64,
    pub spread_paid_b:  f64,
    pub last_direction: String,
    pub last_sig:       String,
    pub last_move_bps:  u64,
}

impl ReaderBook {
    fn record(&mut self, trade: Trade) {
        let oracle = trade.oracle_price as f64 / 1e9;

        match trade.direction {
            "B→A" => {
                let expected_a = trade.amount_in as f64 / oracle;
                self.net_b -= trade.amount_in as f64;
                self.net_a += expected_a;
                self.total_volume_b += trade.amount_in as f64;
                self.spread_paid_b  += trade.amount_in as f64 * 0.0005;
            }
            "A→B" => {
                let expected_b = trade.amount_in as f64 * oracle;
                self.net_a -= trade.amount_in as f64;
                self.net_b += expected_b;
                self.total_volume_b += expected_b;
                self.spread_paid_b  += expected_b * 0.0005;
            }
            _ => {}
        }

        self.last_oracle    = trade.oracle_price;
        self.last_direction = trade.direction.to_string();
        self.last_sig       = trade.sig[..trade.sig.len().min(16)].to_string();
        self.last_move_bps  = trade.price_move;
        self.trades.push(trade);
    }

    pub fn mtm_pnl_b(&self) -> f64 {
        if self.last_oracle == 0 { return 0.0; }
        let oracle = self.last_oracle as f64 / 1e9;
        self.net_b + self.net_a * oracle
    }

    pub fn pnl_bps(&self) -> f64 {
        if self.total_volume_b == 0.0 { return 0.0; }
        self.mtm_pnl_b() / self.total_volume_b * 10_000.0
    }

    fn print_summary(&self) {
        println!();
        println!("╔══════════════════════════════════════════════╗");
        println!("║            READER BOT — BOOK SUMMARY         ║");
        println!("╠══════════════════════════════════════════════╣");
        println!("║  Trades:         {:>8}                    ║", self.trades.len());
        println!("║  Volume (B):     {:>12.2}                ║", self.total_volume_b);
        println!("║  Net A pos:      {:>12.2}                ║", self.net_a);
        println!("║  Net B pos:      {:>12.2}                ║", self.net_b);
        println!("║  Spread paid (B):{:>12.2}                ║", self.spread_paid_b);
        println!("║  MtM PnL (B):    {:>12.4}                ║", self.mtm_pnl_b());
        println!("║  PnL (bps):      {:>12.2}                ║", self.pnl_bps());
        println!("║  Oracle:         {:>12.6}                ║",
            if self.last_oracle > 0 { format!("{:.6}", self.last_oracle as f64 / 1e9) }
            else { "n/a".into() });
        println!("╚══════════════════════════════════════════════╝");
        println!();
        if !self.trades.is_empty() {
            println!("Last {} trades:", self.trades.len().min(8));
            for t in self.trades.iter().rev().take(8) {
                println!(
                    "  #{:>4}  {}  in={:>10}  oracle={:.6}  move={:>4}bps  sig={}…",
                    t.cycle, t.direction, t.amount_in,
                    t.oracle_price as f64 / 1e9,
                    t.price_move,
                    &t.sig[..t.sig.len().min(16)],
                );
            }
            println!();
        }
    }
}

// ── SSE server state ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct ServerState {
    book: Arc<RwLock<ReaderBook>>,
}

// ── Entry point ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    dotenv::dotenv().ok();

    let key_str   = std::env::var("PRIVATE_KEY").context("PRIVATE_KEY not set")?;
    let key_bytes = bs58::decode(&key_str).into_vec().context("base58 decode")?;
    let wallet    = Keypair::from_bytes(&key_bytes).context("keypair from bytes")?;

    let p = |name: &str| -> Result<Pubkey> {
        Pubkey::from_str(
            &std::env::var(name).context(format!("{name} not set"))?
        ).context(format!("invalid pubkey {name}"))
    };

    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.devnet.solana.com".into());
    let ws_url = std::env::var("WS_URL").unwrap_or_else(|_| {
        let s = rpc_url.trim_start_matches("https://").trim_start_matches("http://");
        let host = s.split('?').next().unwrap_or(s);
        format!("wss://{}", host)
    });
    let reader_port: u16 = std::env::var("READER_PORT")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(3001);

    let program_id  = p("PROGRAM_ID")?;
    let pool_pubkey = p("POOL_PUBKEY")?;
    let pool_auth   = p("POOL_AUTH")?;
    let vault_a     = p("VAULT_A")?;
    let vault_b     = p("VAULT_B")?;
    let user_a      = p("USER_A")?;
    let user_b      = p("USER_B")?;

    info!("reader_bot starting");
    info!("  wallet:     {}", wallet.pubkey());
    info!("  pool:       {}", pool_pubkey);
    info!("  ws_url:     {}", ws_url);
    info!("  momentum:   ≥{}bps", MOMENTUM_BPS);
    info!("  trade_size: {}", TRADE_AMOUNT);
    info!("  dashboard:  http://localhost:{}/feed", reader_port);

    let book = Arc::new(RwLock::new(ReaderBook::default()));

    // ── Spawn SSE server ──────────────────────────────────────────────────────
    let srv_state = ServerState { book: Arc::clone(&book) };
    tokio::spawn(async move {
        let cors = CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]);
        let app = Router::new()
            .route("/feed",   get(feed_handler))
            .route("/health", get(health_handler))
            .layer(cors)
            .with_state(srv_state);

        let addr = SocketAddr::from(([0, 0, 0, 0], reader_port));
        info!("reader_bot SSE: http://localhost:{}/feed", reader_port);
        let listener = tokio::net::TcpListener::bind(addr).await.expect("bind reader port");
        axum::serve(listener, app).await.expect("reader SSE server failed");
    });

    // ── Subscribe to pool via LaserStream ─────────────────────────────────────
    let mut pool_rx = spawn_pool_stream(&ws_url, pool_pubkey);
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let mut cycle:       u64 = 0;
    let mut last_price:  u64 = 0;
    let mut last_update: i64 = 0;

    info!("reader_bot: listening for oracle updates…");

    loop {
        if pool_rx.changed().await.is_err() {
            warn!("reader_bot: pool stream closed");
            break;
        }

        let snap = match pool_rx.borrow().clone() {
            Some(s) => s,
            None    => continue,
        };

        if snap.last_update == last_update || snap.oracle_price == 0 {
            continue;
        }
        last_update = snap.last_update;
        cycle += 1;

        info!(
            "[#{cycle}] oracle={:.9}  ra={}  rb={}  spread={}bps",
            snap.oracle_price as f64 / 1e9,
            snap.reserve_a, snap.reserve_b, snap.spread_bps,
        );

        if last_price == 0 {
            last_price = snap.oracle_price;
            info!("[#{cycle}] seed: first tick, waiting for next move");
            continue;
        }

        let price_move_bps = if snap.oracle_price > last_price {
            (snap.oracle_price - last_price) * 10_000 / last_price
        } else {
            (last_price - snap.oracle_price) * 10_000 / last_price
        };

        if price_move_bps < MOMENTUM_BPS {
            debug!("[#{cycle}] skip: {}bps < {}bps threshold", price_move_bps, MOMENTUM_BPS);
            last_price = snap.oracle_price;
            continue;
        }

        // price UP → B→A (buy A), price DOWN → A→B (sell A)
        let direction: u8 = if snap.oracle_price > last_price { 1 } else { 0 };
        let dir_label = if direction == 1 { "B→A" } else { "A→B" };

        info!("[#{cycle}] TRADE {dir_label}  move={price_move_bps}bps  spread≈{}bps", snap.spread_bps);

        let reserve_out = if direction == 1 { snap.reserve_a } else { snap.reserve_b };
        if reserve_out < TRADE_AMOUNT {
            warn!("[#{cycle}] skip: reserve_out {} < {}", reserve_out, TRADE_AMOUNT);
            last_price = snap.oracle_price;
            continue;
        }

        match get_blockhash(&http, &rpc_url).await {
            Ok(bh) => {
                match send_swap(
                    &http, &rpc_url, &wallet, &program_id,
                    &pool_pubkey, &pool_auth,
                    &vault_a, &vault_b, &user_a, &user_b,
                    direction, TRADE_AMOUNT, bh,
                ).await {
                    Ok(sig) => {
                        info!("[#{cycle}] ok: {}", sig);
                        let trade = Trade {
                            cycle,
                            direction: if direction == 1 { "B→A" } else { "A→B" },
                            amount_in: TRADE_AMOUNT,
                            oracle_price: snap.oracle_price,
                            price_move: price_move_bps,
                            sig,
                        };
                        let mut b = book.write().await;
                        b.record(trade);
                        let trade_count = b.trades.len();
                        if trade_count % SUMMARY_INTERVAL == 0 {
                            b.print_summary();
                        }
                    }
                    Err(e) => error!("[#{cycle}] swap failed: {:#}", e),
                }
            }
            Err(e) => error!("[#{cycle}] blockhash: {:#}", e),
        }

        last_price = snap.oracle_price;
    }

    book.read().await.print_summary();
    Ok(())
}

// ── SSE handlers ──────────────────────────────────────────────────────────────

async fn feed_handler(
    axum::extract::State(s): axum::extract::State<ServerState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let feed = stream! {
        loop {
            let data = {
                let b = s.book.read().await;
                let now = SystemTime::now().duration_since(UNIX_EPOCH)
                    .unwrap_or_default().as_secs();
                serde_json::json!({
                    "ts":           now,
                    "trades":       b.trades.len(),
                    "volume_b":     b.total_volume_b,
                    "net_a":        b.net_a,
                    "net_b":        b.net_b,
                    "spread_paid_b": b.spread_paid_b,
                    "mtm_pnl_b":    b.mtm_pnl_b(),
                    "pnl_bps":      b.pnl_bps(),
                    "last_oracle":  b.last_oracle,
                    "last_dir":     b.last_direction,
                    "last_sig":     b.last_sig,
                    "last_move_bps": b.last_move_bps,
                    // last 20 trades for the log
                    "recent": b.trades.iter().rev().take(20).map(|t| serde_json::json!({
                        "cycle":     t.cycle,
                        "dir":       t.direction,
                        "amount_in": t.amount_in,
                        "oracle":    t.oracle_price,
                        "move_bps":  t.price_move,
                        "sig":       &t.sig[..t.sig.len().min(16)],
                    })).collect::<Vec<_>>(),
                }).to_string()
            };
            yield Ok::<Event, Infallible>(
                Event::default().event("reader").data(data)
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    };
    Sse::new(feed).keep_alive(KeepAlive::default())
}

async fn health_handler(
    axum::extract::State(s): axum::extract::State<ServerState>,
) -> Json<serde_json::Value> {
    let b = s.book.read().await;
    Json(serde_json::json!({
        "status":       "running",
        "trades":       b.trades.len(),
        "pnl_bps":      b.pnl_bps(),
        "last_oracle":  b.last_oracle,
    }))
}

// ── LaserStream ───────────────────────────────────────────────────────────────

fn spawn_pool_stream(ws_url: &str, pubkey: Pubkey) -> watch::Receiver<Option<PoolState>> {
    let ws_url = ws_url.to_string();
    let (tx, rx) = watch::channel::<Option<PoolState>>(None);
    tokio::spawn(async move {
        loop {
            match run_pool_stream(&ws_url, &pubkey, &tx).await {
                Ok(())  => info!("laserstream: ended cleanly — reconnecting in 2s"),
                Err(e)  => warn!("laserstream: {:#} — reconnecting in 2s", e),
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
    rx
}

async fn run_pool_stream(
    ws_url: &str,
    pubkey: &Pubkey,
    tx:     &watch::Sender<Option<PoolState>>,
) -> Result<()> {
    info!("laserstream: connecting {}", ws_url);
    let (mut ws, _) = connect_async(ws_url).await
        .map_err(|e| anyhow::anyhow!("WS connect: {e}"))?;

    let sub = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "accountSubscribe",
        "params": [pubkey.to_string(), { "encoding": "base64", "commitment": "confirmed" }]
    });
    ws.send(WsMessage::Text(sub.to_string())).await
        .map_err(|e| anyhow::anyhow!("subscribe: {e}"))?;

    let mut sub_id: Option<u64> = None;

    while let Some(msg) = ws.next().await {
        let text = match msg.map_err(|e| anyhow::anyhow!("ws recv: {e}"))? {
            WsMessage::Text(t)   => t,
            WsMessage::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            WsMessage::Ping(p)   => { ws.send(WsMessage::Pong(p)).await.ok(); continue; }
            WsMessage::Close(_)  => return Ok(()),
            _                    => continue,
        };

        let raw: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v, Err(e) => { warn!("json: {e}"); continue; }
        };

        if raw.get("id") == Some(&serde_json::json!(1)) {
            if let Some(id) = raw["result"].as_u64() {
                sub_id = Some(id);
                info!("laserstream: subscribed sub_id={}", id);
            } else {
                return Err(anyhow::anyhow!("subscribe rejected: {}", raw));
            }
            continue;
        }

        if raw["method"].as_str() != Some("accountNotification") { continue; }
        let params = &raw["params"];
        if params["subscription"].as_u64() != sub_id { continue; }

        let b64 = match params["result"]["value"]["data"][0].as_str() {
            Some(s) => s, None => { warn!("no data"); continue; }
        };
        let data = match base64::engine::general_purpose::STANDARD.decode(b64) {
            Ok(b) => b, Err(e) => { warn!("b64: {e}"); continue; }
        };

        debug!("laserstream: slot={} len={}", params["result"]["context"]["slot"], data.len());
        if let Some(snap) = PoolState::from_bytes(&data) {
            let _ = tx.send(Some(snap));
        }
    }
    Ok(())
}

// ── RPC helpers ───────────────────────────────────────────────────────────────

async fn get_blockhash(http: &reqwest::Client, rpc_url: &str) -> Result<Hash> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "getLatestBlockhash",
        "params": [{ "commitment": "confirmed" }]
    });
    let resp: serde_json::Value = http.post(rpc_url).json(&body)
        .send().await.context("getLatestBlockhash")?
        .json().await.context("parse blockhash")?;
    resp["result"]["value"]["blockhash"].as_str()
        .context("missing blockhash")?
        .parse::<Hash>().context("parse Hash")
}

async fn send_tx(http: &reqwest::Client, rpc_url: &str, tx: &Transaction) -> Result<String> {
    let enc = base64::engine::general_purpose::STANDARD
        .encode(bincode::serialize(tx).context("serialize tx")?);
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "sendTransaction",
        "params": [enc, { "encoding": "base64", "skipPreflight": false, "preflightCommitment": "confirmed" }]
    });
    let resp: serde_json::Value = http.post(rpc_url).json(&body)
        .send().await.context("sendTransaction")?
        .json().await.context("parse sendTransaction")?;
    if let Some(err) = resp.get("error") {
        anyhow::bail!("RPC error: {}", err);
    }
    resp["result"].as_str().map(|s| s.to_string()).context("missing sig")
}

// ── SWAP instruction ──────────────────────────────────────────────────────────

async fn send_swap(
    http: &reqwest::Client, rpc_url: &str, wallet: &Keypair,
    program_id: &Pubkey, pool: &Pubkey, pool_auth: &Pubkey,
    vault_a: &Pubkey, vault_b: &Pubkey,
    user_a: &Pubkey, user_b: &Pubkey,
    direction: u8, amount_in: u64, blockhash: Hash,
) -> Result<String> {
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID).unwrap();
    let (user_in, vault_in, user_out, vault_out) = if direction == 0 {
        (user_a, vault_a, user_b, vault_b)
    } else {
        (user_b, vault_b, user_a, vault_a)
    };

    let mut data = Vec::with_capacity(18);
    data.push(2u8);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // min_out=0 on devnet
    data.push(direction);

    let swap_ix = Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*pool,      false),
            AccountMeta::new(*user_in,   false),
            AccountMeta::new(*vault_in,  false),
            AccountMeta::new(*user_out,  false),
            AccountMeta::new(*vault_out, false),
            AccountMeta::new_readonly(wallet.pubkey(), true),
            AccountMeta::new_readonly(*pool_auth, false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data,
    };

    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let msg = Message::new(&[cu_ix, swap_ix], Some(&wallet.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.sign(&[wallet], blockhash);
    send_tx(http, rpc_url, &tx).await
}
