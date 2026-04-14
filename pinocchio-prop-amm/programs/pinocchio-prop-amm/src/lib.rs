#![no_std]

use pinocchio::{AccountView, Address, ProgramResult};
use pinocchio::cpi::{Seed, Signer};
use pinocchio::error::ProgramError;
use pinocchio::sysvars::Sysvar;
use pinocchio::sysvars::clock::Clock;
use pinocchio_token::instructions::{Burn, MintTo, Transfer};

mod state;
use state::*;

pinocchio::program_entrypoint!(process_instruction);
pinocchio::default_allocator!();
pinocchio::nostd_panic_handler!();

const INIT_POOL:        u8 = 0;
const UPDATE_ORACLE:    u8 = 1;
const SWAP:             u8 = 2;
const ADD_LIQUIDITY:    u8 = 3;
const REMOVE_LIQUIDITY: u8 = 4;
const COLLECT_FEES:     u8 = 5;
const SET_ADMIN:        u8 = 6;

pub fn process_instruction(
    _program_id: &Address,
    accounts: &[AccountView],
    data: &[u8],
) -> ProgramResult {
    if data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    match data[0] {
        INIT_POOL        => initialize_pool(accounts, &data[1..]),
        UPDATE_ORACLE    => update_oracle(accounts, &data[1..]),
        SWAP             => swap(accounts, &data[1..]),
        ADD_LIQUIDITY    => add_liquidity(accounts, &data[1..]),
        REMOVE_LIQUIDITY => remove_liquidity(accounts, &data[1..]),
        COLLECT_FEES     => collect_fees(accounts),
        SET_ADMIN        => set_admin(accounts, &data[1..]),
        _                => Err(ProgramError::InvalidInstructionData),
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// isqrt_u128: Newton's method on u128, returns u64.
/// isqrt((u64::MAX)^2) = u64::MAX, no overflow.
pub fn isqrt_u128(n: u128) -> u64 {
    if n == 0 { return 0; }
    let mut x: u128 = n;
    let mut y: u128 = x / 2 + 1;
    while y < x { x = y; y = (x + n / x) / 2; }
    x as u64
}

#[inline(always)]
fn read_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off+4].try_into().unwrap())
}
#[inline(always)]
fn read_u64(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off+8].try_into().unwrap())
}
#[inline(always)]
fn read_i64(d: &[u8], off: usize) -> i64 {
    i64::from_le_bytes(d[off..off+8].try_into().unwrap())
}
#[inline(always)]
fn write_u32(d: &mut [u8], off: usize, v: u32) {
    d[off..off+4].copy_from_slice(&v.to_le_bytes());
}
#[inline(always)]
fn write_u64(d: &mut [u8], off: usize, v: u64) {
    d[off..off+8].copy_from_slice(&v.to_le_bytes());
}
#[inline(always)]
fn write_i64(d: &mut [u8], off: usize, v: i64) {
    d[off..off+8].copy_from_slice(&v.to_le_bytes());
}

/// Build PDA signer seeds for pool_auth.
/// Seeds: ["pool_auth", pool_key_bytes, &[bump]]
/// Returns a [Seed; 3] that outlives the call site locals.
#[inline(always)]
fn auth_seeds<'a>(prefix: &'a [u8], pool_key: &'a [u8], bump: &'a [u8]) -> [Seed<'a>; 3] {
    [Seed::from(prefix), Seed::from(pool_key), Seed::from(bump)]
}

// ── INIT_POOL (0) ─────────────────────────────────────────────────────────────
//
// Accounts:
//   0  pool   (writable, 304 bytes, pre-allocated by client)
//   1  admin  (signer)
//
// Data (62 bytes after discriminant):
//   [0..32]  admin_pubkey     [u8;32]
//   [32]     pool_auth_bump   u8   (client derives with findProgramAddress)
//   [33]     dead_lp_bump     u8
//   [34..38] fee_bps          u32
//   [38..42] k_param          u32
//   [42..50] target_a         u64
//   [50..58] target_b         u64
//   [58..62] max_staleness    u32

fn initialize_pool(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 2 { return Err(ProgramError::NotEnoughAccountKeys); }
    if data.len() < 62    { return Err(ProgramError::InvalidInstructionData); }

    let pool = &accounts[0];

    let admin_bytes:  [u8;32] = data[0..32].try_into().unwrap();
    let auth_bump:    u8      = data[32];
    let dead_bump:    u8      = data[33];
    let fee_bps:      u32     = u32::from_le_bytes(data[34..38].try_into().unwrap());
    let k_param:      u32     = u32::from_le_bytes(data[38..42].try_into().unwrap());
    let target_a:     u64     = u64::from_le_bytes(data[42..50].try_into().unwrap());
    let target_b:     u64     = u64::from_le_bytes(data[50..58].try_into().unwrap());
    let max_stale:    u32     = u32::from_le_bytes(data[58..62].try_into().unwrap());

    let mut d = pool.try_borrow_mut()?;
    if d.len() < POOL_SIZE { return Err(ProgramError::AccountDataTooSmall); }

    write_u64(&mut d, OFF_MAGIC,        MAGIC);
    d[OFF_ADMIN..OFF_ADMIN+32].copy_from_slice(&admin_bytes);
    d[OFF_AUTH_BUMP] = auth_bump;
    d[OFF_DEAD_BUMP] = dead_bump;
    write_u32(&mut d, OFF_MAX_STALE,    max_stale);
    write_u64(&mut d, OFF_RESERVE_A,    0);
    write_u64(&mut d, OFF_RESERVE_B,    0);
    write_u64(&mut d, OFF_TARGET_A,     target_a);
    write_u64(&mut d, OFF_TARGET_B,     target_b);
    write_u64(&mut d, OFF_LP_SUPPLY,    0);
    write_u64(&mut d, OFF_FEE_A,        0);
    write_u64(&mut d, OFF_FEE_B,        0);
    write_u64(&mut d, OFF_ORACLE_PRICE, 0);
    write_u32(&mut d, OFF_SPREAD_BPS,   0);
    write_u32(&mut d, OFF_K_PARAM,      k_param);
    write_u32(&mut d, OFF_VOL_ADJ,      0);
    write_u32(&mut d, OFF_FEE_BPS,      fee_bps);
    write_i64(&mut d, OFF_LAST_UPDATE,  i64::MIN); // intentionally stale
    d[OFF_TWAP_CURSOR] = 0;
    for b in &mut d[OFF_TWAP_OBS..POOL_SIZE] { *b = 0; }

    Ok(())
}

// ── UPDATE_ORACLE (1) ─────────────────────────────────────────────────────────
//
// Accounts:
//   0  pool   (writable)
//   1  admin  (signer) — must match pool.admin
//
// Data (40 bytes after discriminant):
//   [0..8]   oracle_price  u64
//   [8..12]  spread_bps    u32
//   [12..16] vol_adj       u32
//   [16..20] k_param       u32
//   [20..24] fee_bps       u32
//   [24..32] target_a      u64
//   [32..40] target_b      u64

fn update_oracle(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 2 { return Err(ProgramError::NotEnoughAccountKeys); }
    if data.len() < 40    { return Err(ProgramError::InvalidInstructionData); }

    let pool  = &accounts[0];
    let admin = &accounts[1];

    let oracle_price = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let spread_bps   = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let vol_adj      = u32::from_le_bytes(data[12..16].try_into().unwrap());
    let k_param      = u32::from_le_bytes(data[16..20].try_into().unwrap());
    let fee_bps      = u32::from_le_bytes(data[20..24].try_into().unwrap());
    let target_a     = u64::from_le_bytes(data[24..32].try_into().unwrap());
    let target_b     = u64::from_le_bytes(data[32..40].try_into().unwrap());

    // Admin auth check
    {
        let d = pool.try_borrow()?;
        if admin.address().as_ref() != &d[OFF_ADMIN..OFF_ADMIN+32]
            || !admin.is_signer()
        {
            return Err(ProgramError::Custom(1)); // InvalidAdmin
        }
        if spread_bps.saturating_add(vol_adj) > MAX_SPREAD_BPS {
            return Err(ProgramError::Custom(5)); // InvalidSpread
        }
    }

    let clock = Clock::get()?;
    let now = clock.unix_timestamp;

    let mut d = pool.try_borrow_mut()?;

    write_u64(&mut d, OFF_ORACLE_PRICE, oracle_price);
    write_u32(&mut d, OFF_SPREAD_BPS,   spread_bps);
    write_u32(&mut d, OFF_K_PARAM,      k_param);
    write_u32(&mut d, OFF_VOL_ADJ,      vol_adj);
    write_u32(&mut d, OFF_FEE_BPS,      fee_bps);
    write_u64(&mut d, OFF_TARGET_A,     target_a);
    write_u64(&mut d, OFF_TARGET_B,     target_b);
    write_i64(&mut d, OFF_LAST_UPDATE,  now);

    // Write TWAP ring buffer observation
    let cursor = d[OFF_TWAP_CURSOR] as usize;
    let obs_off = OFF_TWAP_OBS + cursor * 16;
    write_u64(&mut d, obs_off,     oracle_price);
    write_i64(&mut d, obs_off + 8, now);
    d[OFF_TWAP_CURSOR] = ((cursor + 1) % TWAP_SLOTS) as u8;

    Ok(())
}

// ── SWAP (2) ──────────────────────────────────────────────────────────────────
//
// Accounts:
//   0  pool      (writable)
//   1  user_in   (writable)
//   2  vault_in  (writable)
//   3  user_out  (writable)
//   4  vault_out (writable)
//   5  user      (signer)
//   6  pool_auth (PDA — authority for vault_out)
//
// Data (17 bytes after discriminant):
//   [0..8]   amount_in  u64
//   [8..16]  min_out    u64
//   [16]     direction  u8  (0 = A→B, 1 = B→A)

fn swap(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 7 { return Err(ProgramError::NotEnoughAccountKeys); }
    if data.len() < 17    { return Err(ProgramError::InvalidInstructionData); }

    let pool      = &accounts[0];
    let user_in   = &accounts[1];
    let vault_in  = &accounts[2];
    let user_out  = &accounts[3];
    let vault_out = &accounts[4];
    let user      = &accounts[5];
    let pool_auth = &accounts[6];

    let amount_in = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let min_out   = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let direction = data[16]; // 0 = A→B, 1 = B→A

    let pool_key_bytes: [u8; 32];
    let reserve_a: u64;
    let reserve_b: u64;
    let target_a:  u64;
    let target_b:  u64;
    let oracle_price: u64;
    let spread_bps:   u32;
    let k_param:      u32;
    let vol_adj:      u32;
    let fee_bps:      u32;
    let last_update:  i64;
    let max_staleness: u32;
    let auth_bump:    u8;

    {
        let d = pool.try_borrow()?;
        pool_key_bytes = pool.address().as_ref().try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        reserve_a    = read_u64(&d, OFF_RESERVE_A);
        reserve_b    = read_u64(&d, OFF_RESERVE_B);
        target_a     = read_u64(&d, OFF_TARGET_A);
        target_b     = read_u64(&d, OFF_TARGET_B);
        oracle_price = read_u64(&d, OFF_ORACLE_PRICE);
        spread_bps   = read_u32(&d, OFF_SPREAD_BPS);
        k_param      = read_u32(&d, OFF_K_PARAM);
        vol_adj      = read_u32(&d, OFF_VOL_ADJ);
        fee_bps      = read_u32(&d, OFF_FEE_BPS);
        last_update  = read_i64(&d, OFF_LAST_UPDATE);
        max_staleness = read_u32(&d, OFF_MAX_STALE);
        auth_bump    = d[OFF_AUTH_BUMP];
    }

    // Staleness check
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    if last_update == i64::MIN
        || now.saturating_sub(last_update) > max_staleness as i64
    {
        return Err(ProgramError::Custom(2)); // StaleOracle
    }

    if oracle_price == 0 {
        return Err(ProgramError::Custom(6)); // ZeroOraclePrice
    }

    if reserve_a == 0 && reserve_b == 0 {
        return Err(ProgramError::Custom(8)); // ZeroReserves
    }

    let effective_spread: u32 = spread_bps + vol_adj;

    // Value-weighted inventory skew
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
    let skew_mag: u32 = ((deviation * deviation / 8000) as u64)
        .min(effective_spread as u64) as u32;

    let (ask_spread, bid_spread) = if deviation >= 0 {
        (effective_spread + skew_mag, effective_spread.saturating_sub(skew_mag))
    } else {
        (effective_spread.saturating_sub(skew_mag), effective_spread + skew_mag)
    };

    // Price impact (symmetric, u128 to prevent overflow)
    let reserve_in: u64 = if direction == 0 { reserve_a } else { reserve_b };
    let impact_bps: u32 = if reserve_in == 0 { 0 } else {
        ((k_param as u128 * amount_in as u128 / reserve_in as u128).min(10_000)) as u32
    };

    let (actual_out, fee_amount, is_a_to_b) = if direction == 0 {
        // A→B
        let ask_price = (oracle_price as u128)
            .saturating_mul(10_000 + ask_spread as u128) / 10_000;
        let ask_impact = ask_price
            .saturating_mul(10_000 + impact_bps as u128) / 10_000;
        if ask_impact == 0 { return Err(ProgramError::ArithmeticOverflow); }
        let out_raw = (amount_in as u128 * 1_000_000_000) / ask_impact;
        let fee = out_raw * fee_bps as u128 / 10_000;
        let actual = out_raw.saturating_sub(fee);
        (actual as u64, fee as u64, true)
    } else {
        // B→A
        let bid_safe = bid_spread.min(10_000);
        let bid_price = (oracle_price as u128)
            .saturating_mul(10_000u128.saturating_sub(bid_safe as u128)) / 10_000;
        let bid_impact = bid_price
            .saturating_mul(10_000u128.saturating_sub(impact_bps as u128)) / 10_000;
        let out_raw = (amount_in as u128 * bid_impact) / 1_000_000_000;
        let fee = out_raw * fee_bps as u128 / 10_000;
        let actual = out_raw.saturating_sub(fee);
        (actual as u64, fee as u64, false)
    };

    if actual_out < min_out {
        return Err(ProgramError::Custom(3)); // SlippageExceeded
    }

    let bump_arr = [auth_bump];
    let signer_seeds = auth_seeds(b"pool_auth", &pool_key_bytes, &bump_arr);
    let signer = Signer::from(&signer_seeds);

    Transfer { from: user_in, to: vault_in, authority: user, amount: amount_in }.invoke()?;
    Transfer { from: vault_out, to: user_out, authority: pool_auth, amount: actual_out }
        .invoke_signed(&[signer])?;

    let mut d = pool.try_borrow_mut()?;
    if is_a_to_b {
        let ra = read_u64(&d, OFF_RESERVE_A);
        let rb = read_u64(&d, OFF_RESERVE_B);
        let fb = read_u64(&d, OFF_FEE_B);
        write_u64(&mut d, OFF_RESERVE_A, ra + amount_in);
        write_u64(&mut d, OFF_RESERVE_B, rb - actual_out);
        write_u64(&mut d, OFF_FEE_B,     fb + fee_amount);
    } else {
        let ra = read_u64(&d, OFF_RESERVE_A);
        let rb = read_u64(&d, OFF_RESERVE_B);
        let fa = read_u64(&d, OFF_FEE_A);
        write_u64(&mut d, OFF_RESERVE_B, rb + amount_in);
        write_u64(&mut d, OFF_RESERVE_A, ra - actual_out);
        write_u64(&mut d, OFF_FEE_A,     fa + fee_amount);
    }

    Ok(())
}

// ── ADD_LIQUIDITY (3) ─────────────────────────────────────────────────────────
//
// Accounts:
//   0  pool            (writable)
//   1  user_a          (writable)
//   2  vault_a         (writable)
//   3  user_b          (writable)
//   4  vault_b         (writable)
//   5  lp_mint         (writable)
//   6  user_lp         (writable)
//   7  dead_lp_account (writable) — SPL token account owned by dead_lp PDA
//   8  user            (signer)
//   9  pool_auth       (PDA signer — mint authority)
//
// Data (24 bytes after discriminant):
//   [0..8]   amount_a   u64
//   [8..16]  amount_b   u64
//   [16..24] min_lp_out u64

fn add_liquidity(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 10 { return Err(ProgramError::NotEnoughAccountKeys); }
    if data.len() < 24     { return Err(ProgramError::InvalidInstructionData); }

    let pool            = &accounts[0];
    let user_a          = &accounts[1];
    let vault_a         = &accounts[2];
    let user_b          = &accounts[3];
    let vault_b         = &accounts[4];
    let lp_mint         = &accounts[5];
    let user_lp         = &accounts[6];
    let dead_lp_account = &accounts[7];
    let user            = &accounts[8];
    let pool_auth       = &accounts[9];

    let amount_a   = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let amount_b   = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let min_lp_out = u64::from_le_bytes(data[16..24].try_into().unwrap());

    if amount_a == 0 || amount_b == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }

    let pool_key_bytes: [u8; 32];
    let reserve_a: u64;
    let reserve_b: u64;
    let lp_supply: u64;
    let auth_bump: u8;

    {
        let d = pool.try_borrow()?;
        pool_key_bytes = pool.address().as_ref().try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        reserve_a = read_u64(&d, OFF_RESERVE_A);
        reserve_b = read_u64(&d, OFF_RESERVE_B);
        lp_supply = read_u64(&d, OFF_LP_SUPPLY);
        auth_bump = d[OFF_AUTH_BUMP];
    }

    // Guard: subsequent deposit requires non-zero reserves
    if lp_supply > 0 && (reserve_a == 0 || reserve_b == 0) {
        return Err(ProgramError::Custom(9)); // InsufficientLiquidity
    }

    // Gross LP
    let gross_lp: u64 = if lp_supply == 0 {
        let product = (amount_a as u128)
            .checked_mul(amount_b as u128)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        isqrt_u128(product)
    } else {
        let share_a = (amount_a as u128 * lp_supply as u128 / reserve_a as u128) as u64;
        let share_b = (amount_b as u128 * lp_supply as u128 / reserve_b as u128) as u64;
        share_a.min(share_b)
    };

    if gross_lp <= MINIMUM_LIQUIDITY {
        return Err(ProgramError::Custom(4)); // DepositTooSmall
    }

    let bump_arr = [auth_bump];
    let signer_seeds = auth_seeds(b"pool_auth", &pool_key_bytes, &bump_arr);
    let signer = Signer::from(&signer_seeds);

    let user_lp_amount = if lp_supply == 0 {
        MintTo {
            mint: lp_mint,
            account: dead_lp_account,
            mint_authority: pool_auth,
            amount: MINIMUM_LIQUIDITY,
        }.invoke_signed(&[signer])?;

        // Re-derive signer (Signer is not Copy)
        let signer2 = Signer::from(&signer_seeds);
        let _ = signer2; // will be used below
        gross_lp - MINIMUM_LIQUIDITY
    } else {
        gross_lp
    };

    if user_lp_amount < min_lp_out {
        return Err(ProgramError::Custom(3)); // SlippageExceeded
    }

    let final_signer = Signer::from(&signer_seeds);

    Transfer { from: user_a, to: vault_a, authority: user, amount: amount_a }.invoke()?;
    Transfer { from: user_b, to: vault_b, authority: user, amount: amount_b }.invoke()?;
    MintTo {
        mint: lp_mint,
        account: user_lp,
        mint_authority: pool_auth,
        amount: user_lp_amount,
    }.invoke_signed(&[final_signer])?;

    let mut d = pool.try_borrow_mut()?;
    write_u64(&mut d, OFF_RESERVE_A, reserve_a + amount_a);
    write_u64(&mut d, OFF_RESERVE_B, reserve_b + amount_b);
    write_u64(&mut d, OFF_LP_SUPPLY, lp_supply + gross_lp);

    Ok(())
}

// ── REMOVE_LIQUIDITY (4) ──────────────────────────────────────────────────────
//
// Accounts:
//   0  pool      (writable)
//   1  vault_a   (writable)
//   2  user_a    (writable)
//   3  vault_b   (writable)
//   4  user_b    (writable)
//   5  lp_mint   (writable)
//   6  user_lp   (writable)
//   7  pool_auth (PDA signer)
//   8  user      (signer — LP token authority)
//
// Data (24 bytes after discriminant):
//   [0..8]   lp_amount  u64
//   [8..16]  min_out_a  u64
//   [16..24] min_out_b  u64

fn remove_liquidity(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 9 { return Err(ProgramError::NotEnoughAccountKeys); }
    if data.len() < 24    { return Err(ProgramError::InvalidInstructionData); }

    let pool      = &accounts[0];
    let vault_a   = &accounts[1];
    let user_a    = &accounts[2];
    let vault_b   = &accounts[3];
    let user_b    = &accounts[4];
    let lp_mint   = &accounts[5];
    let user_lp   = &accounts[6];
    let pool_auth = &accounts[7];
    let user      = &accounts[8];

    let lp_amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let min_out_a = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let min_out_b = u64::from_le_bytes(data[16..24].try_into().unwrap());

    if lp_amount == 0 { return Err(ProgramError::InvalidInstructionData); }

    let pool_key_bytes: [u8; 32];
    let reserve_a: u64;
    let reserve_b: u64;
    let lp_supply: u64;
    let fee_a:     u64;
    let fee_b:     u64;
    let auth_bump: u8;

    {
        let d = pool.try_borrow()?;
        pool_key_bytes = pool.address().as_ref().try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        reserve_a = read_u64(&d, OFF_RESERVE_A);
        reserve_b = read_u64(&d, OFF_RESERVE_B);
        lp_supply = read_u64(&d, OFF_LP_SUPPLY);
        fee_a     = read_u64(&d, OFF_FEE_A);
        fee_b     = read_u64(&d, OFF_FEE_B);
        auth_bump = d[OFF_AUTH_BUMP];
    }

    if lp_supply == 0 || lp_amount > lp_supply {
        return Err(ProgramError::InsufficientFunds);
    }

    let withdrawable_a = reserve_a.saturating_sub(fee_a);
    let withdrawable_b = reserve_b.saturating_sub(fee_b);

    let out_a = (lp_amount as u128 * withdrawable_a as u128 / lp_supply as u128) as u64;
    let out_b = (lp_amount as u128 * withdrawable_b as u128 / lp_supply as u128) as u64;

    if out_a < min_out_a || out_b < min_out_b {
        return Err(ProgramError::Custom(3)); // SlippageExceeded
    }

    let bump_arr = [auth_bump];
    let signer_seeds = auth_seeds(b"pool_auth", &pool_key_bytes, &bump_arr);
    let signer_a = Signer::from(&signer_seeds);
    let signer_b = Signer::from(&signer_seeds);

    Burn     { account: user_lp, mint: lp_mint, authority: user,      amount: lp_amount }.invoke()?;
    Transfer { from: vault_a, to: user_a, authority: pool_auth, amount: out_a }
        .invoke_signed(&[signer_a])?;
    Transfer { from: vault_b, to: user_b, authority: pool_auth, amount: out_b }
        .invoke_signed(&[signer_b])?;

    let mut d = pool.try_borrow_mut()?;
    write_u64(&mut d, OFF_RESERVE_A, reserve_a - out_a);
    write_u64(&mut d, OFF_RESERVE_B, reserve_b - out_b);
    write_u64(&mut d, OFF_LP_SUPPLY, lp_supply - lp_amount);

    Ok(())
}

// ── COLLECT_FEES (5) ──────────────────────────────────────────────────────────
//
// Accounts:
//   0  pool      (writable)
//   1  vault_a   (writable)
//   2  admin_a   (writable)
//   3  vault_b   (writable)
//   4  admin_b   (writable)
//   5  admin     (signer)
//   6  pool_auth (PDA signer)

fn collect_fees(accounts: &[AccountView]) -> ProgramResult {
    if accounts.len() < 7 { return Err(ProgramError::NotEnoughAccountKeys); }

    let pool      = &accounts[0];
    let vault_a   = &accounts[1];
    let admin_a   = &accounts[2];
    let vault_b   = &accounts[3];
    let admin_b   = &accounts[4];
    let admin     = &accounts[5];
    let pool_auth = &accounts[6];

    let pool_key_bytes: [u8; 32];
    let fee_a: u64;
    let fee_b: u64;
    let auth_bump: u8;

    {
        let d = pool.try_borrow()?;
        if admin.address().as_ref() != &d[OFF_ADMIN..OFF_ADMIN+32]
            || !admin.is_signer()
        {
            return Err(ProgramError::Custom(1)); // InvalidAdmin
        }
        pool_key_bytes = pool.address().as_ref().try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        fee_a     = read_u64(&d, OFF_FEE_A);
        fee_b     = read_u64(&d, OFF_FEE_B);
        auth_bump = d[OFF_AUTH_BUMP];
    }

    let bump_arr = [auth_bump];
    let signer_seeds = auth_seeds(b"pool_auth", &pool_key_bytes, &bump_arr);

    if fee_a > 0 {
        let signer = Signer::from(&signer_seeds);
        Transfer { from: vault_a, to: admin_a, authority: pool_auth, amount: fee_a }
            .invoke_signed(&[signer])?;
    }
    if fee_b > 0 {
        let signer = Signer::from(&signer_seeds);
        Transfer { from: vault_b, to: admin_b, authority: pool_auth, amount: fee_b }
            .invoke_signed(&[signer])?;
    }

    let mut d = pool.try_borrow_mut()?;
    write_u64(&mut d, OFF_FEE_A, 0);
    write_u64(&mut d, OFF_FEE_B, 0);

    Ok(())
}

// ── SET_ADMIN (6) ─────────────────────────────────────────────────────────────
//
// Accounts:
//   0  pool          (writable)
//   1  current_admin (signer)
//
// Data (32 bytes):
//   [0..32] new_admin_pubkey

fn set_admin(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    if accounts.len() < 2 { return Err(ProgramError::NotEnoughAccountKeys); }
    if data.len() < 32    { return Err(ProgramError::InvalidInstructionData); }

    let pool          = &accounts[0];
    let current_admin = &accounts[1];
    let new_admin: [u8;32] = data[0..32].try_into().unwrap();

    let mut d = pool.try_borrow_mut()?;
    if current_admin.address().as_ref() != &d[OFF_ADMIN..OFF_ADMIN+32]
        || !current_admin.is_signer()
    {
        return Err(ProgramError::Custom(1)); // InvalidAdmin
    }

    d[OFF_ADMIN..OFF_ADMIN+32].copy_from_slice(&new_admin);
    Ok(())
}
