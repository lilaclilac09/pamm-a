use pinocchio::{account_info::AccountInfo, entrypoint, pubkey::Pubkey, ProgramResult};
use prop_amm_submission_sdk::{set_return_data_bytes, set_return_data_u64};

const NAME: &str = "EWMA Dynamic Fee v2";
const MODEL_USED: &str = "Claude Sonnet 4.6";
const STORAGE_SIZE: usize = 1024;

// ── Storage layout (40 bytes used out of 1024) ────────────────────────────────
// [0..8]   ewma_vol      : EWMA of |Δprice/price|, scaled 1e9
//                          e.g. 1_000_000 = 0.001 = 0.1% per step
// [8..16]  last_rx       : reserve_x from previous after_swap
// [16..24] last_ry       : reserve_y from previous after_swap
// [24..32] shock_steps   : countdown after large price move (0 = no shock)
// [32..40] direction_ema : EWMA of trade direction, 1e9 = full-buy, 0 = full-sell

// ── Fee formula ───────────────────────────────────────────────────────────────
// fee = BASE + vol_fee + shock_fee + momentum_fee   (capped at 100 bps)
//
// vol_fee      = ewma_vol * 2
// shock_fee    = shock_steps * 4 bps  (decays 1 step/trade, max 32 bps at full shock)
// momentum_fee = 25 bps on the arb-favored side when direction_ema > 0.70 or < 0.30
//
// At calm (vol ≈ 0.1%, no shock, no momentum):
//   8 + 2×1 = ~28 bps  → below 30 bps normaliser floor → attracts retail
// After shock (vol 0.3%, fresh shock):
//   8 + 6 + 32 = 46 bps → protects for 8 steps
// Arb-detected side during sustained trend:
//   +25 bps on top → taxes directed flow

const BASE_FEE_1E9: u64       = 800_000;     //  8 bps
const VOL_MULT: u64           = 2;
const MAX_FEE_1E9: u64        = 10_000_000;  // 100 bps

// Vol EWMA
const ALPHA_1E9: u128         = 200_000_000; // 0.20
const ONE_M_ALPHA_1E9: u128   = 800_000_000; // 0.80

// Shock: price move ≥ 0.5% triggers SHOCK_DECAY_STEPS countdown
// Each remaining step adds SHOCK_FEE_PER_STEP to fee
const SHOCK_THRESHOLD_1E9: u64 = 5_000_000; // 0.5% price move
const SHOCK_DECAY_STEPS: u64   = 8;
const SHOCK_FEE_PER_STEP: u64  = 400_000;   // 4 bps × steps remaining (max 32 bps)

// Directional momentum: EWMA of inferred trade direction
// When ema > DIR_HIGH: sustained buying = bullish arb → surcharge on side=0 (buy X)
// When ema < DIR_LOW:  sustained selling = bearish arb → surcharge on side=1 (sell X)
const DIR_ALPHA_1E9: u128         = 300_000_000; // 0.30 — reacts within ~3 trades
const DIR_ONE_M_ALPHA_1E9: u128   = 700_000_000;
const DIR_HIGH: u64               = 700_000_000; // 0.70 — bullish arb threshold
const DIR_LOW: u64                = 300_000_000; // 0.30 — bearish arb threshold
const MOMENTUM_SURCHARGE_1E9: u64 = 2_500_000;  // 25 bps

// ── Instruction struct (same layout for tags 0, 1, 2) ─────────────────────────
// byte 0        : tag / side  (0 = pay-Y/get-X, 1 = pay-X/get-Y, 2 = after_swap)
// bytes [1..9]  : input_amount u64 LE
// bytes [9..17] : reserve_x   u64 LE   (post-trade for tag 2)
// bytes [17..25]: reserve_y   u64 LE   (post-trade for tag 2)
// bytes [25..]  : storage     [u8; 1024]
#[derive(wincode::SchemaRead)]
struct Instruction {
    side:         u8,
    input_amount: u64,
    reserve_x:    u64,
    reserve_y:    u64,
    storage:      [u8; STORAGE_SIZE],
}

#[cfg(not(feature = "no-entrypoint"))]
entrypoint!(process_instruction);

pub fn process_instruction(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.is_empty() {
        return Ok(());
    }

    match instruction_data[0] {
        0 | 1 => set_return_data_u64(compute_swap(instruction_data)),
        2 => {
            if let Ok(d) = wincode::deserialize::<Instruction>(instruction_data) {
                set_return_data_bytes(&update_storage(&d));
            }
        }
        3 => set_return_data_bytes(NAME.as_bytes()),
        4 => set_return_data_bytes(MODEL_USED.as_bytes()),
        _ => {}
    }

    Ok(())
}

pub fn compute_swap(data: &[u8]) -> u64 {
    let d: Instruction = match wincode::deserialize(data) {
        Ok(x) => x,
        Err(_) => return 0,
    };

    let input = d.input_amount as u128;
    let rx    = d.reserve_x as u128;
    let ry    = d.reserve_y as u128;
    if rx == 0 || ry == 0 || input == 0 { return 0; }

    let ewma_vol      = read_u64(&d.storage, 0);
    let last_rx       = read_u64(&d.storage, 8);
    let shock_steps   = read_u64(&d.storage, 24);
    let direction_ema = read_u64(&d.storage, 32);

    // Vol component (existing)
    let vol_fee = ewma_vol.saturating_mul(VOL_MULT);

    // Shock component: each remaining countdown step adds 4 bps
    let shock_fee = shock_steps.saturating_mul(SHOCK_FEE_PER_STEP);

    // Momentum component: only when we have prior history (last_rx > 0)
    // Bullish arb (dir_ema > 0.70) buys X via side=0 → surcharge side=0
    // Bearish arb (dir_ema < 0.30) sells X via side=1 → surcharge side=1
    let momentum_fee = if last_rx > 0 {
        if direction_ema > DIR_HIGH && d.side == 0 {
            MOMENTUM_SURCHARGE_1E9
        } else if direction_ema < DIR_LOW && d.side == 1 {
            MOMENTUM_SURCHARGE_1E9
        } else {
            0
        }
    } else {
        0
    };

    let fee_1e9 = (BASE_FEE_1E9 + vol_fee + shock_fee + momentum_fee).min(MAX_FEE_1E9) as u128;
    let keep    = 1_000_000_000u128 - fee_1e9;
    let k       = rx * ry;
    let net     = input * keep / 1_000_000_000;

    match d.side {
        0 => {
            // Trader pays Y → receives X
            let new_ry = ry + net;
            let new_rx = ceil_div(k, new_ry);
            rx.saturating_sub(new_rx) as u64
        }
        1 => {
            // Trader pays X → receives Y
            let new_rx = rx + net;
            let new_ry = ceil_div(k, new_rx);
            ry.saturating_sub(new_ry) as u64
        }
        _ => 0,
    }
}

// ── After-swap: update vol EWMA, shock countdown, direction EMA ───────────────
fn update_storage(d: &Instruction) -> [u8; STORAGE_SIZE] {
    let mut storage = d.storage;

    let old_vol       = read_u64(&storage, 0);
    let last_rx       = read_u64(&storage, 8);
    let last_ry       = read_u64(&storage, 16);
    let shock_steps   = read_u64(&storage, 24);
    let direction_ema = read_u64(&storage, 32);

    let cur_rx = d.reserve_x as u128;
    let cur_ry = d.reserve_y as u128;

    // |Δ(ry/rx)| / (ry/rx) = |cur_ry*last_rx - last_ry*cur_rx| / (last_ry*cur_rx)
    let price_change_1e9: u64 = if last_rx > 0 && last_ry > 0 && cur_rx > 0 {
        let old_rx    = last_rx as u128;
        let old_ry    = last_ry as u128;
        let cross_new = cur_ry * old_rx;
        let cross_old = old_ry * cur_rx;
        let diff  = if cross_new > cross_old { cross_new - cross_old }
                    else                     { cross_old - cross_new };
        let denom = old_ry * cur_rx;
        if denom > 0 {
            (diff * 1_000_000_000 / denom).min(u64::MAX as u128) as u64
        } else { 0 }
    } else { 0 };

    // Update EWMA vol (existing)
    let new_vol = ((ALPHA_1E9       * price_change_1e9 as u128
                  + ONE_M_ALPHA_1E9 * old_vol as u128)
                  / 1_000_000_000) as u64;

    // Shock: reset to SHOCK_DECAY_STEPS on large move, else count down
    let new_shock = if price_change_1e9 >= SHOCK_THRESHOLD_1E9 {
        SHOCK_DECAY_STEPS
    } else {
        shock_steps.saturating_sub(1)
    };

    // Direction: infer from whether X increased or decreased this trade
    // cur_rx < last_rx → X was bought (side=0, bullish) → signal = 1e9
    // cur_rx > last_rx → X was sold   (side=1, bearish) → signal = 0
    // cur_rx == last_rx (edge case)                      → signal = 5e8 (neutral)
    let new_dir = if last_rx > 0 {
        let dir_signal: u128 = if cur_rx < last_rx as u128 {
            1_000_000_000
        } else if cur_rx > last_rx as u128 {
            0
        } else {
            500_000_000
        };
        ((DIR_ALPHA_1E9       * dir_signal
        + DIR_ONE_M_ALPHA_1E9 * direction_ema as u128)
        / 1_000_000_000) as u64
    } else {
        // First trade ever — initialize to neutral so momentum doesn't fire early
        500_000_000
    };

    write_u64(&mut storage, 0,  new_vol);
    write_u64(&mut storage, 8,  d.reserve_x);
    write_u64(&mut storage, 16, d.reserve_y);
    write_u64(&mut storage, 24, new_shock);
    write_u64(&mut storage, 32, new_dir);

    storage
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[inline]
fn ceil_div(a: u128, b: u128) -> u128 { (a + b - 1) / b }

#[inline]
fn read_u64(b: &[u8], off: usize) -> u64 {
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(arr)
}

#[inline]
fn write_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

pub fn get_model_used() -> &'static str { MODEL_USED }


#[cfg(not(target_os = "solana"))]
#[inline]
fn __prop_amm_after_swap_noop(_data: &[u8], _storage: &mut [u8]) {}

#[cfg(not(target_os = "solana"))]
#[no_mangle]
pub extern "C" fn __prop_amm_compute_swap_export(data: *const u8, len: usize) -> u64 {
    prop_amm_submission_sdk::ffi_compute_swap(data, len, compute_swap)
}

#[cfg(not(target_os = "solana"))]
#[no_mangle]
pub extern "C" fn __prop_amm_after_swap_export(
    data: *const u8,
    data_len: usize,
    storage: *mut u8,
    storage_len: usize,
) {
    prop_amm_submission_sdk::ffi_after_swap(
        data,
        data_len,
        storage,
        storage_len,
        __prop_amm_after_swap_noop,
    );
}
