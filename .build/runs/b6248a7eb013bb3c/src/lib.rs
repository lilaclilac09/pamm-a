use pinocchio::{account_info::AccountInfo, entrypoint, pubkey::Pubkey, ProgramResult};
use prop_amm_submission_sdk::{set_return_data_bytes, set_return_data_u64};

const NAME: &str = "EWMA Dynamic Fee v1";
const MODEL_USED: &str = "Claude Sonnet 4.6";
const STORAGE_SIZE: usize = 1024;

const BASE_FEE_1E9: u64  = 800_000;
const VOL_MULT: u64       = 2;
const MAX_FEE_1E9: u64   = 10_000_000;
const ALPHA_1E9: u128     = 200_000_000;
const ONE_M_ALPHA_1E9: u128 = 800_000_000;

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
    if instruction_data.is_empty() { return Ok(()); }
    match instruction_data[0] {
        0 | 1 => set_return_data_u64(compute_swap(instruction_data)),
        2 => {}
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
    let ewma_vol = u64::from_le_bytes(d.storage[0..8].try_into().unwrap());
    let fee_1e9  = (BASE_FEE_1E9 + ewma_vol * VOL_MULT).min(MAX_FEE_1E9) as u128;
    let keep     = 1_000_000_000u128 - fee_1e9;
    let k = rx * ry;
    let net = input * keep / 1_000_000_000;
    match d.side {
        0 => { let new_ry = ry + net; let new_rx = (k + new_ry - 1) / new_ry; rx.saturating_sub(new_rx) as u64 }
        1 => { let new_rx = rx + net; let new_ry = (k + new_rx - 1) / new_rx; ry.saturating_sub(new_ry) as u64 }
        _ => 0,
    }
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
