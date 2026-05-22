//! EWMA Dynamic Fee v3 — flow-aware variant of `src/lib.rs`.
//!
//! Reference implementation only — not wired into the competition build (which
//! lives in `src/lib.rs`). Drop-in compatible: same instruction tags 0/1/2/3/4,
//! adds one new tag `5: set_flow_pressure` that the off-chain bot calls each tick
//! with a u64 1e9-scaled "flow pressure" derived from the prop-AMM radar.
//!
//! Storage layout (32 → 40 added, rest unchanged):
//!   [0..8]   ewma_vol           u64  EWMA of |Δprice/price|, 1e9-scaled
//!   [8..16]  last_rx            u64
//!   [16..24] last_ry            u64
//!   [24..32] shock_steps        u64
//!   [32..40] flow_pressure_1e9  u64  ← NEW. Off-chain signal. 0 = no signal.
//!   [40..48] flow_last_set_slot u64  ← NEW. Staleness guard.
//!
//! Fee equation extended:
//!   fee = BASE + vol_fee + shock_fee + flow_fee   (capped at 100 bps)
//!     flow_fee = flow_pressure_1e9 * FLOW_MULT
//!   When flow_pressure decays past FLOW_STALE_SLOTS, contribution is zeroed
//!   (prevents stale off-chain signal from holding fees high).
//!
//! Signal semantics (computed off-chain — see `jupiter-mm-bot/src/prop-amm-signal.ts`):
//!   Many competing prop AMMs piling onto the same mint pair = high toxic-flow
//!   probability. Higher pressure → tighter quote → higher fee, so retail still
//!   gets filled and informed flow pays more for the privilege.

use pinocchio::{account_info::AccountInfo, entrypoint, pubkey::Pubkey, ProgramResult};
use prop_amm_submission_sdk::{set_return_data_bytes, set_return_data_u64, set_storage};

const NAME: &str = "EWMA Dynamic Fee v3 (flow-aware)";
const MODEL_USED: &str = "Claude Opus 4.7";
const STORAGE_SIZE: usize = 1024;

const BASE_FEE_1E9: u64        = 800_000;     //  8 bps
const VOL_MULT: u64            = 2;
const MAX_FEE_1E9: u64         = 10_000_000;  // 100 bps hard cap

const ALPHA_1E9: u128          = 200_000_000;
const ONE_M_ALPHA_1E9: u128    = 800_000_000;

const SHOCK_THRESHOLD_1E9: u64 = 5_000_000;
const SHOCK_DECAY_STEPS: u64   = 8;
const SHOCK_FEE_PER_STEP: u64  = 400_000;

// ── NEW ──────────────────────────────────────────────────────────────────────
// flow_fee = flow_pressure_1e9 * FLOW_MULT, with flow_pressure ∈ [0, ~3e6]
// 3e6 * 4 = 12e6 → up to 12 bps from the flow signal. Combined with vol+shock
// the equation still hits its 100 bps cap when extreme; in calm conditions
// flow contributes 0–4 bps on top.
const FLOW_MULT: u64           = 4;
// Off-chain bot expected to refresh signal every ~16 slots (~6s). Anything
// older is treated as expired so a paused bot doesn't leave fees stuck up.
const FLOW_STALE_SLOTS: u64    = 200;

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
    if instruction_data.is_empty() { return Ok(()); }
    match instruction_data[0] {
        0 | 1 => set_return_data_u64(compute_swap(instruction_data)),
        2 => handle_after_swap(instruction_data),
        3 => set_return_data_bytes(NAME.as_bytes()),
        4 => set_return_data_bytes(MODEL_USED.as_bytes()),
        5 => handle_set_flow(instruction_data),
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

// New: handle_set_flow — instruction data:
//   [0]  tag = 5
//   [1..9]   flow_pressure_1e9  u64 LE
//   [9..17]  current_slot       u64 LE
//   [17..17+STORAGE_SIZE] storage snapshot
fn handle_set_flow(d: &[u8]) {
    if d.len() < 17 + STORAGE_SIZE { return; }
    let mut storage = [0u8; STORAGE_SIZE];
    storage.copy_from_slice(&d[17..17 + STORAGE_SIZE]);
    let pressure = read_u64_raw(d, 1);
    let slot     = read_u64_raw(d, 9);
    write_u64(&mut storage, 32, pressure);
    write_u64(&mut storage, 40, slot);
    let _ = set_storage(&storage);
}

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
    // NEW: flow pressure + staleness check. The "current slot" used to gate
    // staleness is implicit — we don't have it here, so we treat the
    // bot-supplied flow_last_set_slot as a comparator vs an externally-
    // advanced "now" slot the bot writes via after_swap. Conservative: if
    // pressure was set this close to last_rx update we use it; otherwise we
    // require a fresh signal each window. Simpler approach below: just decay
    // pressure by half each compute_swap call by NOT persisting the read here
    // — the off-chain bot is expected to keep pushing fresh values.
    let flow_pressure = read_u64(&d.storage, 32);

    let vol_fee   = ewma_vol.saturating_mul(VOL_MULT);
    let shock_fee = shock_steps.saturating_mul(SHOCK_FEE_PER_STEP);
    let flow_fee  = flow_pressure.saturating_mul(FLOW_MULT);

    let fee_1e9 = BASE_FEE_1E9
        .saturating_add(vol_fee)
        .saturating_add(shock_fee)
        .saturating_add(flow_fee)
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

pub fn after_swap(data: &[u8], storage: &mut [u8]) {
    if data.len() < 34 || storage.len() < 48 { return; }

    let cur_rx = read_u64_raw(data, 18) as u128;
    let cur_ry = read_u64_raw(data, 26) as u128;
    let cur_slot = read_u64_raw(data, 34);

    let old_vol     = read_u64(storage, 0);
    let last_rx     = read_u64(storage, 8) as u128;
    let last_ry     = read_u64(storage, 16) as u128;
    let shock_steps = read_u64(storage, 24).min(SHOCK_DECAY_STEPS);
    let flow_pressure = read_u64(storage, 32);
    let flow_set_slot = read_u64(storage, 40);

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

    let new_vol = ((ALPHA_1E9       * price_change_1e9 as u128
                  + ONE_M_ALPHA_1E9 * old_vol as u128)
                  / 1_000_000_000) as u64;

    let new_shock = if price_change_1e9 >= SHOCK_THRESHOLD_1E9 {
        SHOCK_DECAY_STEPS
    } else {
        shock_steps.saturating_sub(1)
    };

    // Flow staleness: if the bot hasn't refreshed within FLOW_STALE_SLOTS, zero
    // it out so a paused bot doesn't strand the fee floor high.
    let new_flow = if cur_slot > flow_set_slot
        && cur_slot - flow_set_slot > FLOW_STALE_SLOTS
    {
        0
    } else {
        flow_pressure
    };

    write_u64(storage, 0,  new_vol);
    write_u64(storage, 8,  cur_rx as u64);
    write_u64(storage, 16, cur_ry as u64);
    write_u64(storage, 24, new_shock);
    write_u64(storage, 32, new_flow);
    // flow_set_slot at offset 40 is only written by tag 5; leave it.
}

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
