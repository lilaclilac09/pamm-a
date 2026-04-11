#![no_std]

use pinocchio::{AccountView, Address, ProgramResult};
use pinocchio::error::ProgramError;
use pinocchio_token::instructions::{Burn, MintTo, Transfer};

mod state;

// no_std requires nostd_panic_handler; entrypoint! uses default_panic_handler (std only)
pinocchio::program_entrypoint!(process_instruction);
pinocchio::default_allocator!();
pinocchio::nostd_panic_handler!();

const INIT_POOL: u8 = 0;
const UPDATE_ORACLE: u8 = 1;
const SWAP: u8 = 2;
const ADD_LIQUIDITY: u8 = 3;
const REMOVE_LIQUIDITY: u8 = 4;

pub fn process_instruction(
    _program_id: &Address,
    accounts: &[AccountView],
    data: &[u8],
) -> ProgramResult {
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }

    match data[0] {
        INIT_POOL => initialize_pool(accounts),
        UPDATE_ORACLE => update_oracle(accounts, &data[1..]),
        SWAP => swap(accounts, &data[1..]),
        ADD_LIQUIDITY => add_liquidity(accounts, &data[1..]),
        REMOVE_LIQUIDITY => remove_liquidity(accounts, &data[1..]),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn initialize_pool(accounts: &[AccountView]) -> ProgramResult {
    let pool = &accounts[0];
    let mut data = pool.try_borrow_mut()?;

    data[72..80].copy_from_slice(&1_000_000_000u64.to_le_bytes()); // mid_price
    data[80..84].copy_from_slice(&10u32.to_le_bytes());             // base_spread (10 bps)
    data[84..88].copy_from_slice(&5000u32.to_le_bytes());           // vol_factor
    data[88..92].copy_from_slice(&15000u32.to_le_bytes());          // skew_factor
    data[92..100].copy_from_slice(&10000u64.to_le_bytes());         // target_ratio (100% base)

    Ok(())
}

fn update_oracle(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    let pool = &accounts[0];
    if !pool.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    if data.len() < 28 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let new_mid_price:    u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let new_base_spread:  u32 = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let new_vol_factor:   u32 = u32::from_le_bytes(data[12..16].try_into().unwrap());
    let new_skew_factor:  u32 = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let new_target_ratio: u64 = u64::from_le_bytes(data[20..28].try_into().unwrap());

    let mut pool_data = pool.try_borrow_mut()?;

    pool_data[72..80].copy_from_slice(&new_mid_price.to_le_bytes());
    pool_data[80..84].copy_from_slice(&new_base_spread.to_le_bytes());
    pool_data[84..88].copy_from_slice(&new_vol_factor.to_le_bytes());
    pool_data[88..92].copy_from_slice(&new_skew_factor.to_le_bytes());
    pool_data[92..100].copy_from_slice(&new_target_ratio.to_le_bytes());

    Ok(())
}

fn swap(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    // Account layout:
    // 0 = pool (writable)
    // 1 = user_in  (writable) — user's input token account
    // 2 = vault_in (writable) — pool's input vault
    // 3 = user_out (writable) — user's output token account
    // 4 = vault_out (writable) — pool's output vault
    // 5 = user     (signer)   — authority for user_in transfer
    // 6 = pool_authority (signer/PDA) — authority for vault_out transfer
    if accounts.len() < 7 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    if data.len() < 16 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let pool         = &accounts[0];
    let user_in      = &accounts[1];
    let vault_in     = &accounts[2];
    let user_out     = &accounts[3];
    let vault_out    = &accounts[4];
    let user         = &accounts[5];
    let pool_auth    = &accounts[6];

    let amount_in: u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let min_out:   u64 = u64::from_le_bytes(data[8..16].try_into().unwrap());

    let (reserve_in, reserve_out, base_spread, vol_factor, skew_factor, target_ratio) = {
        let d = pool.try_borrow()?;
        (
            u64::from_le_bytes(d[8..16].try_into().unwrap()),
            u64::from_le_bytes(d[16..24].try_into().unwrap()),
            u32::from_le_bytes(d[80..84].try_into().unwrap()),
            u32::from_le_bytes(d[84..88].try_into().unwrap()),
            u32::from_le_bytes(d[88..92].try_into().unwrap()),
            u64::from_le_bytes(d[92..100].try_into().unwrap()),
        )
    };

    if reserve_in == 0 || reserve_out == 0 {
        return Err(ProgramError::Custom(1));
    }

    // Dynamic spread: bot passes actual volatility encoded in vol_factor
    // Program uses 4500 as the fixed base multiplier
    let volatility = 4500u32;
    let dynamic_spread = base_spread + (vol_factor * volatility / 10000);

    let current_ratio = (reserve_in * 10000) / (reserve_in + reserve_out);
    let deviation = current_ratio as i64 - target_ratio as i64;
    let skew_adjust = (deviation * deviation / 8000) * skew_factor as i64 / 10000;

    let effective_spread = (dynamic_spread as i64 + skew_adjust).max(0) as u32;

    let base_out = (amount_in * reserve_out) / (reserve_in + amount_in);
    let spread_adj = (base_out * effective_spread as u64) / 10000;
    let final_out = base_out.saturating_sub(spread_adj);

    if final_out < min_out {
        return Err(ProgramError::Custom(2));
    }

    // User sends input tokens to pool vault
    Transfer { from: user_in, to: vault_in, authority: user, amount: amount_in }.invoke()?;

    // Pool sends output tokens to user
    Transfer { from: vault_out, to: user_out, authority: pool_auth, amount: final_out }.invoke()?;

    // Update reserves
    let mut d = pool.try_borrow_mut()?;
    d[8..16].copy_from_slice(&(reserve_in + amount_in).to_le_bytes());
    d[16..24].copy_from_slice(&(reserve_out - final_out).to_le_bytes());

    Ok(())
}

fn isqrt(n: u64) -> u64 {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x { x = y; y = (x + n / x) / 2; }
    x
}

fn add_liquidity(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    // Account layout:
    // 0 = pool        (writable)
    // 1 = user_a      (writable) — user's token A account
    // 2 = vault_a     (writable) — pool's token A vault
    // 3 = user_b      (writable) — user's token B account
    // 4 = vault_b     (writable) — pool's token B vault
    // 5 = lp_mint     (writable) — LP token mint (authority = pool_auth)
    // 6 = user_lp     (writable) — user's LP token account
    // 7 = user        (signer)
    // 8 = pool_auth   (signer)   — pool keypair, mint authority
    if accounts.len() < 9 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    if data.len() < 16 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let pool      = &accounts[0];
    let user_a    = &accounts[1];
    let vault_a   = &accounts[2];
    let user_b    = &accounts[3];
    let vault_b   = &accounts[4];
    let lp_mint   = &accounts[5];
    let user_lp   = &accounts[6];
    let user      = &accounts[7];
    let pool_auth = &accounts[8];

    let amount_a: u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let amount_b: u64 = u64::from_le_bytes(data[8..16].try_into().unwrap());

    if amount_a == 0 || amount_b == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let (reserve_a, reserve_b, lp_supply) = {
        let d = pool.try_borrow()?;
        (
            u64::from_le_bytes(d[8..16].try_into().unwrap()),
            u64::from_le_bytes(d[16..24].try_into().unwrap()),
            u64::from_le_bytes(d[24..32].try_into().unwrap()),
        )
    };

    // LP minted: sqrt(a*b) on first deposit, proportional on subsequent
    let lp_amount = if lp_supply == 0 {
        isqrt(amount_a.saturating_mul(amount_b))
    } else {
        // min of proportional shares to enforce correct ratio
        let share_a = (amount_a as u128 * lp_supply as u128 / reserve_a as u128) as u64;
        let share_b = (amount_b as u128 * lp_supply as u128 / reserve_b as u128) as u64;
        share_a.min(share_b)
    };

    if lp_amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }

    Transfer { from: user_a, to: vault_a, authority: user,      amount: amount_a }.invoke()?;
    Transfer { from: user_b, to: vault_b, authority: user,      amount: amount_b }.invoke()?;
    MintTo   { mint: lp_mint, account: user_lp, mint_authority: pool_auth, amount: lp_amount }.invoke()?;

    let mut d = pool.try_borrow_mut()?;
    d[8..16].copy_from_slice(&(reserve_a + amount_a).to_le_bytes());
    d[16..24].copy_from_slice(&(reserve_b + amount_b).to_le_bytes());
    d[24..32].copy_from_slice(&(lp_supply + lp_amount).to_le_bytes());

    Ok(())
}

fn remove_liquidity(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    // Account layout:
    // 0 = pool        (writable)
    // 1 = vault_a     (writable) — pool's token A vault
    // 2 = user_a      (writable) — user's token A account
    // 3 = vault_b     (writable) — pool's token B vault
    // 4 = user_b      (writable) — user's token B account
    // 5 = lp_mint     (writable) — LP token mint
    // 6 = user_lp     (writable) — user's LP token account (burn from here)
    // 7 = pool_auth   (signer)   — vault authority
    // 8 = user        (signer)   — LP token authority
    if accounts.len() < 9 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    if data.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let pool      = &accounts[0];
    let vault_a   = &accounts[1];
    let user_a    = &accounts[2];
    let vault_b   = &accounts[3];
    let user_b    = &accounts[4];
    let lp_mint   = &accounts[5];
    let user_lp   = &accounts[6];
    let pool_auth = &accounts[7];
    let user      = &accounts[8];

    let lp_amount: u64 = u64::from_le_bytes(data[0..8].try_into().unwrap());
    if lp_amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let (reserve_a, reserve_b, lp_supply) = {
        let d = pool.try_borrow()?;
        (
            u64::from_le_bytes(d[8..16].try_into().unwrap()),
            u64::from_le_bytes(d[16..24].try_into().unwrap()),
            u64::from_le_bytes(d[24..32].try_into().unwrap()),
        )
    };

    if lp_supply == 0 || lp_amount > lp_supply {
        return Err(ProgramError::InsufficientFunds);
    }

    // Proportional withdrawal
    let out_a = (lp_amount as u128 * reserve_a as u128 / lp_supply as u128) as u64;
    let out_b = (lp_amount as u128 * reserve_b as u128 / lp_supply as u128) as u64;

    Burn     { account: user_lp, mint: lp_mint, authority: user,      amount: lp_amount }.invoke()?;
    Transfer { from: vault_a, to: user_a, authority: pool_auth, amount: out_a }.invoke()?;
    Transfer { from: vault_b, to: user_b, authority: pool_auth, amount: out_b }.invoke()?;

    let mut d = pool.try_borrow_mut()?;
    d[8..16].copy_from_slice(&(reserve_a - out_a).to_le_bytes());
    d[16..24].copy_from_slice(&(reserve_b - out_b).to_le_bytes());
    d[24..32].copy_from_slice(&(lp_supply - lp_amount).to_le_bytes());

    Ok(())
}
