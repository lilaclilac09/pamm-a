//! Pinocchio Prop AMM — Oracle Bot
//!
//! 职责：每隔 N 秒从 Pyth Hermes 获取最新价格和置信度，
//! 读取链上池子库存状态，计算动态 spread/skew 参数，
//! 调用 UPDATE_ORACLE 指令更新 AMM 定价。

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::{collections::VecDeque, str::FromStr};
use tokio::time::{sleep, Duration};
use tracing::{error, info};

// ─── 配置 ────────────────────────────────────────────────────────────────────

struct Config {
    rpc_url: String,
    wallet: Keypair,
    program_id: Pubkey,
    pool_pubkey: Pubkey,
    pyth_price_feed_id: String, // Pyth price feed hex id
    update_interval_secs: u64,

    // 风险参数
    base_spread_bps: u32,  // 最小 spread，单位 bps（1 bps = 0.01%）
    target_ratio_bps: u64, // 目标库存比，单位 bps（5000 = 50/50）
    skew_factor: u32,      // 库存偏斜权重（建议 10000~20000）
    max_vol_factor: u32,   // 波动率最大放大系数（建议 8000）
}

impl Config {
    fn from_env() -> Result<Self> {
        dotenv::dotenv().ok();

        let private_key = std::env::var("PRIVATE_KEY").context("PRIVATE_KEY not set")?;
        let key_bytes = bs58::decode(&private_key)
            .into_vec()
            .context("invalid base58 private key")?;
        let wallet = Keypair::from_bytes(&key_bytes).context("invalid keypair bytes")?;

        Ok(Config {
            rpc_url: std::env::var("RPC_URL")
                .unwrap_or_else(|_| "https://api.devnet.solana.com".into()),
            wallet,
            program_id: Pubkey::from_str(
                &std::env::var("PROGRAM_ID").context("PROGRAM_ID not set")?,
            )?,
            pool_pubkey: Pubkey::from_str(
                &std::env::var("POOL_PUBKEY").context("POOL_PUBKEY not set")?,
            )?,
            // SOL/USD feed id on mainnet
            pyth_price_feed_id: std::env::var("PYTH_FEED_ID").unwrap_or_else(|_| {
                "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d".into()
            }),
            update_interval_secs: std::env::var("UPDATE_INTERVAL_SECS")
                .unwrap_or_else(|_| "5".into())
                .parse()
                .unwrap_or(5),
            base_spread_bps: std::env::var("BASE_SPREAD_BPS")
                .unwrap_or_else(|_| "10".into())
                .parse()
                .unwrap_or(10),
            target_ratio_bps: std::env::var("TARGET_RATIO_BPS")
                .unwrap_or_else(|_| "5000".into())
                .parse()
                .unwrap_or(5000),
            skew_factor: std::env::var("SKEW_FACTOR")
                .unwrap_or_else(|_| "15000".into())
                .parse()
                .unwrap_or(15000),
            max_vol_factor: std::env::var("MAX_VOL_FACTOR")
                .unwrap_or_else(|_| "8000".into())
                .parse()
                .unwrap_or(8000),
        })
    }
}

// ─── Pyth Hermes 响应结构 ─────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct HermesResponse {
    parsed: Vec<HermesParsed>,
}

#[derive(Debug, serde::Deserialize)]
struct HermesParsed {
    price: HermesPrice,
}

#[derive(Debug, serde::Deserialize)]
struct HermesPrice {
    price: String, // 价格整数（需结合 expo）
    conf: String,  // 置信区间整数（需结合 expo）
    expo: i32,
}

// ─── 状态 ─────────────────────────────────────────────────────────────────────

struct BotState {
    price_history: VecDeque<u64>, // 历史价格（scaled to 1e9）
    cycle_count: u64,
}

impl BotState {
    fn new() -> Self {
        BotState {
            price_history: VecDeque::with_capacity(20),
            cycle_count: 0,
        }
    }

    fn push_price(&mut self, price: u64) {
        if self.price_history.len() == 20 {
            self.price_history.pop_front();
        }
        self.price_history.push_back(price);
    }

    /// 基于价格历史估算波动率，返回 0..10000 范围（bps）
    fn historical_volatility_bps(&self) -> u32 {
        let n = self.price_history.len();
        if n < 3 {
            return 4500; // 数据不足时用默认值
        }
        let mean = self.price_history.iter().copied().sum::<u64>() as f64 / n as f64;
        let variance = self
            .price_history
            .iter()
            .map(|&p| (p as f64 - mean).powi(2))
            .sum::<f64>()
            / (n as f64 - 1.0);
        let rel_vol = variance.sqrt() / mean;
        (rel_vol * 10000.0).min(10000.0) as u32
    }
}

// ─── 主循环 ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Pinocchio Prop AMM Oracle Bot starting...");

    let cfg = Config::from_env().context("failed to load config")?;
    let client = RpcClient::new(cfg.rpc_url.clone());
    let http = reqwest::Client::new();
    let mut state = BotState::new();

    info!("RPC:      {}", cfg.rpc_url);
    info!("Pool:     {}", cfg.pool_pubkey);
    info!("Program:  {}", cfg.program_id);
    info!("Interval: {}s | base_spread: {}bps | target_ratio: {}bps",
        cfg.update_interval_secs, cfg.base_spread_bps, cfg.target_ratio_bps);

    loop {
        state.cycle_count += 1;

        match run_cycle(&cfg, &client, &http, &mut state).await {
            Ok(sig) => info!("[{}] UPDATE_ORACLE ok: {}", state.cycle_count, sig),
            Err(e) => error!("[{}] error: {:#}", state.cycle_count, e),
        }

        sleep(Duration::from_secs(cfg.update_interval_secs)).await;
    }
}

async fn run_cycle(
    cfg: &Config,
    client: &RpcClient,
    http: &reqwest::Client,
    state: &mut BotState,
) -> Result<String> {
    // 1. 从 Pyth Hermes 获取最新价格 + 置信度
    let (mid_price_scaled, conf_bps) = fetch_pyth_price(http, &cfg.pyth_price_feed_id)
        .await
        .context("pyth fetch failed")?;

    state.push_price(mid_price_scaled);

    // 2. 读取链上池子库存
    let pool_data = client
        .get_account_data(&cfg.pool_pubkey)
        .context("pool account fetch failed")?;

    let (reserve_in, reserve_out) = parse_reserves(&pool_data)?;

    // 3. 计算当前库存比
    let total = reserve_in + reserve_out;
    let current_ratio_bps: u64 = if total == 0 {
        cfg.target_ratio_bps
    } else {
        reserve_in * 10000 / total
    };

    // 4. 波动率：取 Pyth confidence 和历史波动率的较大值
    let hist_vol = state.historical_volatility_bps();
    let effective_vol = conf_bps.max(hist_vol);

    // 5. vol_factor：把有效波动率映射到 [0, max_vol_factor]
    //    链上公式：dynamic_spread = base_spread + (vol_factor * 4500 / 10000)
    let vol_factor = ((effective_vol as u64 * cfg.max_vol_factor as u64) / 10000)
        .min(cfg.max_vol_factor as u64) as u32;

    // 6. 库存偏斜超过 15% 时扩大 base_spread，吸引反向交易
    let inventory_deviation = (current_ratio_bps as i64 - cfg.target_ratio_bps as i64).abs();
    let skew_bonus = if inventory_deviation > 1500 { 5u32 } else { 0u32 };
    let adjusted_base_spread = cfg.base_spread_bps + skew_bonus;

    info!(
        "price_1e9={} conf={}bps hist_vol={}bps vol_factor={} reserve_in={} reserve_out={} ratio={}bps skew_bonus={}",
        mid_price_scaled, conf_bps, hist_vol, vol_factor,
        reserve_in, reserve_out, current_ratio_bps, skew_bonus
    );

    // 7. 构建并发送 UPDATE_ORACLE 指令
    let sig = send_update_oracle(
        cfg,
        client,
        mid_price_scaled,
        adjusted_base_spread,
        vol_factor,
        cfg.skew_factor,
        cfg.target_ratio_bps,
    )?;

    Ok(sig)
}

// ─── Pyth Hermes 价格获取 ─────────────────────────────────────────────────────

/// 返回 (mid_price_scaled_to_1e9, confidence_bps)
async fn fetch_pyth_price(http: &reqwest::Client, feed_id: &str) -> Result<(u64, u32)> {
    let url = format!(
        "https://hermes.pyth.network/v2/updates/price/latest?ids[]={}&parsed=true",
        feed_id
    );

    let resp: HermesResponse = http
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .context("hermes http request failed")?
        .json()
        .await
        .context("hermes json parse failed")?;

    let parsed = resp
        .parsed
        .first()
        .context("no price data in hermes response")?;

    let price_int: i64 = parsed.price.price.parse().context("invalid price string")?;
    let conf_int: u64 = parsed.price.conf.parse().context("invalid conf string")?;
    let expo = parsed.price.expo;

    if price_int <= 0 {
        anyhow::bail!("non-positive price from pyth: {}", price_int);
    }

    // 归一化到 1e9 精度
    let mid_price_u64: u64 = if expo >= 0 {
        (price_int as u64).saturating_mul(10u64.pow((expo as u32).saturating_add(9)))
    } else {
        let divisor = 10u64.pow(expo.unsigned_abs());
        (price_int as u64)
            .saturating_mul(1_000_000_000)
            .checked_div(divisor)
            .unwrap_or(0)
    };

    // confidence → bps（相对价格的比例）
    let conf_bps = if price_int > 0 && conf_int > 0 {
        let rel = conf_int as f64 / price_int.unsigned_abs() as f64;
        (rel * 10000.0).min(10000.0) as u32
    } else {
        0
    };

    Ok((mid_price_u64, conf_bps))
}

// ─── 池子状态解析 ─────────────────────────────────────────────────────────────

fn parse_reserves(pool_data: &[u8]) -> Result<(u64, u64)> {
    if pool_data.len() < 24 {
        anyhow::bail!("pool account data too short ({} bytes)", pool_data.len());
    }
    let reserve_in = u64::from_le_bytes(pool_data[8..16].try_into().unwrap());
    let reserve_out = u64::from_le_bytes(pool_data[16..24].try_into().unwrap());
    Ok((reserve_in, reserve_out))
}

// ─── 构建并发送 UPDATE_ORACLE 指令 ───────────────────────────────────────────

fn send_update_oracle(
    cfg: &Config,
    client: &RpcClient,
    mid_price: u64,
    base_spread: u32,
    vol_factor: u32,
    skew_factor: u32,
    target_ratio: u64,
) -> Result<String> {
    // 指令数据格式（对应 lib.rs UPDATE_ORACLE handler）
    // byte  0    : discriminant = 1
    // bytes 1..9 : mid_price (u64 le)
    // bytes 9..13: base_spread (u32 le)
    // bytes 13..17: vol_factor (u32 le)
    // bytes 17..21: skew_factor (u32 le)
    // bytes 21..29: target_ratio (u64 le)
    let mut data = Vec::with_capacity(29);
    data.push(1u8);
    data.extend_from_slice(&mid_price.to_le_bytes());
    data.extend_from_slice(&base_spread.to_le_bytes());
    data.extend_from_slice(&vol_factor.to_le_bytes());
    data.extend_from_slice(&skew_factor.to_le_bytes());
    data.extend_from_slice(&target_ratio.to_le_bytes());

    let ix = Instruction {
        program_id: cfg.program_id,
        accounts: vec![AccountMeta::new(cfg.pool_pubkey, false)],
        data,
    };

    let blockhash = client
        .get_latest_blockhash()
        .context("get_latest_blockhash failed")?;

    let msg = Message::new(&[ix], Some(&cfg.wallet.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.sign(&[&cfg.wallet], blockhash);

    let sig = client
        .send_and_confirm_transaction(&tx)
        .context("send_and_confirm_transaction failed")?;

    Ok(sig.to_string())
}
