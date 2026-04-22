use pinocchio::{account_info::AccountInfo, entrypoint, pubkey::Pubkey, ProgramResult};
use prop_amm_submission_sdk::{set_return_data_bytes, set_return_data_u64, set_storage};

const NAME: &str = "EWMA Dynamic Fee v2";
const MODEL_USED: &str = "Claude Sonnet 4.6";
const STORAGE_SIZE: usize = 1024;

// ── State layout (first 32 bytes of storage) ──────────────────────────────────
// [0..8]   ewma_vol    : EWMA of |Δprice/price|, scaled 1e9
// [8..16]  last_rx     : reserve_x saved after previous swap
// [16..24] last_ry     : reserve_y saved after previous swap
// [24..32] shock_steps : countdown after large price move (max SHOCK_DECAY_STEPS)
//
// ── Compute-swap instruction layout (tags 0 / 1) ──────────────────────────────
// [0]: side  [1..9]: input  [9..17]: rx  [17..25]: ry  [25..1049]: storage
//
// ── After-swap instruction layout (tag 2) ─────────────────────────────────────
// [0]: tag  [1]: side  [2..10]: input  [10..18]: output
// [18..26]: post_rx   [26..34]: post_ry  [34..42]: step  [42..1066]: storage
//
// ── Fee formula ───────────────────────────────────────────────────────────────
// fee = BASE + vol_fee + shock_fee  (capped at 100 bps)
//
// vol_fee   = ewma_vol * 2
// shock_fee = shock_steps * 4 bps  (max 32 bps when fresh shock, decays each trade)
//
// At calm (vol ≈ 0.1%, no shock):   8 + 2 ≈ 28 bps
//   → stays below normalizer's 30 bps floor → attracts retail
// After a 0.5%+ price shock:        8 + 6 + 32 = 46 bps for 8 steps
//   → fee stays elevated while arbs continue attacking same move
// At volatile (vol ≈ 0.3%):         8 + 6 = ~14 bps vol + shock decay

const BASE_FEE_1E9: u64        = 800_000;     //  8 bps
const VOL_MULT: u64            = 2;
const MAX_FEE_1E9: u64         = 10_000_000;  // 100 bps hard cap

const ALPHA_1E9: u128          = 200_000_000; // vol EMA α=0.20
const ONE_M_ALPHA_1E9: u128    = 800_000_000;

const SHOCK_THRESHOLD_1E9: u64 = 5_000_000;   // 0.5% price move triggers shock
const SHOCK_DECAY_STEPS: u64   = 8;
const SHOCK_FEE_PER_STEP: u64  = 400_000;     // 4 bps × remaining steps (max 32 bps)

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

// BPF path: copy storage snapshot from instruction data, run after_swap, persist via set_storage.
fn handle_after_swap(d: &[u8]) {
    if d.len() < 42 + STORAGE_SIZE { return; }
    let mut storage = [0u8; STORAGE_SIZE];
    storage.copy_from_slice(&d[42..42 + STORAGE_SIZE]);
    after_swap(d, &mut storage);
    let _ = set_storage(&storage);
}

// ── Compute swap ──────────────────────────────────────────────────────────────

pub fn compute_swap(data: &[u8]) -> u64 {
    let d: ComputeSwapInstruction = match wincode::deserialize(data) {
        Ok(x) => x,
        Err(_) => return 0,
    };

    let input = d.input_amount as u128;
    let rx    = d.reserve_x as u128;
    let ry    = d.reserve_y as u128;
    if rx == 0 || ry == 0 || input == 0 { return 0; }

    let ewma_vol    = read_u64(&d.storage, 0);
    let shock_steps = read_u64(&d.storage, 24).min(SHOCK_DECAY_STEPS);

    let vol_fee   = ewma_vol.saturating_mul(VOL_MULT);
    let shock_fee = shock_steps.saturating_mul(SHOCK_FEE_PER_STEP);

    let fee_1e9 = BASE_FEE_1E9
        .saturating_add(vol_fee)
        .saturating_add(shock_fee)
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

// ── After-swap: update vol EWMA and shock countdown ───────────────────────────
// Native ABI: called directly by the test harness with live mutable storage.
// data = encoded after_swap instruction (post_rx at [18..26], post_ry at [26..34])
pub fn after_swap(data: &[u8], storage: &mut [u8]) {
    if data.len() < 34 || storage.len() < 8 { return; }

    let cur_rx = read_u64_raw(data, 18) as u128;
    let cur_ry = read_u64_raw(data, 26) as u128;

    let old_vol     = read_u64(storage, 0);
    let last_rx     = if storage.len() >= 16 { read_u64(storage, 8) as u128 } else { 0 };
    let last_ry     = if storage.len() >= 24 { read_u64(storage, 16) as u128 } else { 0 };
    let shock_steps = if storage.len() >= 32 { read_u64(storage, 24).min(SHOCK_DECAY_STEPS) } else { 0 };

    // Relative price change |Δ(ry/rx)| / (ry/rx) via cross-multiplication
    let price_change_1e9: u64 = if last_rx > 0 && last_ry > 0 && cur_rx > 0 {
        let cross_new = cur_ry.saturating_mul(last_rx);
        let cross_old = last_ry.saturating_mul(cur_rx);
        let diff  = if cross_new > cross_old { cross_new - cross_old }
                    else                     { cross_old - cross_new };
        let denom = last_ry.saturating_mul(cur_rx);
        if denom > 0 {
            diff.saturating_mul(1_000_000_000)
                .checked_div(denom)
                .unwrap_or(u128::MAX)
                .min(u64::MAX as u128) as u64
        } else { 0 }
    } else { 0 };

    // EWMA vol: α=0.20, converges to steady-state vol within ~15 steps
    let new_vol = ((ALPHA_1E9       * price_change_1e9 as u128
                  + ONE_M_ALPHA_1E9 * old_vol as u128)
                  / 1_000_000_000) as u64;

    // Shock: reset countdown on large move, else decay by 1
    let new_shock = if price_change_1e9 >= SHOCK_THRESHOLD_1E9 {
        SHOCK_DECAY_STEPS
    } else {
        shock_steps.saturating_sub(1)
    };

    write_u64(storage, 0,  new_vol);
    write_u64(storage, 8,  cur_rx as u64);
    write_u64(storage, 16, cur_ry as u64);
    write_u64(storage, 24, new_shock);
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
fn read_u64_raw(b: &[u8], off: usize) -> u64 { read_u64(b, off) }

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
