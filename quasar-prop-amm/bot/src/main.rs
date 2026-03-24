use solana_sdk::{pubkey::Pubkey, signature::Keypair};
use solana_client::rpc_client::RpcClient;
use std::str::FromStr;
use tokio::time::{sleep, Duration};
use dotenv::dotenv;
use bs58;
use std::collections::VecDeque;

// use jito_bundle::{...}; // 伪代码，需替换为实际 Jito Bundle SDK

const PRICE_HISTORY_LEN: usize = 20;

#[tokio::main]
async fn main() {
    dotenv().ok();

    let rpc_url = std::env::var("RPC_URL").unwrap();
    let wallet = Keypair::from_bytes(&bs58::decode(std::env::var("WALLET_PRIVATE_KEY").unwrap()).into_vec().unwrap()).unwrap();
    let pool_pubkey = Pubkey::from_str(&std::env::var("POOL_PUBKEY").unwrap()).unwrap();
    let jito_url = std::env::var("JITO_RELAY").unwrap();
    let harmonic_url = std::env::var("HARMONIC_RELAY").unwrap_or_default();
    let tip_lamports: u64 = std::env::var("TIP_LAMPORTS").unwrap().parse().unwrap();

    let client = RpcClient::new(rpc_url);
    println!("🚀 Oracle Bot (Jito Bundle) 启动");

    let mut price_history: VecDeque<u64> = VecDeque::with_capacity(PRICE_HISTORY_LEN);

    loop {
        // 1. 实时读取链上库存
        let pool_data = client.get_account_data(&pool_pubkey).unwrap();
        let reserve0 = u64::from_le_bytes(pool_data[8..16].try_into().unwrap());
        let reserve1 = u64::from_le_bytes(pool_data[16..24].try_into().unwrap());

        // 2. 获取 Pyth/Chainlink 最新价格（需用实际 SDK 替换）
        let price = get_latest_price().await.unwrap_or(1_000_000_000u64);
        if price_history.len() == PRICE_HISTORY_LEN { price_history.pop_front(); }
        price_history.push_back(price);

        // 3. 计算动态波动率（基于历史价格）
        let volatility = calculate_volatility(&price_history).unwrap_or(4500u32);
        // 4. 计算 spread/skew 参数
        let base_spread = 8u32;
        let vol_factor = 5000u32;
        let skew_factor = 18000u32;
        let target_ratio = 9800u64;

        // 5. 构建 update_oracle 指令
        let ix = build_update_oracle_ix(
            &pool_pubkey,
            base_spread,
            vol_factor,
            skew_factor,
            target_ratio,
            get_current_slot().unwrap_or(0),
        );
        // 6. 构建 tip 指令
        let tip_ix = build_tip_ix(tip_lamports);
        // 7. 构建 bundle（主交易+tip，tip最后，最多5笔）
        let mut bundle = build_bundle(vec![ix, tip_ix]);
        if bundle.len() > 5 { bundle.truncate(5); }

        // 8. simulateBundle 检查（CU/冲突）
        let cu = simulate_bundle_cu(&bundle);
        let tip_per_cu = if cu > 0 { tip_lamports as f64 / cu as f64 } else { 0.0 };
        println!("[Bundle] CU: {cu}, Tip: {tip_lamports}, Tip/CU: {tip_per_cu:.2}");
        if cu > 0 && tip_per_cu >= 1.0 {
            // 9. 发送到 Jito + Harmonic，避免重复、失败重试、优先级控制
            let mut sent = false;
            if send_bundle_with_result(&bundle, &jito_url) {
                sent = true;
                println!("[Jito] Bundle sent successfully");
            } else {
                println!("[Jito] Bundle send failed");
            }
            if !harmonic_url.is_empty() {
                if send_bundle_with_result(&bundle, &harmonic_url) {
                    sent = true;
                    println!("[Harmonic] Bundle sent successfully");
                } else {
                    println!("[Harmonic] Bundle send failed");
                }
            }
            if !sent {
                println!("[Bundle] All relays failed, will retry next round");
            }
        }
        sleep(Duration::from_secs(3)).await;
    }
}

// 以下为伪函数，需用实际 SDK 实现
async fn get_latest_price() -> Option<u64> {
    // TODO: 集成 Pyth/Chainlink SDK 获取真实价格
    Some(1_000_000_000)
}
fn calculate_volatility(history: &VecDeque<u64>) -> Option<u32> {
    if history.len() < 2 { return None; }
    let mean = history.iter().copied().sum::<u64>() as f64 / history.len() as f64;
    let var = history.iter().map(|p| (*p as f64 - mean).powi(2)).sum::<f64>() / (history.len() as f64 - 1.0);
    Some((var.sqrt() / mean * 10000.0) as u32)
}
fn build_update_oracle_ix(_pool: &Pubkey, _base: u32, _vol: u32, _skew: u32, _target: u64, _slot: u64) -> Vec<u8> { vec![] }
fn build_tip_ix(_tip: u64) -> Vec<u8> { vec![] }
fn build_bundle(ixs: Vec<Vec<u8>>) -> Vec<Vec<u8>> { ixs }
fn simulate_bundle_cu(_bundle: &Vec<Vec<u8>>) -> u64 { 10000 } // TODO: 用真实 simulateBundle 返回 CU
fn send_bundle(_bundle: &Vec<Vec<u8>>, _url: &str) {
    // 兼容旧接口
    let _ = send_bundle_with_result(_bundle, _url);
}

fn send_bundle_with_result(_bundle: &Vec<Vec<u8>>, _url: &str) -> bool {
    // TODO: 实际实现应返回 true/false 表示发送是否成功
    true
}
fn get_current_slot() -> Option<u64> { Some(0) }
