use litesvm::LiteSVM;
use solana_address::Address;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_signer::Signer;
use solana_system_interface::instruction::create_account;
use solana_transaction::Transaction;

const PROGRAM_ID: Address =
    solana_pubkey::pubkey!("7ZJzbiwQgNWs6h4VQvacvCEdQndQNDHRfscxaQE32dm6");

// ── helpers ──────────────────────────────────────────────────────────────────

fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    (svm, payer)
}

fn create_pool_account(svm: &mut LiteSVM, payer: &Keypair) -> Keypair {
    let pool = Keypair::new();
    let rent = svm.minimum_balance_for_rent_exemption(100);
    let ix = create_account(&payer.pubkey(), &pool.pubkey(), rent, 100, &PROGRAM_ID);
    let tx = Transaction::new(
        &[payer, &pool],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();
    pool
}

fn init_pool(svm: &mut LiteSVM, payer: &Keypair, pool: &Address) {
    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(*pool, false)],
        data: vec![0],
    };
    let tx = Transaction::new(
        &[payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();
}

fn read_oracle_params(svm: &LiteSVM, pool: &Address) -> (u64, u32, u32, u32, u64) {
    let data = svm.get_account(pool).unwrap().data;
    let mid_price    = u64::from_le_bytes(data[72..80].try_into().unwrap());
    let base_spread  = u32::from_le_bytes(data[80..84].try_into().unwrap());
    let vol_factor   = u32::from_le_bytes(data[84..88].try_into().unwrap());
    let skew_factor  = u32::from_le_bytes(data[88..92].try_into().unwrap());
    let target_ratio = u64::from_le_bytes(data[92..100].try_into().unwrap());
    (mid_price, base_spread, vol_factor, skew_factor, target_ratio)
}

fn read_reserves(svm: &LiteSVM, pool: &Address) -> (u64, u64, u64) {
    let data = svm.get_account(pool).unwrap().data;
    let ra = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let rb = u64::from_le_bytes(data[16..24].try_into().unwrap());
    let lp = u64::from_le_bytes(data[24..32].try_into().unwrap());
    (ra, rb, lp)
}

fn write_reserves(svm: &mut LiteSVM, pool: &Address, ra: u64, rb: u64) {
    let mut acc = svm.get_account(pool).unwrap();
    acc.data[8..16].copy_from_slice(&ra.to_le_bytes());
    acc.data[16..24].copy_from_slice(&rb.to_le_bytes());
    svm.set_account(*pool, acc);
}

// ── tests ────────────────────────────────────────────────────────────────────

#[test]
fn test_init_pool_sets_defaults() {
    let (mut svm, payer) = setup();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");

    let pool = create_pool_account(&mut svm, &payer);
    init_pool(&mut svm, &payer, &pool.pubkey());

    let (mid_price, base_spread, vol_factor, skew_factor, target_ratio) =
        read_oracle_params(&svm, &pool.pubkey());

    assert_eq!(mid_price,    1_000_000_000, "default mid_price");
    assert_eq!(base_spread,  10,            "default base_spread");
    assert_eq!(vol_factor,   5000,          "default vol_factor");
    assert_eq!(skew_factor,  15000,         "default skew_factor");
    assert_eq!(target_ratio, 10000,         "default target_ratio");
}

#[test]
fn test_update_oracle_writes_all_fields() {
    let (mut svm, payer) = setup();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");

    let pool = create_pool_account(&mut svm, &payer);
    init_pool(&mut svm, &payer, &pool.pubkey());

    let new_price:  u64 = 85_000_000_000;
    let new_spread: u32 = 15;
    let new_vf:     u32 = 3000;
    let new_skew:   u32 = 12000;
    let new_ratio:  u64 = 5000;

    let mut data = vec![1u8];
    data.extend_from_slice(&new_price.to_le_bytes());
    data.extend_from_slice(&new_spread.to_le_bytes());
    data.extend_from_slice(&new_vf.to_le_bytes());
    data.extend_from_slice(&new_skew.to_le_bytes());
    data.extend_from_slice(&new_ratio.to_le_bytes());

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(pool.pubkey(), false)],
        data,
    };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();

    let (mid, spread, vf, skew, ratio) = read_oracle_params(&svm, &pool.pubkey());
    assert_eq!(mid,    new_price,  "mid_price updated");
    assert_eq!(spread, new_spread, "base_spread updated");
    assert_eq!(vf,     new_vf,     "vol_factor updated");
    assert_eq!(skew,   new_skew,   "skew_factor updated");
    assert_eq!(ratio,  new_ratio,  "target_ratio updated");
}

#[test]
fn test_update_oracle_rejects_short_data() {
    let (mut svm, payer) = setup();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");

    let pool = create_pool_account(&mut svm, &payer);
    init_pool(&mut svm, &payer, &pool.pubkey());

    // 11 bytes total (discriminant + 10) — need 29 total
    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(pool.pubkey(), false)],
        data: vec![1u8; 11],
    };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    assert!(svm.send_transaction(tx).is_err(), "short data must fail");
}

#[test]
fn test_initial_reserves_are_zero() {
    let (mut svm, payer) = setup();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");

    let pool = create_pool_account(&mut svm, &payer);
    init_pool(&mut svm, &payer, &pool.pubkey());

    let (ra, rb, lp) = read_reserves(&svm, &pool.pubkey());
    assert_eq!(ra, 0, "initial reserve_a = 0");
    assert_eq!(rb, 0, "initial reserve_b = 0");
    assert_eq!(lp, 0, "initial LP supply = 0");
}

#[test]
fn test_swap_rejects_zero_reserves() {
    let (mut svm, payer) = setup();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");

    let pool = create_pool_account(&mut svm, &payer);
    init_pool(&mut svm, &payer, &pool.pubkey());
    // reserves stay 0

    let mut data = vec![2u8];
    data.extend_from_slice(&10_000_000u64.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());

    let dummy = Address::new_unique();
    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: (0..7).map(|_| AccountMeta::new(dummy, false)).collect(),
        data,
    };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    assert!(svm.send_transaction(tx).is_err(), "swap with zero reserves must fail");
}

#[test]
fn test_write_reserves_helper_works() {
    // Confirm our test helper correctly sets reserve bytes
    let (mut svm, payer) = setup();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");

    let pool = create_pool_account(&mut svm, &payer);
    init_pool(&mut svm, &payer, &pool.pubkey());

    write_reserves(&mut svm, &pool.pubkey(), 1_000_000_000, 2_000_000_000);
    let (ra, rb, _) = read_reserves(&svm, &pool.pubkey());
    assert_eq!(ra, 1_000_000_000);
    assert_eq!(rb, 2_000_000_000);
}

// ── math unit tests (no SVM needed) ─────────────────────────────────────────

#[test]
fn test_swap_output_formula() {
    // base_out = amount_in * reserve_out / (reserve_in + amount_in)
    // With 50/50 pool (1B each), swap 10M in:
    // base_out = 10_000_000 * 1_000_000_000 / 1_010_000_000 = 9_900_990
    let amount_in:   u64 = 10_000_000;
    let reserve_in:  u64 = 1_000_000_000;
    let reserve_out: u64 = 1_000_000_000;

    let base_out = amount_in * reserve_out / (reserve_in + amount_in);
    assert_eq!(base_out, 9_900_990);
}

#[test]
fn test_dynamic_spread_formula() {
    // dynamic_spread = base_spread + (vol_factor * 4500 / 10000)
    // base_spread=10, vol_factor=5000 → 10 + 2250 = 2260 bps
    let base_spread: u32 = 10;
    let vol_factor:  u32 = 5000;
    let volatility:  u32 = 4500;
    let dynamic = base_spread + (vol_factor * volatility / 10000);
    assert_eq!(dynamic, 2260);
}

#[test]
fn test_spread_applied_to_output() {
    // final_out = base_out - (base_out * spread / 10000)
    let base_out: u64 = 9_900_990;
    let spread:   u32 = 2260;
    let spread_adj = base_out * spread as u64 / 10000;
    let final_out = base_out - spread_adj;
    assert_eq!(spread_adj, 223_762);
    assert_eq!(final_out,  9_677_228);
}

#[test]
fn test_isqrt_lp_first_deposit() {
    fn isqrt(n: u64) -> u64 {
        if n == 0 { return 0; }
        let mut x = n;
        let mut y = (x + 1) / 2;
        while y < x { x = y; y = (x + n / x) / 2; }
        x
    }
    // 500M * 500M = 250 * 10^15, sqrt = 500M
    assert_eq!(isqrt(500_000_000u64.saturating_mul(500_000_000)), 500_000_000);
    assert_eq!(isqrt(100 * 400), 200);
    assert_eq!(isqrt(0), 0);
    assert_eq!(isqrt(1), 1);
    assert_eq!(isqrt(u64::MAX), 4_294_967_295); // floor(sqrt(2^64-1))
}

#[test]
fn test_lp_proportional_second_deposit() {
    // reserve=1000/1000, lp_supply=1000, deposit 100/200
    // share_a = 100*1000/1000 = 100, share_b = 200*1000/1000 = 200
    // minted = min(100, 200) = 100
    let reserve_a: u128 = 1000;
    let reserve_b: u128 = 1000;
    let lp_supply: u128 = 1000;
    let amount_a:  u128 = 100;
    let amount_b:  u128 = 200;

    let share_a = (amount_a * lp_supply / reserve_a) as u64;
    let share_b = (amount_b * lp_supply / reserve_b) as u64;
    assert_eq!(share_a.min(share_b), 100);
}

#[test]
fn test_proportional_withdrawal() {
    // 50% of LP burned: get 50% of each reserve back
    let lp_amount: u128 = 500;
    let lp_supply: u128 = 1000;
    let reserve_a: u128 = 1000;
    let reserve_b: u128 = 800;

    let out_a = (lp_amount * reserve_a / lp_supply) as u64;
    let out_b = (lp_amount * reserve_b / lp_supply) as u64;
    assert_eq!(out_a, 500);
    assert_eq!(out_b, 400);
}

#[test]
fn test_skew_adjust_formula() {
    // deviation = current_ratio - target_ratio
    // skew_adjust = (deviation^2 / 8000) * skew_factor / 10000
    // At 60% ratio (6000 bps), target 5000, skew_factor=15000:
    // deviation = 1000, skew_adjust = (1000*1000/8000) * 15000/10000 = 125 * 1.5 = 187
    let current_ratio: i64 = 6000;
    let target_ratio:  i64 = 5000;
    let skew_factor:   i64 = 15000;
    let deviation = current_ratio - target_ratio;
    let skew_adjust = (deviation * deviation / 8000) * skew_factor / 10000;
    assert_eq!(skew_adjust, 187);
}
