use pinocchio::{account_info::AccountInfo, entrypoint, pubkey::Pubkey, ProgramResult};
use prop_amm_submission_sdk::{set_return_data_bytes, set_return_data_u64, set_storage};

const NAME: &str = "EWMA v2 no momentum";
const MODEL_USED: &str = "Claude Sonnet 4.6";
const STORAGE_SIZE: usize = 1024;

const BASE_FEE_1E9: u64 = 800_000;
const VOL_MULT: u64 = 2;
const MAX_FEE_1E9: u64 = 10_000_000;
const ALPHA_1E9: u128 = 200_000_000;
const ONE_M_ALPHA_1E9: u128 = 800_000_000;
const SHOCK_THRESHOLD_1E9: u64 = 5_000_000;
const SHOCK_DECAY_STEPS: u64 = 8;
const SHOCK_FEE_PER_STEP: u64 = 400_000;

#[derive(wincode::SchemaRead)]
struct ComputeSwapInstruction {
    side: u8, input_amount: u64, reserve_x: u64, reserve_y: u64,
    storage: [u8; STORAGE_SIZE],
}

#[cfg(not(feature = "no-entrypoint"))]
entrypoint!(process_instruction);

pub fn process_instruction(_program_id: &Pubkey, _accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    if instruction_data.is_empty() { return Ok(()); }
    match instruction_data[0] {
        0 | 1 => set_return_data_u64(compute_swap(instruction_data)),
        2 => handle_after_swap(instruction_data),
        3 => set_return_data_bytes(NAME.as_bytes()),
        4 => set_return_data_bytes(MODEL_USED.as_bytes()),
        _ => {}
    }
    Ok(())
}

fn handle_after_swap(d: &[u8]) {
    if d.len() < 42 + STORAGE_SIZE { return; }
    let mut storage = [0u8; STORAGE_SIZE];
    storage.copy_from_slice(&d[42..42 + STORAGE_SIZE]);
    after_swap(d, &mut storage);
    let _ = set_storage(&storage);
}

pub fn compute_swap(data: &[u8]) -> u64 {
    let d: ComputeSwapInstruction = match wincode::deserialize(data) { Ok(x) => x, Err(_) => return 0 };
    let input = d.input_amount as u128;
    let rx = d.reserve_x as u128;
    let ry = d.reserve_y as u128;
    if rx == 0 || ry == 0 || input == 0 { return 0; }
    let ewma_vol = u64::from_le_bytes(d.storage[0..8].try_into().unwrap());
    let shock_steps = u64::from_le_bytes(d.storage[24..32].try_into().unwrap()).min(SHOCK_DECAY_STEPS);
    let vol_fee = ewma_vol.saturating_mul(VOL_MULT);
    let shock_fee = shock_steps.saturating_mul(SHOCK_FEE_PER_STEP);
    let fee_1e9 = BASE_FEE_1E9.saturating_add(vol_fee).saturating_add(shock_fee).min(MAX_FEE_1E9) as u128;
    let keep = 1_000_000_000u128 - fee_1e9;
    let k = rx * ry;
    let net = input * keep / 1_000_000_000;
    match d.side {
        0 => { let nr = ry + net; let nx = (k + nr - 1) / nr; rx.saturating_sub(nx) as u64 }
        1 => { let nx = rx + net; let ny = (k + nx - 1) / nx; ry.saturating_sub(ny) as u64 }
        _ => 0,
    }
}

pub fn after_swap(data: &[u8], storage: &mut [u8]) {
    if data.len() < 34 || storage.len() < 8 { return; }
    let cur_rx = u64::from_le_bytes(data[18..26].try_into().unwrap()) as u128;
    let cur_ry = u64::from_le_bytes(data[26..34].try_into().unwrap()) as u128;
    let old_vol = u64::from_le_bytes(storage[0..8].try_into().unwrap());
    let last_rx = u64::from_le_bytes(storage[8..16].try_into().unwrap());
    let last_ry = u64::from_le_bytes(storage[16..24].try_into().unwrap());
    let shock_steps = u64::from_le_bytes(storage[24..32].try_into().unwrap()).min(SHOCK_DECAY_STEPS);
    let price_change_1e9: u64 = if last_rx > 0 && last_ry > 0 && cur_rx > 0 {
        let cn = cur_ry.saturating_mul(last_rx as u128);
        let co = (last_ry as u128).saturating_mul(cur_rx);
        let diff = if cn > co { cn - co } else { co - cn };
        let denom = (last_ry as u128).saturating_mul(cur_rx);
        if denom > 0 { diff.saturating_mul(1_000_000_000).checked_div(denom).unwrap_or(u128::MAX).min(u64::MAX as u128) as u64 } else { 0 }
    } else { 0 };
    let new_vol = ((ALPHA_1E9 * price_change_1e9 as u128 + ONE_M_ALPHA_1E9 * old_vol as u128) / 1_000_000_000) as u64;
    let new_shock = if price_change_1e9 >= SHOCK_THRESHOLD_1E9 { SHOCK_DECAY_STEPS } else { shock_steps.saturating_sub(1) };
    storage[0..8].copy_from_slice(&new_vol.to_le_bytes());
    storage[8..16].copy_from_slice(&(cur_rx as u64).to_le_bytes());
    storage[16..24].copy_from_slice(&(cur_ry as u64).to_le_bytes());
    storage[24..32].copy_from_slice(&new_shock.to_le_bytes());
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
