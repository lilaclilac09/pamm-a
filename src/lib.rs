use pinocchio::{account_info::AccountInfo, entrypoint, pubkey::Pubkey, ProgramResult};
use prop_amm_submission_sdk::{set_return_data_bytes, set_return_data_u64};

const NAME: &str = "EWMA Dynamic Fee";
const MODEL_USED: &str = "Claude Sonnet 4.6";
const STORAGE_SIZE: usize = 1024;

// ── Storage layout (24 bytes used out of 1024) ────────────────────────────────
// [0..8]   ewma_vol  : u64 LE — EWMA of |Δprice|/price, scaled 1e9
//                               e.g. 1_000_000 = 0.001 = 0.1% per step
// [8..16]  last_rx   : u64 LE — reserve_x from previous after_swap
// [16..24] last_ry   : u64 LE — reserve_y from previous after_swap

// ── Fee formula ───────────────────────────────────────────────────────────────
// fee_1e9 = BASE_FEE_1E9 + ewma_vol * VOL_MULT  (capped at MAX_FEE_1E9)
//
// At calm market  vol ≈ 0.1% (ewma_vol = 1_000_000):
//   fee = 800_000 + 2×1_000_000 = 2_800_000 = 28 bps  (beats 30 bps normaliser)
//
// At volatile     vol ≈ 0.3% (ewma_vol = 3_000_000):
//   fee = 800_000 + 2×3_000_000 = 6_800_000 = 68 bps  (protects against arbs)
//
// On first swap   ewma_vol = 0:
//   fee = 8 bps  → dominant retail attraction before market moves

const BASE_FEE_1E9: u64  = 800_000;        //  8 bps
const VOL_MULT: u64       = 2;              // vol → fee amplifier
const MAX_FEE_1E9: u64   = 10_000_000;     // 100 bps hard cap
const ALPHA_1E9: u128     = 200_000_000;   // 0.20 — faster spike response
const ONE_M_ALPHA_1E9: u128 = 800_000_000; // 0.80

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
        0 | 1 => {
            set_return_data_u64(compute_swap(instruction_data));
        }
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

    // Dynamic fee: read EWMA vol from storage
    let ewma_vol = read_u64(&d.storage, 0);
    let fee_1e9  = (BASE_FEE_1E9 + ewma_vol * VOL_MULT).min(MAX_FEE_1E9) as u128;
    let keep     = 1_000_000_000u128 - fee_1e9;

    let k   = rx * ry;
    let net = input * keep / 1_000_000_000;

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

// ── After-swap: update EWMA vol from price change, persist reserves ───────────
fn update_storage(d: &Instruction) -> [u8; STORAGE_SIZE] {
    let mut storage = d.storage;

    let old_vol = read_u64(&storage, 0);
    let last_rx = read_u64(&storage, 8);
    let last_ry = read_u64(&storage, 16);

    let cur_rx = d.reserve_x as u128;
    let cur_ry = d.reserve_y as u128;

    // |Δ(ry/rx)| / (ry/rx) = |cur_ry*last_rx - last_ry*cur_rx| / (last_ry*cur_rx)
    let price_change_1e9: u64 = if last_rx > 0 && last_ry > 0 && cur_rx > 0 {
        let old_rx   = last_rx as u128;
        let old_ry   = last_ry as u128;
        let cross_new = cur_ry * old_rx;
        let cross_old = old_ry * cur_rx;
        let diff  = if cross_new > cross_old { cross_new - cross_old }
                    else                     { cross_old - cross_new };
        let denom = old_ry * cur_rx;
        if denom > 0 {
            (diff * 1_000_000_000 / denom).min(u64::MAX as u128) as u64
        } else { 0 }
    } else { 0 };

    let new_vol = ((ALPHA_1E9   * price_change_1e9 as u128
                  + ONE_M_ALPHA_1E9 * old_vol as u128)
                  / 1_000_000_000) as u64;

    write_u64(&mut storage, 0,  new_vol);
    write_u64(&mut storage, 8,  d.reserve_x);
    write_u64(&mut storage, 16, d.reserve_y);

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
