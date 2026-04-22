use pinocchio::{account_info::AccountInfo, entrypoint, pubkey::Pubkey, ProgramResult};
use prop_amm_submission_sdk::{set_return_data_bytes, set_return_data_u64, set_storage};

const NAME: &str = "EWMA Dynamic Fee v2";
const MODEL_USED: &str = "Claude Sonnet 4.6";
const STORAGE_SIZE: usize = 1024;

// ── State layout (first 40 bytes of storage) ──────────────────────────────────
// [0..8]   ewma_vol      : EWMA of |Δprice/price|, scaled 1e9
// [8..16]  last_rx       : reserve_x saved after previous swap
// [16..24] last_ry       : reserve_y saved after previous swap
// [24..32] shock_steps   : countdown after large price move (max SHOCK_DECAY_STEPS)
// [32..40] direction_ema : EWMA of trade direction (1e9=full-buy, 0=full-sell)
//
// ── Compute-swap instruction layout (tags 0 / 1) ──────────────────────────────
// [0]: side  [1..9]: input  [9..17]: rx  [17..25]: ry  [25..1049]: storage
//
// ── After-swap instruction layout (tag 2) ─────────────────────────────────────
// [0]: tag  [1]: side  [2..10]: input  [10..18]: output
// [18..26]: post_rx   [26..34]: post_ry  [34..42]: step  [42..1066]: storage
//
// ── Fee formula ───────────────────────────────────────────────────────────────
// fee = BASE + vol_fee + shock_fee + momentum_fee  (capped at 100 bps)
//
// vol_fee      = ewma_vol * 2
// shock_fee    = shock_steps * 4 bps  (max 32 bps when shock is fresh)
// momentum_fee = 25 bps on arb-favored side when ema > 0.70 or < 0.30
//
// At calm (vol ≈ 0.1%, no shock, no momentum):  8 + 2×1 ≈ 28 bps
//   → stays below normalizer's 30 bps floor → attracts retail
// After 0.5%+ price shock:  8 + 6 + 32 = 46 bps for 8 steps
//   → fee stays elevated while arbs continue attacking same move
// Sustained directional arb flow:  +25 bps on arb-favored side only
//   → taxes arbs, doesn't raise counter-direction (retail) fees

const BASE_FEE_1E9: u64         = 800_000;     //  8 bps
const VOL_MULT: u64             = 2;
const MAX_FEE_1E9: u64          = 10_000_000;  // 100 bps hard cap

const ALPHA_1E9: u128           = 200_000_000; // vol EMA α=0.20
const ONE_M_ALPHA_1E9: u128     = 800_000_000;

const SHOCK_THRESHOLD_1E9: u64  = 5_000_000;   // 0.5% move triggers shock
const SHOCK_DECAY_STEPS: u64    = 8;
const SHOCK_FEE_PER_STEP: u64   = 400_000;     // 4 bps × remaining steps (max 32 bps)

const DIR_ALPHA_1E9: u128       = 300_000_000; // direction EMA α=0.30
const DIR_ONE_M_ALPHA: u128     = 700_000_000;
const DIR_HIGH: u64             = 700_000_000; // bullish arb threshold
const DIR_LOW: u64              = 300_000_000; // bearish arb threshold
const MOMENTUM_FEE_1E9: u64     = 2_500_000;   // 25 bps on arb-favored side

#[derive(wincode::SchemaRead)]
struct ComputeSwapInstruction {
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
        2 => handle_after_swap(instruction_data),
        3 => set_return_data_bytes(NAME.as_bytes()),
        4 => set_return_data_bytes(MODEL_USED.as_bytes()),
        _ => {}
    }

    Ok(())
}

// ── BPF after-swap handler ────────────────────────────────────────────────────
// Copies the storage snapshot from instruction_data[42..], runs after_swap,
// then calls set_storage (custom BPF syscall) to persist the updated state.
fn handle_after_swap(instruction_data: &[u8]) {
    if instruction_data.len() < 42 + STORAGE_SIZE {
        return;
    }
    let mut storage = [0u8; STORAGE_SIZE];
    storage.copy_from_slice(&instruction_data[42..42 + STORAGE_SIZE]);
    after_swap(instruction_data, &mut storage);
    let _ = set_storage(&storage);
}

// ── Compute swap: quoted output given current storage state ───────────────────

pub fn compute_swap(data: &[u8]) -> u64 {
    let d: ComputeSwapInstruction = match wincode::deserialize(data) {
        Ok(x) => x,
        Err(_) => return 0,
    };

    let input = d.input_amount as u128;
    let rx    = d.reserve_x as u128;
    let ry    = d.reserve_y as u128;
    if rx == 0 || ry == 0 || input == 0 { return 0; }

    let ewma_vol      = read_u64(&d.storage, 0);
    let last_rx       = read_u64(&d.storage, 8);
    // Clamp to valid ranges to be robust against uninitialized storage bytes
    let shock_steps   = read_u64(&d.storage, 24).min(SHOCK_DECAY_STEPS);
    let direction_ema = read_u64(&d.storage, 32).min(1_000_000_000);

    let vol_fee   = ewma_vol.saturating_mul(VOL_MULT);
    let shock_fee = shock_steps.saturating_mul(SHOCK_FEE_PER_STEP);

    // Momentum surcharge only after first trade (last_rx > 0 = we have history)
    // Bullish arb (ema > 0.70) repeatedly buys X (side=0) → surcharge side=0
    // Bearish arb (ema < 0.30) repeatedly sells X (side=1) → surcharge side=1
    let momentum_fee = if last_rx > 0 {
        if direction_ema > DIR_HIGH && d.side == 0 { MOMENTUM_FEE_1E9 }
        else if direction_ema < DIR_LOW && d.side == 1 { MOMENTUM_FEE_1E9 }
        else { 0 }
    } else { 0 };

    let fee_1e9 = BASE_FEE_1E9
        .saturating_add(vol_fee)
        .saturating_add(shock_fee)
        .saturating_add(momentum_fee)
        .min(MAX_FEE_1E9) as u128;

    let keep = 1_000_000_000u128 - fee_1e9;
    let k    = rx * ry;
    let net  = input * keep / 1_000_000_000;

    match d.side {
        0 => {
            let new_ry = ry + net;
            let new_rx = ceil_div(k, new_ry);
            rx.saturating_sub(new_rx) as u64
        }
        1 => {
            let new_rx = rx + net;
            let new_ry = ceil_div(k, new_rx);
            ry.saturating_sub(new_ry) as u64
        }
        _ => 0,
    }
}

// ── After-swap: update storage with post-trade state ─────────────────────────
// Native ABI: called directly by the test harness with live mutable storage.
// data = encoded after_swap instruction (post_rx at [18..26], post_ry at [26..34])
// storage = live 1024-byte storage; reads old state from here, writes new state here.
pub fn after_swap(data: &[u8], storage: &mut [u8]) {
    if data.len() < 34 || storage.len() < 8 { return; }

    let cur_rx = read_u64_raw(data, 18);
    let cur_ry = read_u64_raw(data, 26);

    let old_vol       = if storage.len() >= 8  { read_u64(storage, 0) } else { 0 };
    let last_rx       = if storage.len() >= 16 { read_u64(storage, 8) } else { 0 };
    let last_ry       = if storage.len() >= 24 { read_u64(storage, 16) } else { 0 };
    let shock_steps   = if storage.len() >= 32 { read_u64(storage, 24).min(SHOCK_DECAY_STEPS) } else { 0 };
    let direction_ema = if storage.len() >= 40 { read_u64(storage, 32).min(1_000_000_000) } else { 500_000_000 };

    // Relative price change |Δ(ry/rx)| / (ry/rx) via cross-multiplication
    // saturating_mul prevents u128 overflow with extreme reserve values
    let price_change_1e9: u64 = if last_rx > 0 && last_ry > 0 && cur_rx > 0 {
        let cr = cur_rx as u128;
        let cy = cur_ry as u128;
        let lr = last_rx as u128;
        let ly = last_ry as u128;
        let cross_new = cy.saturating_mul(lr);
        let cross_old = ly.saturating_mul(cr);
        let diff  = if cross_new > cross_old { cross_new - cross_old }
                    else                     { cross_old - cross_new };
        let denom = ly.saturating_mul(cr);
        if denom > 0 {
            diff.saturating_mul(1_000_000_000)
                .checked_div(denom)
                .unwrap_or(u128::MAX)
                .min(u64::MAX as u128) as u64
        } else { 0 }
    } else { 0 };

    // EWMA vol
    let new_vol = ((ALPHA_1E9       * price_change_1e9 as u128
                  + ONE_M_ALPHA_1E9 * old_vol as u128)
                  / 1_000_000_000) as u64;

    // Shock countdown: reset on large move, decay by 1 otherwise
    let new_shock = if price_change_1e9 >= SHOCK_THRESHOLD_1E9 {
        SHOCK_DECAY_STEPS
    } else {
        shock_steps.saturating_sub(1)
    };

    // Direction EMA: infer trade direction from whether X decreased (buy) or increased (sell)
    let new_dir = if last_rx > 0 {
        let dir_signal: u128 = if (cur_rx as u128) < last_rx as u128 {
            1_000_000_000  // X bought → bullish arb
        } else if (cur_rx as u128) > last_rx as u128 {
            0              // X sold → bearish arb
        } else {
            500_000_000    // no change → neutral
        };
        ((DIR_ALPHA_1E9  * dir_signal
        + DIR_ONE_M_ALPHA * direction_ema as u128)
        / 1_000_000_000) as u64
    } else {
        500_000_000  // first trade: seed neutral to avoid spurious surcharge
    };

    if storage.len() >= 8  { write_u64(storage, 0,  new_vol); }
    if storage.len() >= 16 { write_u64(storage, 8,  cur_rx); }
    if storage.len() >= 24 { write_u64(storage, 16, cur_ry); }
    if storage.len() >= 32 { write_u64(storage, 24, new_shock); }
    if storage.len() >= 40 { write_u64(storage, 32, new_dir); }
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
fn read_u64_raw(b: &[u8], off: usize) -> u64 {
    read_u64(b, off)
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
        after_swap,
    );
}
