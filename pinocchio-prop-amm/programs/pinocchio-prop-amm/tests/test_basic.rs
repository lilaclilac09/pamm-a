use litesvm::LiteSVM;
use solana_account::Account;
use solana_address::{address, Address};
use solana_keypair::Keypair;
// Use re-exports from solana_message so types match what Message::new expects
use solana_message::{AccountMeta, Instruction, Message};
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM_ID: Address =
    address!("7ZJzbiwQgNWs6h4VQvacvCEdQndQNDHRfscxaQE32dm6");

// ── helpers ──────────────────────────────────────────────────────────────────

fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    (svm, payer)
}

/// Create a 100-byte pool account owned by the program.
fn create_pool(svm: &mut LiteSVM) -> Address {
    let pool = Address::new_unique();
    svm.set_account(pool, Account {
        lamports: 1_000_000,
        data: vec![0u8; 100],
        owner: PROGRAM_ID,
        executable: false,
        rent_epoch: u64::MAX,
    });
    pool
}

fn init_pool(svm: &mut LiteSVM, payer: &Keypair, pool: Address) {
    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(pool, false)],
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
    (
        u64::from_le_bytes(data[72..80].try_into().unwrap()),
        u32::from_le_bytes(data[80..84].try_into().unwrap()),
        u32::from_le_bytes(data[84..88].try_into().unwrap()),
        u32::from_le_bytes(data[88..92].try_into().unwrap()),
        u64::from_le_bytes(data[92..100].try_into().unwrap()),
    )
}

fn read_reserves(svm: &LiteSVM, pool: &Address) -> (u64, u64, u64) {
    let data = svm.get_account(pool).unwrap().data;
    (
        u64::from_le_bytes(data[8..16].try_into().unwrap()),
        u64::from_le_bytes(data[16..24].try_into().unwrap()),
        u64::from_le_bytes(data[24..32].try_into().unwrap()),
    )
}

fn write_reserves(svm: &mut LiteSVM, pool: Address, ra: u64, rb: u64) {
    let mut acc = svm.get_account(&pool).unwrap();
    acc.data[8..16].copy_from_slice(&ra.to_le_bytes());
    acc.data[16..24].copy_from_slice(&rb.to_le_bytes());
    svm.set_account(pool, acc);
}

// ── tests ────────────────────────────────────────────────────────────────────

#[test]
fn test_init_pool_sets_defaults() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    let (mid_price, base_spread, vol_factor, skew_factor, target_ratio) =
        read_oracle_params(&svm, &pool);

    assert_eq!(mid_price,    1_000_000_000, "default mid_price");
    assert_eq!(base_spread,  10,            "default base_spread");
    assert_eq!(vol_factor,   5000,          "default vol_factor");
    assert_eq!(skew_factor,  15000,         "default skew_factor");
    assert_eq!(target_ratio, 10000,         "default target_ratio");
}

#[test]
fn test_update_oracle_writes_all_fields() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

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
        accounts: vec![AccountMeta::new(pool, false)],
        data,
    };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).unwrap();

    let (mid, spread, vf, skew, ratio) = read_oracle_params(&svm, &pool);
    assert_eq!(mid,    new_price);
    assert_eq!(spread, new_spread);
    assert_eq!(vf,     new_vf);
    assert_eq!(skew,   new_skew);
    assert_eq!(ratio,  new_ratio);
}

#[test]
fn test_update_oracle_rejects_short_data() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    let ix = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(pool, false)],
        data: vec![1u8; 11], // need 29 bytes total
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
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    let (ra, rb, lp) = read_reserves(&svm, &pool);
    assert_eq!(ra, 0);
    assert_eq!(rb, 0);
    assert_eq!(lp, 0);
}

#[test]
fn test_swap_rejects_zero_reserves() {
    let (mut svm, payer) = setup();
    let pool = create_pool(&mut svm);
    init_pool(&mut svm, &payer, pool);

    let mut data = vec![2u8];
    data.extend_from_slice(&10_000_000u64.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());

    let dummy = Address::new_unique();
    let accounts: Vec<AccountMeta> = (0..7).map(|_| AccountMeta::new(dummy, false)).collect();
    let ix = Instruction { program_id: PROGRAM_ID, accounts, data };
    let tx = Transaction::new(
        &[&payer],
        Message::new(&[ix], Some(&payer.pubkey())),
        svm.latest_blockhash(),
    );
    assert!(svm.send_transaction(tx).is_err(), "zero reserves must fail");
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

// ── math unit tests ──────────────────────────────────────────────────────────

#[test]
fn test_swap_output_formula() {
    let amount_in:   u64 = 10_000_000;
    let reserve_in:  u64 = 1_000_000_000;
    let reserve_out: u64 = 1_000_000_000;
    let base_out = amount_in * reserve_out / (reserve_in + amount_in);
    assert_eq!(base_out, 9_900_990);
}

#[test]
fn test_dynamic_spread_formula() {
    let base_spread: u32 = 10;
    let vol_factor:  u32 = 5000;
    let volatility:  u32 = 4500;
    let dynamic = base_spread + (vol_factor * volatility / 10000);
    assert_eq!(dynamic, 2260);
}

#[test]
fn test_spread_applied_to_output() {
    let base_out:  u64 = 9_900_990;
    let spread:    u32 = 2260;
    let adj = base_out * spread as u64 / 10000;
    let final_out = base_out - adj;
    assert_eq!(adj,       2_237_623);
    assert_eq!(final_out, 7_663_367);
}

#[test]
fn test_isqrt_lp_first_deposit() {
    fn isqrt(n: u64) -> u64 {
        if n == 0 { return 0; }
        let mut x = n; let mut y = x / 2 + 1;
        while y < x { x = y; y = (x + n / x) / 2; } x
    }
    assert_eq!(isqrt(500_000_000u64.saturating_mul(500_000_000)), 500_000_000);
    assert_eq!(isqrt(100 * 400), 200);
    assert_eq!(isqrt(0), 0);
    assert_eq!(isqrt(1), 1);
    assert_eq!(isqrt(u64::MAX), 4_294_967_295);
}

#[test]
fn test_lp_proportional_second_deposit() {
    let (reserve_a, reserve_b, lp_supply) = (1000u128, 1000u128, 1000u128);
    let (amount_a, amount_b) = (100u128, 200u128);
    let share_a = (amount_a * lp_supply / reserve_a) as u64;
    let share_b = (amount_b * lp_supply / reserve_b) as u64;
    assert_eq!(share_a.min(share_b), 100);
}

#[test]
fn test_proportional_withdrawal() {
    let (lp_amount, lp_supply) = (500u128, 1000u128);
    let (reserve_a, reserve_b) = (1000u128, 800u128);
    let out_a = (lp_amount * reserve_a / lp_supply) as u64;
    let out_b = (lp_amount * reserve_b / lp_supply) as u64;
    assert_eq!(out_a, 500);
    assert_eq!(out_b, 400);
}

#[test]
fn test_skew_adjust_formula() {
    let current_ratio: i64 = 6000;
    let target_ratio:  i64 = 5000;
    let skew_factor:   i64 = 15000;
    let deviation = current_ratio - target_ratio;
    let skew_adjust = (deviation * deviation / 8000) * skew_factor / 10000;
    assert_eq!(skew_adjust, 187);
}
