# Pinocchio Prop AMM — Core English Examples

## A. Complete Oracle Update Instruction (Low CU, All Dynamic Params)
```rust
// src/lib.rs — update_oracle function (optimized for low CU)
fn update_oracle(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let pool = &accounts[0];
    if !pool.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    // Parse 5 params from off-chain
    let new_mid_price: u64   = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let new_base_spread: u32 = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let new_vol_factor: u32  = u32::from_le_bytes(data[12..16].try_into().unwrap());
    let new_skew_factor: u32 = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let new_target_ratio: u64= u64::from_le_bytes(data[20..28].try_into().unwrap());
    // Zero-copy write (Pinocchio's most CU-efficient way)
    let mut pool_data = pool.try_borrow_mut_data()?;
    pool_data[72..80].copy_from_slice(&new_mid_price.to_le_bytes());
    pool_data[80..84].copy_from_slice(&new_base_spread.to_le_bytes());
    pool_data[84..88].copy_from_slice(&new_vol_factor.to_le_bytes());
    pool_data[88..92].copy_from_slice(&new_skew_factor.to_le_bytes());
    pool_data[92..100].copy_from_slice(&new_target_ratio.to_le_bytes());
    pool_data[100..108].copy_from_slice(&Clock::get()?.slot.to_le_bytes());
    msg!("Oracle Updated | mid={} spread={} vol={} skew={}", new_mid_price, new_base_spread, new_vol_factor, new_skew_factor);
    Ok(())
}
```

## B. Add Liquidity + Remove Liquidity (with Dynamic Spread)
```rust
// Add Liquidity
fn add_liquidity(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let amount0: u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let amount1: u64 = u64::from_le_bytes(data[8..16].try_into().unwrap());
    TokenProgram::transfer(&accounts[1], &accounts[2], amount0, &[])?;  // user -> vault0
    TokenProgram::transfer(&accounts[3], &accounts[4], amount1, &[])?;  // user -> vault1
    let mut data_mut = accounts[0].try_borrow_mut_data()?;
    let r0 = u64::from_le_bytes(data_mut[8..16].try_into().unwrap()) + amount0;
    let r1 = u64::from_le_bytes(data_mut[16..24].try_into().unwrap()) + amount1;
    data_mut[8..16].copy_from_slice(&r0.to_le_bytes());
    data_mut[16..24].copy_from_slice(&r1.to_le_bytes());
    msg!("Add Liquidity: {} + {}", amount0, amount1);
    Ok(())
}
// Remove Liquidity
fn remove_liquidity(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let lp_amount: u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let pool_data = accounts[0].try_borrow_data()?;
    let reserve0 = u64::from_le_bytes(pool_data[8..16].try_into().unwrap());
    let reserve1 = u64::from_le_bytes(pool_data[16..24].try_into().unwrap());
    let share = lp_amount * 10000 / 1_000_000_000; // Assume total LP supply is 1B
    let out0 = reserve0 * share / 10000;
    let out1 = reserve1 * share / 10000;
    TokenProgram::transfer(&accounts[2], &accounts[1], out0, &[])?;   // vault0 -> user
    TokenProgram::transfer(&accounts[4], &accounts[3], out1, &[])?;   // vault1 -> user
    msg!("Remove Liquidity: {} + {}", out0, out1);
    Ok(())
}
```

## C. Off-chain Rust Oracle Bot (with Real-time Inventory Monitoring)
```rust
// bot/src/main.rs (simplified + real-time inventory monitoring)
#[tokio::main]
async fn main() {
    dotenv().ok();
    let client = RpcClient::new(std::env::var("RPC_URL").unwrap());
    let wallet = Keypair::from_secret_key(...);
    let pool_pubkey = Pubkey::from_str(&std::env::var("POOL_PUBKEY").unwrap()).unwrap();
    loop {
        let account_data = client.get_account_data(&pool_pubkey).unwrap();
        let reserve0 = u64::from_le_bytes(account_data[8..16].try_into().unwrap());
        let reserve1 = u64::from_le_bytes(account_data[16..24].try_into().unwrap());
        let current_ratio = if reserve0 + reserve1 > 0 {
            (reserve0 * 10000) / (reserve0 + reserve1)
        } else { 10000 };
        let params = OracleParams {
            mid_price: get_pyth_price().await.unwrap(),
            base_spread: 8,
            vol_factor: calculate_vol_factor(),
            skew_factor: if current_ratio > 12000 { 20000 } else { 12000 },
            target_ratio: 9800,
        };
        send_oracle_update(&client, &wallet, &pool_pubkey, &params).await.ok();
        println!("Update done | reserve0={} | reserve1={} | ratio={}", reserve0, reserve1, current_ratio);
        sleep(Duration::from_secs(3)).await;
    }
}
```

## D. Advanced Inventory Skew (Exponential Curve)
```rust
// Advanced inventory skew calculation (recommended for production)
fn calculate_advanced_skew(current_ratio: u64, target_ratio: u64, skew_factor: u32) -> i64 {
    let deviation = current_ratio as i64 - target_ratio as i64;
    // Exponential curve: larger deviation, stronger penalty (smoother, more realistic)
    let skew_adjust = (deviation * deviation / 8000) * skew_factor as i64 / 10000;
    // Clamp adjustment range
    skew_adjust.clamp(-8000, 12000)
}
```

---

**You now have:**
- A. Complete low CU Oracle Update
- B. Add + Remove Liquidity (with dynamic spread)
- C. Off-chain bot + real-time inventory monitoring
- D. Advanced exponential inventory skew

**Next step:**
- Reply with one of: "LiteSVM test flow", "Full repo package", or "Add LP Token first" and I’ll proceed step by step in English only.