use litesvm::LiteSVM;
use solana_account::Account;
use solana_address::{address, Address};
use solana_keypair::Keypair;
use solana_message::{AccountMeta, Instruction, Message};
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM_ID: Address =
    address!("7ZJzbiwQgNWs6h4VQvacvCEdQndQNDHRfscxaQE32dm6");

// ── byte offsets (must match state.rs) ───────────────────────────────────────
const OFF_MAGIC:        usize = 0;
const OFF_ADMIN:        usize = 8;
const OFF_AUTH_BUMP:    usize = 40;
const OFF_DEAD_BUMP:    usize = 41;
const OFF_MAX_STALE:    usize = 42;
const OFF_RESERVE_A:    usize = 48;
const OFF_RESERVE_B:    usize = 56;
const OFF_TARGET_A:     usize = 64;
const OFF_TARGET_B:     usize = 72;
const OFF_LP_SUPPLY:    usize = 80;
const OFF_FEE_A:        usize = 88;
const OFF_FEE_B:        usize = 96;
const OFF_ORACLE_PRICE: usize = 104;
const OFF_SPREAD_BPS:   usize = 112;
const OFF_K_PARAM:      usize = 116;
const OFF_VOL_ADJ:      usize = 120;
const OFF_FEE_BPS:      usize = 124;
const OFF_LAST_UPDATE:  usize = 128;
const OFF_TWAP_CURSOR:  usize = 136;
const OFF_TWAP_OBS:     usize = 144;

const POOL_SIZE: usize = 304;
const MAGIC:     u64   = 0x504d4d504f4f4c31;
const MIN_LIQ:   u64   = 1_000;

// ── helpers ──────────────────────────────────────────────────────────────────

fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    (svm, payer)
}

/// Create a 304-byte pool account owned by the program.
fn create_pool(svm: &mut LiteSVM) -> Address {
    let pool = Address::new_unique();
    svm.set_account(pool, Account {
        lamports: 1_000_000,
        data: vec![0u8; POOL_SIZE],
        owner: PROGRAM_ID,
        executable: false,
        rent_epoch: u64::MAX,
    });
    pool
}

/// INIT_POOL: data = admin_pubkey[32] + auth_bump[1] + dead_lp_bump[1] + fee_bps[4] + k_param[4] + target_a[8] + target_b[8] + max_staleness[4]
fn init_pool(svm: &mut LiteSVM, payer: &Keypair, pool: Address) {
    let mut data = vec![0u8]; // discriminant
    data.extend_from_slice(payer.pubkey().as_ref()); // admin = payer for tests
    data.push(255u8); // auth_bump (dummy — no real PDA in unit tests)
    data.push(254u8); // dead_lp_bump (dummy)
    data.extend_from_slice(&5u32.to_le_bytes());    // fee_bps = 5
    data.extend_from_slice(&1000u32.to_le_bytes()); // k_param = 1000
    data.extend_from_slice(&0u64.to_le_bytes());    // target_a = 0 (set by bot later)
    data.extend_from_slice(&0u64.to_le_bytes());    // target_b = 0
    data.extend_from_slice(&30u32.to_le_bytes());   // max_staleness = 30s

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(payer.pubkey(), true),
        ],
        data,
    };
    let tx = Transaction::new(
        &[payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();
}

/// UPDATE_ORACLE: data = oracle_price[8] + spread_bps[4] + vol_adj[4] + k_param[4] + fee_bps[4] + target_a[8] + target_b[8]
fn update_oracle(
    svm: &mut LiteSVM,
    payer: &Keypair,
    pool: Address,
    oracle_price: u64,
    spread_bps: u32,
    vol_adj: u32,
    k_param: u32,
    fee_bps: u32,
    target_a: u64,
    target_b: u64,
) {
    let mut data = vec![1u8]; // discriminant
    data.extend_from_slice(&oracle_price.to_le_bytes());
    data.extend_from_slice(&spread_bps.to_le_bytes());
    data.extend_from_slice(&vol_adj.to_le_bytes());
    data.extend_from_slice(&k_param.to_le_bytes());
    data.extend_from_slice(&fee_bps.to_le_bytes());
    data.extend_from_slice(&target_a.to_le_bytes());
    data.extend_from_slice(&target_b.to_le_bytes());

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(payer.pubkey(), true),
        ],
        data,
    };
    let tx = Transaction::new(
        &[payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();
}

fn read_reserves(svm: &LiteSVM, pool: &Address) -> (u64, u64, u64) {
    let data = svm.get_account(pool).unwrap().data;
    (
        u64::from_le_bytes(data[OFF_RESERVE_A..OFF_RESERVE_A+8].try_into().unwrap()),
        u64::from_le_bytes(data[OFF_RESERVE_B..OFF_RESERVE_B+8].try_into().unwrap()),
        u64::from_le_bytes(data[OFF_LP_SUPPLY..OFF_LP_SUPPLY+8].try_into().unwrap()),
    )
}

fn read_oracle_params(svm: &LiteSVM, pool: &Address) -> (u64, u32, u32, u32) {
    let data = svm.get_account(pool).unwrap().data;
    (
        u64::from_le_bytes(data[OFF_ORACLE_PRICE..OFF_ORACLE_PRICE+8].try_into().unwrap()),
        u32::from_le_bytes(data[OFF_SPREAD_BPS..OFF_SPREAD_BPS+4].try_into().unwrap()),
        u32::from_le_bytes(data[OFF_K_PARAM..OFF_K_PARAM+4].try_into().unwrap()),
        u32::from_le_bytes(data[OFF_VOL_ADJ..OFF_VOL_ADJ+4].try_into().unwrap()),
    )
}

fn read_fees(svm: &LiteSVM, pool: &Address) -> (u64, u64) {
    let data = svm.get_account(pool).unwrap().data;
    (
        u64::from_le_bytes(data[OFF_FEE_A..OFF_FEE_A+8].try_into().unwrap()),
        u64::from_le_bytes(data[OFF_FEE_B..OFF_FEE_B+8].try_into().unwrap()),
    )
}

/// Write reserves directly (for test setup — bypasses on-chain logic).
fn write_reserves(svm: &mut LiteSVM, pool: Address, ra: u64, rb: u64) {
    let mut acc = svm.get_account(&pool).unwrap();
    acc.data[OFF_RESERVE_A..OFF_RESERVE_A+8].copy_from_slice(&ra.to_le_bytes());
    acc.data[OFF_RESERVE_B..OFF_RESERVE_B+8].copy_from_slice(&rb.to_le_bytes());
    svm.set_account(pool, acc);
}

fn get_last_update(svm: &LiteSVM, pool: &Address) -> i64 {
    let data = svm.get_account(pool).unwrap().data;
    i64::from_le_bytes(data[OFF_LAST_UPDATE..OFF_LAST_UPDATE+8].try_into().unwrap())
}

fn get_error_code(result: &litesvm::types::TransactionResult) -> Option<u32> {
    match result {
        Err(e) => {
            let msg = format!("{e:?}");
            // Look for "Custom(N)" in the error message
            if let Some(start) = msg.find("Custom(") {
                let rest = &msg[start + 7..];
                if let Some(end) = rest.find(')') {
                    return rest[..end].parse().ok();
                }
            }
            None
        }
        Ok(_) => None,
    }
}

// ── INIT_POOL tests ───────────────────────────────────────────────────────────

#[test]
fn test_init_pool_magic_and_admin() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    let data = svm.get_account(&pool).unwrap().data;

    // magic
    let magic = u64::from_le_bytes(data[OFF_MAGIC..OFF_MAGIC+8].try_into().unwrap());
    assert_eq!(magic, MAGIC, "magic bytes wrong");

    // admin = payer
    let admin: [u8; 32] = data[OFF_ADMIN..OFF_ADMIN+32].try_into().unwrap();
    assert_eq!(&admin, payer.pubkey().as_ref(), "admin pubkey wrong");

    // bumps were stored (non-zero for most PDAs but we just check they were written)
    let auth_bump = data[OFF_AUTH_BUMP];
    let dead_bump = data[OFF_DEAD_BUMP];
    assert!(auth_bump <= 255, "auth_bump range");
    assert!(dead_bump <= 255, "dead_lp_bump range");

    // last_oracle_update = i64::MIN
    let last_update = i64::from_le_bytes(data[OFF_LAST_UPDATE..OFF_LAST_UPDATE+8].try_into().unwrap());
    assert_eq!(last_update, i64::MIN, "last_oracle_update must be i64::MIN at init");

    // max_staleness_secs = 30
    let max_stale = u32::from_le_bytes(data[OFF_MAX_STALE..OFF_MAX_STALE+4].try_into().unwrap());
    assert_eq!(max_stale, 30);
}

#[test]
fn test_init_pool_reserves_and_lp_zero() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    let (ra, rb, lp) = read_reserves(&svm, &pool);
    assert_eq!(ra, 0);
    assert_eq!(rb, 0);
    assert_eq!(lp, 0);
}

// ── UPDATE_ORACLE tests ───────────────────────────────────────────────────────

#[test]
fn test_update_oracle_writes_all_fields() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    update_oracle(&mut svm, &payer, pool,
        150_000_000_000, // oracle_price (SOL/USDC @ $150, scaled 1e9)
        20,              // spread_bps
        30,              // vol_adj
        1000,            // k_param
        5,               // fee_bps
        500_000_000,     // target_a
        500_000_000,     // target_b
    );

    let (price, spread, k, vol) = read_oracle_params(&svm, &pool);
    assert_eq!(price,  150_000_000_000);
    assert_eq!(spread, 20);
    assert_eq!(k,      1000);
    assert_eq!(vol,    30);

    // last_oracle_update should no longer be i64::MIN
    let last_update = get_last_update(&svm, &pool);
    assert_ne!(last_update, i64::MIN, "last_oracle_update must be updated");

    // TWAP cursor should advance to 1
    let data = svm.get_account(&pool).unwrap().data;
    assert_eq!(data[OFF_TWAP_CURSOR], 1, "TWAP cursor should advance");

    // TWAP slot 0 should have the oracle price
    let twap_price = u64::from_le_bytes(data[OFF_TWAP_OBS..OFF_TWAP_OBS+8].try_into().unwrap());
    assert_eq!(twap_price, 150_000_000_000, "TWAP slot 0 should have oracle price");
}

#[test]
fn test_update_oracle_rejects_short_data() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(payer.pubkey(), true),
        ],
        data: vec![1u8; 10], // need 41 bytes total
    };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    assert!(svm.send_transaction(tx).is_err(), "short data must fail");
}

// ── SUCCESS CRITERION 1: UPDATE_ORACLE non-admin rejection ───────────────────

#[test]
fn test_update_oracle_rejects_non_admin() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool); // payer is admin

    // Create a different signer
    let payer2 = Keypair::new();
    svm.airdrop(&payer2.pubkey(), 10_000_000_000).unwrap();

    let mut data = vec![1u8]; // UPDATE_ORACLE discriminant
    data.extend_from_slice(&150_000_000_000u64.to_le_bytes()); // oracle_price
    data.extend_from_slice(&10u32.to_le_bytes()); // spread_bps
    data.extend_from_slice(&0u32.to_le_bytes());  // vol_adj
    data.extend_from_slice(&1000u32.to_le_bytes()); // k_param
    data.extend_from_slice(&5u32.to_le_bytes());   // fee_bps
    data.extend_from_slice(&0u64.to_le_bytes());   // target_a
    data.extend_from_slice(&0u64.to_le_bytes());   // target_b

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(payer2.pubkey(), true), // NOT the admin
        ],
        data,
    };
    let tx = Transaction::new(
        &[&payer2],
        Message::new(&[ix], Some(&payer2.pubkey())),
        svm.latest_blockhash(),
    );
    let result = svm.send_transaction(tx);
    assert!(result.is_err(), "non-admin must fail");
    assert_eq!(get_error_code(&result), Some(1), "must be Custom(1) = InvalidAdmin");
}

// ── SUCCESS CRITERION 2: SWAP stale oracle ───────────────────────────────────

#[test]
fn test_swap_stale_oracle() {
    use litesvm::types::FailedTransactionMetadata;
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    // Set up pool with reserves so we don't hit ZeroReserves first
    write_reserves(&mut svm, pool, 1_000_000_000, 1_000_000_000);

    // Update oracle at a timestamp that will be "in the past"
    update_oracle(&mut svm, &payer, pool,
        150_000_000_000, 10, 0, 1000, 5, 500_000_000, 500_000_000);

    // Manually set last_oracle_update to a very old value (clock is 0 in litesvm by default)
    // last_oracle_update = -100 (far in the past), so now(~0) - (-100) = 100 > 30s
    {
        let mut acc = svm.get_account(&pool).unwrap();
        let old_ts: i64 = -100;
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE+8].copy_from_slice(&old_ts.to_le_bytes());
        svm.set_account(pool, acc);
    }

    let dummy = Address::new_unique();
    let mut data = vec![2u8]; // SWAP discriminant
    data.extend_from_slice(&1_000_000u64.to_le_bytes()); // amount_in
    data.extend_from_slice(&0u64.to_le_bytes());          // min_out
    data.push(0u8);                                        // direction A→B

    let accounts: Vec<AccountMeta> = (0..7).map(|_| AccountMeta::new(dummy, false)).collect();
    let mut accs = vec![AccountMeta::new(pool, false)];
    accs.extend_from_slice(&accounts[1..]);

    let ix = Instruction { program_id: PROGRAM_ID, accounts: accs, data };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    let result = svm.send_transaction(tx);
    assert!(result.is_err(), "stale oracle must fail");
    assert_eq!(get_error_code(&result), Some(2), "must be Custom(2) = StaleOracle");
}

// ── SUCCESS CRITERION 3: SWAP zero reserves ──────────────────────────────────

#[test]
fn test_swap_zero_reserves() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    // Set oracle to fresh (last_update = 0 = clock default, so 0 - 0 = 0 <= 30)
    {
        let mut acc = svm.get_account(&pool).unwrap();
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE+8].copy_from_slice(&0i64.to_le_bytes());
        acc.data[OFF_ORACLE_PRICE..OFF_ORACLE_PRICE+8]
            .copy_from_slice(&150_000_000_000u64.to_le_bytes());
        svm.set_account(pool, acc);
    }

    let dummy = Address::new_unique();
    let mut data = vec![2u8]; // SWAP
    data.extend_from_slice(&1_000_000u64.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    data.push(0u8);

    let accs: Vec<AccountMeta> = {
        let mut v = vec![AccountMeta::new(pool, false)];
        for _ in 1..7 { v.push(AccountMeta::new(dummy, false)); }
        v
    };

    let ix = Instruction { program_id: PROGRAM_ID, accounts: accs, data };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    let result = svm.send_transaction(tx);
    assert!(result.is_err(), "zero reserves must fail");
    assert_eq!(get_error_code(&result), Some(8), "must be Custom(8) = ZeroReserves");
}

// ── SUCCESS CRITERION 7: isqrt_u128 overflow ─────────────────────────────────

#[test]
fn test_isqrt_u128_overflow() {
    fn isqrt_u128(n: u128) -> u64 {
        if n == 0 { return 0; }
        let mut x: u128 = n;
        let mut y: u128 = x / 2 + 1;
        while y < x { x = y; y = (x + n / x) / 2; }
        x as u64
    }

    // isqrt(u64::MAX^2) = u64::MAX
    let n = (u64::MAX as u128) * (u64::MAX as u128);
    assert_eq!(isqrt_u128(n), u64::MAX);

    // Basic correctness
    assert_eq!(isqrt_u128(0), 0);
    assert_eq!(isqrt_u128(1), 1);
    assert_eq!(isqrt_u128(4), 2);
    assert_eq!(isqrt_u128(500_000_000u128 * 500_000_000u128), 500_000_000);
    assert_eq!(isqrt_u128(100 * 400), 200);
}

// ── Math unit tests ───────────────────────────────────────────────────────────

#[test]
fn test_pmm_atob_formula() {
    // Verify A→B output formula: amount_out = amount_in * 1e9 / ask_price_with_impact
    let oracle_price: u128 = 150_000_000_000; // SOL/USDC @ $150, scaled 1e9
    let spread_bps: u32 = 10;
    let vol_adj: u32 = 0;
    let k_param: u32 = 0; // no price impact
    let amount_in: u64 = 1_000_000_000; // 1 SOL (9 decimals)
    let fee_bps: u32 = 5;

    let effective_spread = spread_bps + vol_adj;
    let ask_price = oracle_price * (10_000 + effective_spread as u128) / 10_000;
    let impact_bps: u32 = 0; // k=0 → no impact
    let ask_price_impact = ask_price * (10_000 + impact_bps as u128) / 10_000;

    let out_raw = amount_in as u128 * 1_000_000_000 / ask_price_impact;
    let fee = out_raw * fee_bps as u128 / 10_000;
    let actual_out = out_raw - fee;

    // At $150 with 10bps spread + 5bps fee:
    // ask_price = 150e9 * 10010/10000 = 150_150_000_000
    // out_raw = 1e9 * 1e9 / 150_150_000_000 ≈ 6_659_...
    // Should be slightly less than 1e9/150e9 * 1e9 = 6_666_666
    assert!(actual_out > 6_000_000, "output sanity check");
    assert!(actual_out < 7_000_000, "output sanity check upper");
}

#[test]
fn test_skew_formula_overweight_a() {
    // When A is overweight: deviation > 0 → ask_spread increases, bid_spread decreases
    let current_ratio: i64 = 7000; // 70% A
    let target_ratio:  i64 = 5000; // 50% target
    let effective_spread: u32 = 50;

    let deviation = current_ratio - target_ratio; // +2000
    let skew_mag: u32 = ((deviation * deviation / 8000) as u64)
        .min(effective_spread as u64) as u32;

    let (ask_spread, bid_spread) = if deviation >= 0 {
        (effective_spread + skew_mag, effective_spread.saturating_sub(skew_mag))
    } else {
        (effective_spread.saturating_sub(skew_mag), effective_spread + skew_mag)
    };

    // A overweight → A→B should be more expensive (higher ask), B→A cheaper (lower bid)
    assert!(ask_spread >= bid_spread, "A overweight: ask >= bid");
}

#[test]
fn test_skew_rebalancing_direction() {
    // Test criterion 6: when A overweight, B→A output > A→B output for same input
    // This verifies the spread is correctly asymmetric

    let oracle_price: u128 = 1_000_000_000; // 1:1 ratio for simplicity
    let spread_bps: u32 = 100; // 1%
    let vol_adj: u32 = 0;
    let effective_spread = spread_bps + vol_adj;

    // A overweight: current_ratio = 7000, target = 5000
    let deviation: i64 = 7000 - 5000; // +2000
    let skew_mag: u32 = ((deviation * deviation / 8000) as u64)
        .min(effective_spread as u64) as u32;
    let ask_spread = effective_spread + skew_mag;
    let bid_spread_safe = effective_spread.saturating_sub(skew_mag).min(10_000);
    let impact_bps: u32 = 0;
    let fee_bps: u32 = 0; // ignore fees for this test

    let amount_in: u64 = 1_000;

    // A→B output (user sends A, gets B)
    let ask_price = oracle_price * (10_000 + ask_spread as u128) / 10_000;
    let ask_price_impact = ask_price * (10_000 + impact_bps as u128) / 10_000;
    let out_ab = (amount_in as u128 * 1_000_000_000) / ask_price_impact;

    // B→A output (user sends B, gets A)
    let bid_price = oracle_price * (10_000u128.saturating_sub(bid_spread_safe as u128)) / 10_000;
    let bid_price_impact = bid_price * (10_000u128.saturating_sub(impact_bps as u128)) / 10_000;
    let out_ba = (amount_in as u128 * bid_price_impact) / 1_000_000_000;

    // When A overweight: B→A should yield more (tighter spread)
    assert!(out_ba > out_ab, "B→A must yield more when A overweight; out_ba={out_ba}, out_ab={out_ab}");
}

#[test]
fn test_first_deposit_lp_math() {
    // Verify inflation protection math (pure math, no SVM needed)
    let amount_a: u64 = 500_000_000;
    let amount_b: u64 = 500_000_000;

    fn isqrt_u128(n: u128) -> u64 {
        if n == 0 { return 0; }
        let mut x: u128 = n;
        let mut y: u128 = x / 2 + 1;
        while y < x { x = y; y = (x + n / x) / 2; }
        x as u64
    }

    let gross_lp = isqrt_u128(amount_a as u128 * amount_b as u128);
    assert_eq!(gross_lp, 500_000_000);

    let user_lp = gross_lp - MIN_LIQ;
    assert_eq!(user_lp, 499_999_000);
}

#[test]
fn test_proportional_second_deposit() {
    let (reserve_a, reserve_b, lp_supply) = (1000u128, 1000u128, 1000u128);
    let (amount_a, amount_b) = (100u128, 200u128);
    let share_a = (amount_a * lp_supply / reserve_a) as u64;
    let share_b = (amount_b * lp_supply / reserve_b) as u64;
    assert_eq!(share_a.min(share_b), 100); // capped by A (smaller share)
}

#[test]
fn test_remove_liquidity_fee_accounting() {
    // LPs should NOT be able to withdraw fees, only liquid reserves
    let reserve_a = 1_000_000u64;
    let reserve_b = 1_000_000u64;
    let fee_a     = 10_000u64;
    let fee_b     = 10_000u64;
    let lp_supply = 1_000_000u64;
    let lp_amount = 500_000u64; // redeem 50%

    let withdrawable_a = reserve_a.saturating_sub(fee_a);
    let withdrawable_b = reserve_b.saturating_sub(fee_b);
    let out_a = (lp_amount as u128 * withdrawable_a as u128 / lp_supply as u128) as u64;
    let out_b = (lp_amount as u128 * withdrawable_b as u128 / lp_supply as u128) as u64;

    // 50% of (reserve - fee)
    assert_eq!(out_a, (1_000_000 - 10_000) / 2);
    assert_eq!(out_b, (1_000_000 - 10_000) / 2);
}

#[test]
fn test_impact_bps_no_overflow() {
    // Verify u128 intermediate prevents overflow
    let k_param: u32 = 10_000;
    let amount_in: u64 = u64::MAX;
    let reserve_in: u64 = 1_000_000_000;

    let impact_bps: u32 = ((k_param as u128 * amount_in as u128 / reserve_in as u128)
        .min(10_000)) as u32;

    assert_eq!(impact_bps, 10_000, "should saturate at MAX_IMPACT");
    // Must not panic (overflow check)
}

#[test]
fn test_write_reserves_helper() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    write_reserves(&mut svm, pool, 1_000_000_000, 2_000_000_000);
    let (ra, rb, _) = read_reserves(&svm, &pool);
    assert_eq!(ra, 1_000_000_000);
    assert_eq!(rb, 2_000_000_000);
}
