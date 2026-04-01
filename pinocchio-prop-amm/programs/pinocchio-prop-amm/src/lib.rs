#![no_std]
#![no_main]

use pinocchio::account_info::AccountInfo;
use pinocchio::entrypoint;
use pinocchio::msg;
use pinocchio::ProgramResult;
use pinocchio::pubkey::Pubkey;
use pinocchio::program_error::ProgramError;
use pinocchio_token::TokenProgram;

mod state;

entrypoint!(process_instruction);

const INIT_POOL: u8 = 0;
const UPDATE_ORACLE: u8 = 1;
const SWAP: u8 = 2;

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }

    match data[0] {
        INIT_POOL => initialize_pool(accounts),
        UPDATE_ORACLE => update_oracle(accounts, &data[1..]),
        SWAP => swap(accounts, &data[1..]),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn initialize_pool(accounts: &[AccountInfo]) -> ProgramResult {
    let pool = &accounts[0];
    let mut data = pool.try_borrow_mut_data()?;

    data[72..80].copy_from_slice(&1_000_000_000u64.to_le_bytes());
    data[80..84].copy_from_slice(&10u32.to_le_bytes());
    data[84..88].copy_from_slice(&5000u32.to_le_bytes());
    data[88..92].copy_from_slice(&15000u32.to_le_bytes());
    data[92..100].copy_from_slice(&10000u64.to_le_bytes());

    msg!("✅ Prop AMM Pool Initialized");
    Ok(())
}

fn update_oracle(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let pool = &accounts[0];
    if !pool.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    let new_mid_price: u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let new_base_spread: u32 = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let new_vol_factor: u32 = u32::from_le_bytes(data[12..16].try_into().unwrap());
    let new_skew_factor: u32 = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let new_target_ratio: u64 = u64::from_le_bytes(data[20..28].try_into().unwrap());

    let mut pool_data = pool.try_borrow_mut_data()?;

    pool_data[72..80].copy_from_slice(&new_mid_price.to_le_bytes());
    pool_data[80..84].copy_from_slice(&new_base_spread.to_le_bytes());
    pool_data[84..88].copy_from_slice(&new_vol_factor.to_le_bytes());
    pool_data[88..92].copy_from_slice(&new_skew_factor.to_le_bytes());
    pool_data[92..100].copy_from_slice(&new_target_ratio.to_le_bytes());

    msg!("Oracle Updated");
    Ok(())
}

fn swap(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let pool = &accounts[0];
    let user_in = &accounts[1];
    let vault_in = &accounts[2];
    let user_out = &accounts[3];
    let vault_out = &accounts[4];

    let amount_in: u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let min_out: u64 = u64::from_le_bytes(data[8..16].try_into().unwrap());

    let pool_data = pool.try_borrow_data()?;
    let reserve_in = u64::from_le_bytes(pool_data[8..16].try_into().unwrap());
    let reserve_out = u64::from_le_bytes(pool_data[16..24].try_into().unwrap());
    let base_spread = u32::from_le_bytes(pool_data[80..84].try_into().unwrap());
    let vol_factor = u32::from_le_bytes(pool_data[84..88].try_into().unwrap());
    let skew_factor = u32::from_le_bytes(pool_data[88..92].try_into().unwrap());
    let target_ratio = u64::from_le_bytes(pool_data[92..100].try_into().unwrap());

    if reserve_in == 0 || reserve_out == 0 {
        return Err(ProgramError::Custom(1));
    }

    let volatility = 4500u32;
    let dynamic_spread = base_spread + (vol_factor * volatility / 10000);

    let current_ratio = (reserve_in * 10000) / (reserve_in + reserve_out);
    let deviation = current_ratio as i64 - target_ratio as i64;
    let skew_adjust = (deviation * deviation / 8000) * skew_factor as i64 / 10000;

    let effective_spread = (dynamic_spread as i64 + skew_adjust).max(0) as u32;

    let base_out = (amount_in * reserve_out) / (reserve_in + amount_in);
    let spread_adj = (base_out * effective_spread as u64) / 10000;
    let final_out = base_out - spread_adj;

    if final_out < min_out {
        return Err(ProgramError::Custom(2));
    }

    TokenProgram::transfer(user_in, vault_in, amount_in, &[])?;
    TokenProgram::transfer(vault_out, user_out, final_out, &[])?;

    let mut data_mut = pool.try_borrow_mut_data()?;
    data_mut[8..16].copy_from_slice(&(reserve_in + amount_in).to_le_bytes());
    data_mut[16..24].copy_from_slice(&(reserve_out - final_out).to_le_bytes());

    msg!("Swap | spread={}bps | skew={} | out={}", effective_spread, skew_adjust, final_out);
    Ok(())
}