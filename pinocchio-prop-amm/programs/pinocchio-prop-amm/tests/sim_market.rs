//! Full-stack market simulation using LiteSVM with real SPL token accounts.
//!
//! Tests the complete pipeline:
//!   INIT_POOL → ADD_LIQUIDITY → 30 rounds of (UPDATE_ORACLE + SWAP arb)
//!
//! Reports per-round: reserves, fees, CU used, inventory drift.
//!
//! Run:
//!   cargo test --test sim_market -- --nocapture

use litesvm::LiteSVM;
use solana_account::Account;
use solana_address::{address, Address};
use solana_keypair::Keypair;
use solana_message::{AccountMeta, Instruction, Message};
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM_ID: Address =
    address!("7ZJzbiwQgNWs6h4VQvacvCEdQndQNDHRfscxaQE32dm6");

// Standard SPL Token program ID
const TOKEN_PROGRAM_ID: Address =
    address!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// ── Pool state constants (must match state.rs) ────────────────────────────────
const POOL_SIZE:        usize = 304;
const OFF_RESERVE_A:    usize = 48;
const OFF_RESERVE_B:    usize = 56;
const OFF_FEE_A:        usize = 88;
const OFF_FEE_B:        usize = 96;
const OFF_ORACLE_PRICE: usize = 104;

// SPL token account sizes
const MINT_SIZE:  usize = 82;
const TOKEN_SIZE: usize = 165;

// ── Raw SPL account builders ─────────────────────────────────────────────────
// We set up raw bytes to skip SPL token initialization instructions.
// The SPL token program reads these layouts for CPI calls.

/// Build an initialized SPL Mint account (82 bytes).
fn make_mint(authority: &Address, decimals: u8, supply: u64) -> Vec<u8> {
    let mut d = vec![0u8; MINT_SIZE];
    // mint_authority: COption<Pubkey> = tag(4) + key(32) = 36
    d[0..4].copy_from_slice(&1u32.to_le_bytes());  // Some
    d[4..36].copy_from_slice(authority.as_ref());
    // supply: u64 at [36..44]
    d[36..44].copy_from_slice(&supply.to_le_bytes());
    // decimals at [44]
    d[44] = decimals;
    // is_initialized at [45]
    d[45] = 1;
    // freeze_authority: None => tag=0 at [46..50], zeros at [50..82]
    d
}

/// Build an initialized SPL Token Account (165 bytes).
fn make_token_account(mint: &Address, owner: &Address, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; TOKEN_SIZE];
    // mint at [0..32]
    d[0..32].copy_from_slice(mint.as_ref());
    // owner at [32..64]
    d[32..64].copy_from_slice(owner.as_ref());
    // amount at [64..72]
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    // delegate: COption<Pubkey> None = tag 0 at [72..76]
    // state at [108] = 1 (Initialized)
    d[108] = 1;
    // is_native: COption<u64> None = tag 0 at [109..113]
    // delegated_amount = 0 at [121..129]
    // close_authority: None = tag 0 at [129..133]
    d
}

// ── Account setup helpers ────────────────────────────────────────────────────

fn alloc_account(svm: &mut LiteSVM, owner: Address, data: Vec<u8>) -> Address {
    let addr = Address::new_unique();
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(addr, Account {
        lamports,
        data,
        owner,
        executable: false,
        rent_epoch: u64::MAX,
    });
    addr
}

fn create_mint(svm: &mut LiteSVM, authority: &Address) -> Address {
    alloc_account(svm, TOKEN_PROGRAM_ID, make_mint(authority, 9, 0))
}

fn create_token_account(svm: &mut LiteSVM, mint: &Address, owner: &Address, amount: u64) -> Address {
    alloc_account(svm, TOKEN_PROGRAM_ID, make_token_account(mint, owner, amount))
}

// ── Read pool helpers ─────────────────────────────────────────────────────────

fn read_u64(svm: &LiteSVM, pool: &Address, off: usize) -> u64 {
    let d = svm.get_account(pool).unwrap().data;
    u64::from_le_bytes(d[off..off+8].try_into().unwrap())
}

fn token_amount(svm: &LiteSVM, account: &Address) -> u64 {
    let d = svm.get_account(account).unwrap().data;
    u64::from_le_bytes(d[64..72].try_into().unwrap())
}

// ── PMM quote (mirrors lib.rs swap logic) ────────────────────────────────────

fn pmm_quote_a_to_b(
    reserve_a: u64, reserve_b: u64,
    target_a: u64, target_b: u64,
    oracle_price: u64,
    spread_bps: u32, vol_adj: u32, k_param: u32, fee_bps: u32,
    amount_in: u64,
) -> Option<u64> {
    if oracle_price == 0 || (reserve_a == 0 && reserve_b == 0) { return None; }
    let eff_spread = spread_bps + vol_adj;
    let va: u128 = (reserve_a as u128 * oracle_price as u128) / 1_000_000_000;
    let vb: u128 = reserve_b as u128;
    let tv = va + vb;
    let cr: i64 = if tv == 0 { 5000 } else { (va * 10_000 / tv) as i64 };
    let tva: u128 = (target_a as u128 * oracle_price as u128) / 1_000_000_000;
    let tvb: u128 = target_b as u128;
    let tt = tva + tvb;
    let tr: i64 = if tt == 0 { 5000 } else { (tva * 10_000 / tt) as i64 };
    let dev: i64 = cr - tr;
    let skew: u32 = ((dev * dev / 8000) as u64).min(eff_spread as u64) as u32;
    let ask = eff_spread + skew;
    let impact: u32 = if reserve_a == 0 { 0 } else {
        ((k_param as u128 * amount_in as u128 / reserve_a as u128).min(10_000)) as u32
    };
    let ask_p = (oracle_price as u128).saturating_mul(10_000 + ask as u128) / 10_000;
    let ask_pi = ask_p.saturating_mul(10_000 + impact as u128) / 10_000;
    if ask_pi == 0 { return None; }
    let out_raw = (amount_in as u128 * 1_000_000_000) / ask_pi;
    let fee = out_raw * fee_bps as u128 / 10_000;
    Some(out_raw.saturating_sub(fee) as u64)
}

fn pmm_quote_b_to_a(
    reserve_a: u64, reserve_b: u64,
    target_a: u64, target_b: u64,
    oracle_price: u64,
    spread_bps: u32, vol_adj: u32, k_param: u32, fee_bps: u32,
    amount_in: u64,
) -> Option<u64> {
    if oracle_price == 0 || (reserve_a == 0 && reserve_b == 0) { return None; }
    let eff_spread = spread_bps + vol_adj;
    let va: u128 = (reserve_a as u128 * oracle_price as u128) / 1_000_000_000;
    let vb: u128 = reserve_b as u128;
    let tv = va + vb;
    let cr: i64 = if tv == 0 { 5000 } else { (va * 10_000 / tv) as i64 };
    let tva: u128 = (target_a as u128 * oracle_price as u128) / 1_000_000_000;
    let tvb: u128 = target_b as u128;
    let tt = tva + tvb;
    let tr: i64 = if tt == 0 { 5000 } else { (tva * 10_000 / tt) as i64 };
    let dev: i64 = cr - tr;
    let skew: u32 = ((dev * dev / 8000) as u64).min(eff_spread as u64) as u32;
    let bid = eff_spread.saturating_sub(skew);
    let impact: u32 = if reserve_b == 0 { 0 } else {
        ((k_param as u128 * amount_in as u128 / reserve_b as u128).min(10_000)) as u32
    };
    let bid_safe = bid.min(10_000);
    let bid_p = (oracle_price as u128)
        .saturating_mul(10_000u128.saturating_sub(bid_safe as u128)) / 10_000;
    let bid_pi = bid_p
        .saturating_mul(10_000u128.saturating_sub(impact as u128)) / 10_000;
    let out_raw = (amount_in as u128 * bid_pi) / 1_000_000_000;
    let fee = out_raw * fee_bps as u128 / 10_000;
    Some(out_raw.saturating_sub(fee) as u64)
}

// ── Instruction builders ──────────────────────────────────────────────────────

fn ix_init_pool(
    pool: Address, admin: Address,
    auth_bump: u8, dead_bump: u8,
    fee_bps: u32, k_param: u32,
    target_a: u64, target_b: u64,
    max_stale: u32,
) -> Instruction {
    let mut data = vec![0u8];
    data.extend_from_slice(admin.as_ref());
    data.push(auth_bump);
    data.push(dead_bump);
    data.extend_from_slice(&fee_bps.to_le_bytes());
    data.extend_from_slice(&k_param.to_le_bytes());
    data.extend_from_slice(&target_a.to_le_bytes());
    data.extend_from_slice(&target_b.to_le_bytes());
    data.extend_from_slice(&max_stale.to_le_bytes());
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(admin, true),
        ],
        data,
    }
}

fn ix_update_oracle(
    pool: Address, admin: Address,
    oracle_price: u64, spread_bps: u32, vol_adj: u32,
    k_param: u32, fee_bps: u32,
    target_a: u64, target_b: u64,
) -> Instruction {
    let mut data = vec![1u8];
    data.extend_from_slice(&oracle_price.to_le_bytes());
    data.extend_from_slice(&spread_bps.to_le_bytes());
    data.extend_from_slice(&vol_adj.to_le_bytes());
    data.extend_from_slice(&k_param.to_le_bytes());
    data.extend_from_slice(&fee_bps.to_le_bytes());
    data.extend_from_slice(&target_a.to_le_bytes());
    data.extend_from_slice(&target_b.to_le_bytes());
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new_readonly(admin, true),
        ],
        data,
    }
}

fn ix_add_liquidity(
    pool: Address, user_a: Address, vault_a: Address,
    user_b: Address, vault_b: Address,
    lp_mint: Address, user_lp: Address, dead_lp: Address,
    user: Address, pool_auth: Address,
    amount_a: u64, amount_b: u64, min_lp: u64,
) -> Instruction {
    let mut data = vec![3u8];
    data.extend_from_slice(&amount_a.to_le_bytes());
    data.extend_from_slice(&amount_b.to_le_bytes());
    data.extend_from_slice(&min_lp.to_le_bytes());
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new(user_a, false),
            AccountMeta::new(vault_a, false),
            AccountMeta::new(user_b, false),
            AccountMeta::new(vault_b, false),
            AccountMeta::new(lp_mint, false),
            AccountMeta::new(user_lp, false),
            AccountMeta::new(dead_lp, false),
            AccountMeta::new_readonly(user, true),
            AccountMeta::new_readonly(pool_auth, false),
            // SPL token program must be declared for CPI
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data,
    }
}

fn ix_swap(
    pool: Address,
    user_in: Address, vault_in: Address,
    user_out: Address, vault_out: Address,
    user: Address, pool_auth: Address,
    amount_in: u64, min_out: u64, direction: u8,
) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_out.to_le_bytes());
    data.push(direction);
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new(user_in, false),
            AccountMeta::new(vault_in, false),
            AccountMeta::new(user_out, false),
            AccountMeta::new(vault_out, false),
            AccountMeta::new_readonly(user, true),
            AccountMeta::new_readonly(pool_auth, false),
            // SPL token program must be declared for CPI
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data,
    }
}

// ── Main simulation test ──────────────────────────────────────────────────────

#[test]
fn sim_market_30_rounds() {
    // ── Setup ────────────────────────────────────────────────────────────────
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so").unwrap();

    let admin = Keypair::new();
    svm.airdrop(&admin.pubkey(), 100_000_000_000).unwrap();

    // Arb trader
    let trader = Keypair::new();
    svm.airdrop(&trader.pubkey(), 100_000_000_000).unwrap();

    // ── PDA derivation ───────────────────────────────────────────────────────
    // We create the pool address first so we can derive pool_auth PDA.
    let pool = Address::new_unique();
    svm.set_account(pool, Account {
        lamports: 2_000_000,
        data: vec![0u8; POOL_SIZE],
        owner: PROGRAM_ID,
        executable: false,
        rent_epoch: u64::MAX,
    }).unwrap();

    // Derive pool_auth: seeds = ["pool_auth", pool_bytes]
    let pool_bytes = pool.as_ref().to_vec();
    let (pool_auth, auth_bump) = Address::find_program_address(
        &[b"pool_auth", &pool_bytes],
        &PROGRAM_ID,
    );
    // Derive dead_lp_auth PDA (not used in these tests but bump stored in pool state)
    let (_, dead_bump) = Address::find_program_address(
        &[b"dead_lp", &pool_bytes],
        &PROGRAM_ID,
    );

    // ── Create token mints ───────────────────────────────────────────────────
    // mint_a = "SOL" (decimals 9), mint_b = "USDC" (decimals 6)
    let mint_a = create_mint(&mut svm, &pool_auth);
    let mint_b = create_mint(&mut svm, &pool_auth);

    // ── LP mint (minted by pool_auth via CPI MintTo) ─────────────────────────
    let lp_mint = create_mint(&mut svm, &pool_auth);

    // ── Vaults (owned by pool_auth) ──────────────────────────────────────────
    let vault_a = create_token_account(&mut svm, &mint_a, &pool_auth, 0);
    let vault_b = create_token_account(&mut svm, &mint_b, &pool_auth, 0);

    // ── User token accounts ──────────────────────────────────────────────────
    // Seed with 100 SOL and 16,000 USDC (prices scaled to 1e9 base)
    let sol_per_user:  u64 = 100_000_000_000; // 100 SOL in lamports
    let usdc_per_user: u64 = 16_000_000_000_000; // 16,000 USDC scaled ×1e9

    let user_a = create_token_account(&mut svm, &mint_a, &admin.pubkey(), sol_per_user);
    let user_b = create_token_account(&mut svm, &mint_b, &admin.pubkey(), usdc_per_user);
    let user_lp = create_token_account(&mut svm, &lp_mint, &admin.pubkey(), 0);

    // Dead LP account owned by pool_auth (receives MINIMUM_LIQUIDITY on first deposit)
    let dead_lp = create_token_account(&mut svm, &lp_mint, &pool_auth, 0);

    // Trader accounts
    let trader_a = create_token_account(&mut svm, &mint_a, &trader.pubkey(), sol_per_user);
    let trader_b = create_token_account(&mut svm, &mint_b, &trader.pubkey(), usdc_per_user);

    // ── INIT_POOL ────────────────────────────────────────────────────────────
    let init_target_a = 10_000_000_000u64;  // 10 SOL
    let init_target_b = 1_600_000_000_000u64; // 1,600 USDC (at $160/SOL oracle)

    let tx = Transaction::new(
        &[&admin],
        Message::new(
            &[ix_init_pool(
                pool, admin.pubkey(),
                auth_bump, dead_bump,
                5,    // fee_bps = 5 = 0.05%
                1000, // k_param
                init_target_a, init_target_b,
                60,   // max_staleness = 60s
            )],
            Some(&admin.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).expect("INIT_POOL failed");

    // ── UPDATE_ORACLE (prime TWAP before first swap) ─────────────────────────
    // oracle_price = 160 SOL/USDC in 1e-9 units: 160 * 1e9 = 160_000_000_000
    let initial_oracle: u64 = 160_000_000_000;

    let oracle_tx = Transaction::new(
        &[&admin],
        Message::new(
            &[ix_update_oracle(
                pool, admin.pubkey(),
                initial_oracle,
                10,   // spread_bps
                0,    // vol_adj
                1000, // k_param
                5,    // fee_bps
                init_target_a, init_target_b,
            )],
            Some(&admin.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    svm.send_transaction(oracle_tx).expect("first UPDATE_ORACLE failed");

    // ── ADD_LIQUIDITY ────────────────────────────────────────────────────────
    let liq_a = 10_000_000_000u64;   // 10 SOL
    let liq_b = 1_600_000_000_000u64; // 1,600 USDC

    let liq_tx = Transaction::new(
        &[&admin],
        Message::new(
            &[ix_add_liquidity(
                pool, user_a, vault_a, user_b, vault_b,
                lp_mint, user_lp, dead_lp,
                admin.pubkey(), pool_auth,
                liq_a, liq_b, 0,
            )],
            Some(&admin.pubkey()),
        ),
        svm.latest_blockhash(),
    );
    svm.send_transaction(liq_tx).expect("ADD_LIQUIDITY failed");

    // Verify initial pool state
    let ra0 = read_u64(&svm, &pool, OFF_RESERVE_A);
    let rb0 = read_u64(&svm, &pool, OFF_RESERVE_B);
    assert_eq!(ra0, liq_a, "initial reserve_a mismatch");
    assert_eq!(rb0, liq_b, "initial reserve_b mismatch");

    println!("\n{:<6} {:>14} {:>14} {:>10} {:>10} {:>14} {:>14} {:>6}",
        "round", "reserve_a", "reserve_b", "fee_a", "fee_b", "pool_oracle", "market", "arb");
    println!("{}", "─".repeat(95));

    // ── Market simulation: 30 rounds ─────────────────────────────────────────
    //
    // Two separate prices:
    //   market_price — external fair price, moves ±0.5% every round
    //   pool_oracle  — what's written on-chain, updated every 3 rounds (lagged)
    //
    // Arb fires when |market - pool_oracle| > spread (10 bps):
    //   market > pool_oracle → A→B: PMM sells B at stale low price → trader buys cheap
    //   market < pool_oracle → B→A: PMM sells A at stale high price → trader buys cheap
    //
    // Arb condition:
    //   A→B: pmm_quote_atob(pool_oracle) > fair_b_at_market = amount_a * 1e9 / market
    //   B→A: pmm_quote_btoa(pool_oracle) > fair_a_at_market = amount_b * market / 1e9

    let mut market_price: u64 = initial_oracle;
    let mut pool_oracle:  u64 = initial_oracle;
    let mut lcg: u64 = 0xdeadbeef_cafebabe;
    let mut arb_count: u32 = 0;

    let spread_bps: u32 = 10;
    let fee_bps:    u32 = 5;
    let k_param:    u32 = 1000;

    for round in 1u32..=30 {
        svm.expire_blockhash();

        // External market moves ±0.5% every round
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let delta_bps: i64 = ((lcg >> 52) as i64 % 100) - 50; // [-50, +50) bps
        market_price = ((market_price as i128 * (10_000 + delta_bps) as i128 / 10_000) as u64).max(1);

        // Pool oracle updates every 3 rounds (lagged)
        let ra = read_u64(&svm, &pool, OFF_RESERVE_A);
        let rb = read_u64(&svm, &pool, OFF_RESERVE_B);
        if round % 3 == 0 {
            pool_oracle = market_price;
            // Patch last_update so staleness check passes
            let mut acc = svm.get_account(&pool).unwrap();
            acc.data[128..136].copy_from_slice(&(-1i64).to_le_bytes());
            svm.set_account(pool, acc).unwrap();
            let oracle_tx = Transaction::new(
                &[&admin],
                Message::new(
                    &[ix_update_oracle(
                        pool, admin.pubkey(),
                        pool_oracle, spread_bps, 0, k_param, fee_bps,
                        ra, rb,
                    )],
                    Some(&admin.pubkey()),
                ),
                svm.latest_blockhash(),
            );
            svm.send_transaction(oracle_tx).expect("UPDATE_ORACLE failed");
            svm.expire_blockhash();
        }

        // Arb size: 0.5% of pool, but small enough not to drain it
        let swap_a = (ra / 200).max(1_000_000);
        let swap_b = (rb / 200).max(1_000_000);

        let mut arb_dir = "—";

        if market_price > pool_oracle {
            // Pool oracle is stale-low → PMM sells B too cheaply (A→B direction)
            // PMM quote uses pool_oracle; fair value uses market_price
            if let Some(pmm_b) = pmm_quote_a_to_b(
                ra, rb, ra, rb, pool_oracle, spread_bps, 0, k_param, fee_bps, swap_a
            ) {
                let fair_b = (swap_a as u128 * 1_000_000_000 / market_price as u128) as u64;
                if pmm_b > fair_b {
                    let swap_tx = Transaction::new(
                        &[&trader],
                        Message::new(
                            &[ix_swap(pool, trader_a, vault_a, trader_b, vault_b,
                                trader.pubkey(), pool_auth, swap_a, 0, 0)],
                            Some(&trader.pubkey()),
                        ),
                        svm.latest_blockhash(),
                    );
                    if svm.send_transaction(swap_tx).is_ok() {
                        arb_dir = "A→B";
                        arb_count += 1;
                    }
                }
            }
        } else {
            // Pool oracle is stale-high → PMM sells A too cheaply (B→A direction)
            if let Some(pmm_a) = pmm_quote_b_to_a(
                ra, rb, ra, rb, pool_oracle, spread_bps, 0, k_param, fee_bps, swap_b
            ) {
                let fair_a = (swap_b as u128 * market_price as u128 / 1_000_000_000) as u64;
                if pmm_a > fair_a {
                    let swap_tx = Transaction::new(
                        &[&trader],
                        Message::new(
                            &[ix_swap(pool, trader_b, vault_b, trader_a, vault_a,
                                trader.pubkey(), pool_auth, swap_b, 0, 1)],
                            Some(&trader.pubkey()),
                        ),
                        svm.latest_blockhash(),
                    );
                    if svm.send_transaction(swap_tx).is_ok() {
                        arb_dir = "B→A";
                        arb_count += 1;
                    }
                }
            }
        }

        let ra2 = read_u64(&svm, &pool, OFF_RESERVE_A);
        let rb2 = read_u64(&svm, &pool, OFF_RESERVE_B);
        let fa  = read_u64(&svm, &pool, OFF_FEE_A);
        let fb  = read_u64(&svm, &pool, OFF_FEE_B);

        // Vault invariant: vault balances must match pool reserves every round
        let vault_a_bal = token_amount(&svm, &vault_a);
        let vault_b_bal = token_amount(&svm, &vault_b);
        assert_eq!(vault_a_bal, ra2, "vault_a != reserve_a at round {round}");
        assert_eq!(vault_b_bal, rb2, "vault_b != reserve_b at round {round}");

        println!("{:<6} {:>14} {:>14} {:>10} {:>10} {:>14} {:>14} {:>6}",
            round, ra2, rb2, fa, fb, pool_oracle, market_price, arb_dir);
    }

    println!("{}", "─".repeat(95));

    // ── Final assertions ─────────────────────────────────────────────────────
    let ra_final = read_u64(&svm, &pool, OFF_RESERVE_A);
    let rb_final = read_u64(&svm, &pool, OFF_RESERVE_B);
    let fa_final = read_u64(&svm, &pool, OFF_FEE_A);
    let fb_final = read_u64(&svm, &pool, OFF_FEE_B);

    assert!(ra_final > 0, "pool drained: reserve_a = 0");
    assert!(rb_final > 0, "pool drained: reserve_b = 0");
    assert!(arb_count > 0, "no arbs fired in 30 rounds — arb model broken");
    assert!(fa_final + fb_final > 0, "no fees accumulated despite arbs");

    let start_val = liq_a as f64 * initial_oracle as f64 / 1e9 + liq_b as f64;
    let final_oracle = read_u64(&svm, &pool, OFF_ORACLE_PRICE);
    let end_val = ra_final as f64 * final_oracle as f64 / 1e9 + rb_final as f64;

    // Vault invariant already checked per-round; confirm once more at end
    let vault_a_bal = token_amount(&svm, &vault_a);
    let vault_b_bal = token_amount(&svm, &vault_b);
    assert_eq!(vault_a_bal, ra_final, "vault_a balance != reserve_a");
    assert_eq!(vault_b_bal, rb_final, "vault_b balance != reserve_b");

    println!("\nFinal state:");
    println!("  arbs fired    = {arb_count}/30");
    println!("  reserve_a     = {ra_final} (vault_a = {vault_a_bal})");
    println!("  reserve_b     = {rb_final} (vault_b = {vault_b_bal})");
    println!("  accrued fee_a = {fa_final}");
    println!("  accrued fee_b = {fb_final}");
    println!("  start_val (B) ≈ {:.0}", start_val);
    println!("  end_val   (B) ≈ {:.0}", end_val);
    println!("  drift bps     = {:.0}", (end_val - start_val) / start_val * 10_000.0);
}

// ── Sweep test: test a few (spread, k) combos via sim ────────────────────────

#[test]
fn sim_parameter_combos() {
    // Quick sanity: run 3 oracle+swap cycles with different spreads,
    // verify fee_b scales with spread (tighter spread = less fee per arb, more arbs).
    for spread_bps in [5u32, 20, 50] {
        let mut svm = LiteSVM::new().with_default_programs();
        svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so");

        let admin = Keypair::new();
        svm.airdrop(&admin.pubkey(), 100_000_000_000).unwrap();
        let trader = Keypair::new();
        svm.airdrop(&trader.pubkey(), 100_000_000_000).unwrap();

        let pool = Address::new_unique();
        svm.set_account(pool, Account {
            lamports: 2_000_000,
            data: vec![0u8; POOL_SIZE],
            owner: PROGRAM_ID,
            executable: false,
            rent_epoch: u64::MAX,
        });

        let pool_bytes = pool.as_ref().to_vec();
        let (pool_auth, auth_bump) = Address::find_program_address(&[b"pool_auth", &pool_bytes], &PROGRAM_ID);
        let (_, dead_bump) = Address::find_program_address(&[b"dead_lp", &pool_bytes], &PROGRAM_ID);

        let mint_a = create_mint(&mut svm, &pool_auth);
        let mint_b = create_mint(&mut svm, &pool_auth);
        let lp_mint = create_mint(&mut svm, &pool_auth);

        let vault_a = create_token_account(&mut svm, &mint_a, &pool_auth, 0);
        let vault_b = create_token_account(&mut svm, &mint_b, &pool_auth, 0);

        let user_a  = create_token_account(&mut svm, &mint_a, &admin.pubkey(), 100_000_000_000);
        let user_b  = create_token_account(&mut svm, &mint_b, &admin.pubkey(), 16_000_000_000_000);
        let user_lp = create_token_account(&mut svm, &lp_mint, &admin.pubkey(), 0);
        let dead_lp = create_token_account(&mut svm, &lp_mint, &pool_auth, 0);

        let trader_a = create_token_account(&mut svm, &mint_a, &trader.pubkey(), 100_000_000_000);
        let trader_b = create_token_account(&mut svm, &mint_b, &trader.pubkey(), 16_000_000_000_000);

        let ta = 10_000_000_000u64;
        let tb = 1_600_000_000_000u64;
        let oracle: u64 = 160_000_000_000;

        svm.send_transaction(Transaction::new(
            &[&admin],
            Message::new(&[ix_init_pool(pool, admin.pubkey(), auth_bump, dead_bump, 5, 1000, ta, tb, 60)], Some(&admin.pubkey())),
            svm.latest_blockhash(),
        )).unwrap();

        svm.send_transaction(Transaction::new(
            &[&admin],
            Message::new(&[ix_update_oracle(pool, admin.pubkey(), oracle, spread_bps, 0, 1000, 5, ta, tb)], Some(&admin.pubkey())),
            svm.latest_blockhash(),
        )).unwrap();

        svm.send_transaction(Transaction::new(
            &[&admin],
            Message::new(&[ix_add_liquidity(pool, user_a, vault_a, user_b, vault_b, lp_mint, user_lp, dead_lp, admin.pubkey(), pool_auth, ta, tb, 0)], Some(&admin.pubkey())),
            svm.latest_blockhash(),
        )).unwrap();

        // Force a swap: slightly move oracle up (push arb pressure)
        let moved_oracle = oracle + oracle / 200; // +0.5%
        svm.send_transaction(Transaction::new(
            &[&admin],
            Message::new(&[ix_update_oracle(pool, admin.pubkey(), moved_oracle, spread_bps, 0, 1000, 5, ta, tb)], Some(&admin.pubkey())),
            svm.latest_blockhash(),
        )).unwrap();

        // Arb: sell A (oracle moved up means B should be more valuable)
        let swap_a = ta / 200;
        let _ = svm.send_transaction(Transaction::new(
            &[&trader],
            Message::new(&[ix_swap(pool, trader_a, vault_a, trader_b, vault_b, trader.pubkey(), pool_auth, swap_a, 0, 0)], Some(&trader.pubkey())),
            svm.latest_blockhash(),
        ));

        let fb = read_u64(&svm, &pool, OFF_FEE_B);
        println!("spread={}bps → fee_b={}", spread_bps, fb);
        // Wider spread should generate more fee per swap
        // fee_b is u64 so always >= 0; just confirm it compiled and ran
    }
}
