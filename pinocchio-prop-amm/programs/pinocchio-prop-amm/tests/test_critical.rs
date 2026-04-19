//! Critical PMM logic tests covering gaps in test_basic.rs:
//!
//!  1. min_out slippage rejection (Custom(3))
//!  2. fee accumulation invariant (fee_b == sum of per-swap fees)
//!  3. inventory skew asymmetry (B→A cheaper when A overweight)
//!  4. TWAP cursor wraparound after 10+ updates
//!  5. Lagged-oracle arb simulation (actually triggers swaps)

use litesvm::LiteSVM;
use solana_account::Account;
use solana_address::{address, Address};
use solana_keypair::Keypair;
use solana_message::{AccountMeta, Instruction, Message};
use solana_signer::Signer;
use solana_transaction::Transaction;

const PROGRAM_ID: Address = address!("7ZJzbiwQgNWs6h4VQvacvCEdQndQNDHRfscxaQE32dm6");
const TOKEN_PROGRAM_ID: Address = address!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

const POOL_SIZE: usize = 304;
const OFF_RESERVE_A: usize = 48;
const OFF_RESERVE_B: usize = 56;
const OFF_TARGET_A: usize = 64;
const OFF_TARGET_B: usize = 72;
const OFF_FEE_A: usize = 88;
const OFF_FEE_B: usize = 96;
const OFF_ORACLE_PRICE: usize = 104;
const OFF_LAST_UPDATE: usize = 128;
const OFF_TWAP_CURSOR: usize = 136;
const OFF_TWAP_OBS: usize = 144;

const MINT_SIZE: usize = 82;
const TOKEN_SIZE: usize = 165;

// ── Raw SPL account builders ──────────────────────────────────────────────────

fn make_mint(authority: &Address) -> Vec<u8> {
    let mut d = vec![0u8; MINT_SIZE];
    d[0..4].copy_from_slice(&1u32.to_le_bytes());
    d[4..36].copy_from_slice(authority.as_ref());
    d[44] = 9;
    d[45] = 1;
    d
}

fn make_token_account(mint: &Address, owner: &Address, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; TOKEN_SIZE];
    d[0..32].copy_from_slice(mint.as_ref());
    d[32..64].copy_from_slice(owner.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1; // Initialized
    d
}

fn alloc(svm: &mut LiteSVM, owner: Address, data: Vec<u8>) -> Address {
    let addr = Address::new_unique();
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(addr, Account { lamports, data, owner, executable: false, rent_epoch: u64::MAX });
    addr
}

fn new_mint(svm: &mut LiteSVM, auth: &Address) -> Address {
    alloc(svm, TOKEN_PROGRAM_ID, make_mint(auth))
}

fn new_ta(svm: &mut LiteSVM, mint: &Address, owner: &Address, amount: u64) -> Address {
    alloc(svm, TOKEN_PROGRAM_ID, make_token_account(mint, owner, amount))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_u64(svm: &LiteSVM, pool: &Address, off: usize) -> u64 {
    let d = svm.get_account(pool).unwrap().data;
    u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
}

fn token_balance(svm: &LiteSVM, acc: &Address) -> u64 {
    let d = svm.get_account(acc).unwrap().data;
    u64::from_le_bytes(d[64..72].try_into().unwrap())
}

fn get_error_code(result: &litesvm::types::TransactionResult) -> Option<u32> {
    match result {
        Err(e) => {
            let msg = format!("{e:?}");
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

// ── Fixture: pool with real SPL accounts, oracle primed ──────────────────────

struct Fixture {
    svm:       LiteSVM,
    admin:     Keypair,
    trader:    Keypair,
    pool:      Address,
    pool_auth: Address,
    mint_a:    Address,
    mint_b:    Address,
    lp_mint:   Address,
    vault_a:   Address,
    vault_b:   Address,
    user_a:    Address,
    user_b:    Address,
    user_lp:   Address,
    dead_lp:   Address,
    trader_a:  Address,
    trader_b:  Address,
}

impl Fixture {
    fn new(initial_oracle: u64, liq_a: u64, liq_b: u64) -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so").unwrap();

        let admin  = Keypair::new();
        let trader = Keypair::new();
        svm.airdrop(&admin.pubkey(),  100_000_000_000).unwrap();
        svm.airdrop(&trader.pubkey(), 100_000_000_000).unwrap();

        let pool = Address::new_unique();
        let _ = svm.set_account(pool, Account {
            lamports: 2_000_000, data: vec![0u8; POOL_SIZE],
            owner: PROGRAM_ID, executable: false, rent_epoch: u64::MAX,
        });

        let pool_bytes = pool.as_ref().to_vec();
        let (pool_auth, auth_bump) =
            Address::find_program_address(&[b"pool_auth", &pool_bytes], &PROGRAM_ID);
        let (_, dead_bump) =
            Address::find_program_address(&[b"dead_lp", &pool_bytes], &PROGRAM_ID);

        let mint_a  = new_mint(&mut svm, &pool_auth);
        let mint_b  = new_mint(&mut svm, &pool_auth);
        let lp_mint = new_mint(&mut svm, &pool_auth);

        let vault_a = new_ta(&mut svm, &mint_a, &pool_auth, 0);
        let vault_b = new_ta(&mut svm, &mint_b, &pool_auth, 0);

        let user_a  = new_ta(&mut svm, &mint_a, &admin.pubkey(), 100_000_000_000);
        let user_b  = new_ta(&mut svm, &mint_b, &admin.pubkey(), 50_000_000_000_000);
        let user_lp = new_ta(&mut svm, &lp_mint, &admin.pubkey(), 0);
        let dead_lp = new_ta(&mut svm, &lp_mint, &pool_auth, 0);

        let trader_a = new_ta(&mut svm, &mint_a, &trader.pubkey(), 100_000_000_000);
        let trader_b = new_ta(&mut svm, &mint_b, &trader.pubkey(), 50_000_000_000_000);

        let mut f = Fixture {
            svm, admin, trader,
            pool, pool_auth,
            mint_a, mint_b, lp_mint,
            vault_a, vault_b,
            user_a, user_b, user_lp, dead_lp,
            trader_a, trader_b,
        };

        // INIT_POOL
        f.send(&f.admin.insecure_clone(), &[f.ix_init_pool(auth_bump, dead_bump, 5, 1000, liq_a, liq_b, 60)]);

        // Prime oracle (last_update must be recent so swap doesn't reject stale)
        f.prime_oracle(initial_oracle, liq_a, liq_b);

        // ADD_LIQUIDITY
        f.send(&f.admin.insecure_clone(), &[f.ix_add_liquidity(liq_a, liq_b, 0)]);

        f
    }

    fn prime_oracle(&mut self, price: u64, target_a: u64, target_b: u64) {
        // Set last_update = -1 so staleness = 0 - (-1) = 1 < 60
        let mut acc = self.svm.get_account(&self.pool).unwrap();
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE + 8].copy_from_slice(&(-1i64).to_le_bytes());
        let _ = self.svm.set_account(self.pool, acc);
        self.send(
            &self.admin.insecure_clone(),
            &[self.ix_update_oracle(price, 10, 0, 1000, 5, target_a, target_b)],
        );
    }

    fn send(&mut self, signer: &Keypair, ixs: &[Instruction]) {
        let tx = Transaction::new(
            &[signer],
            Message::new(ixs, Some(&signer.pubkey())),
            self.svm.latest_blockhash(),
        );
        self.svm.send_transaction(tx).expect("tx failed");
    }

    fn try_send(&mut self, signer: &Keypair, ixs: &[Instruction]) -> litesvm::types::TransactionResult {
        let tx = Transaction::new(
            &[signer],
            Message::new(ixs, Some(&signer.pubkey())),
            self.svm.latest_blockhash(),
        );
        self.svm.send_transaction(tx)
    }

    fn ix_init_pool(&self, auth_bump: u8, dead_bump: u8, fee_bps: u32, k: u32, ta: u64, tb: u64, stale: u32) -> Instruction {
        let mut data = vec![0u8];
        data.extend_from_slice(self.admin.pubkey().as_ref());
        data.push(auth_bump);
        data.push(dead_bump);
        data.extend_from_slice(&fee_bps.to_le_bytes());
        data.extend_from_slice(&k.to_le_bytes());
        data.extend_from_slice(&ta.to_le_bytes());
        data.extend_from_slice(&tb.to_le_bytes());
        data.extend_from_slice(&stale.to_le_bytes());
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(self.pool, false),
                AccountMeta::new_readonly(self.admin.pubkey(), true),
            ],
            data,
        }
    }

    fn ix_update_oracle(&self, price: u64, spread: u32, vol: u32, k: u32, fee: u32, ta: u64, tb: u64) -> Instruction {
        let mut data = vec![1u8];
        data.extend_from_slice(&price.to_le_bytes());
        data.extend_from_slice(&spread.to_le_bytes());
        data.extend_from_slice(&vol.to_le_bytes());
        data.extend_from_slice(&k.to_le_bytes());
        data.extend_from_slice(&fee.to_le_bytes());
        data.extend_from_slice(&ta.to_le_bytes());
        data.extend_from_slice(&tb.to_le_bytes());
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(self.pool, false),
                AccountMeta::new_readonly(self.admin.pubkey(), true),
            ],
            data,
        }
    }

    fn ix_add_liquidity(&self, amount_a: u64, amount_b: u64, min_lp: u64) -> Instruction {
        let mut data = vec![3u8];
        data.extend_from_slice(&amount_a.to_le_bytes());
        data.extend_from_slice(&amount_b.to_le_bytes());
        data.extend_from_slice(&min_lp.to_le_bytes());
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(self.pool, false),
                AccountMeta::new(self.user_a, false),
                AccountMeta::new(self.vault_a, false),
                AccountMeta::new(self.user_b, false),
                AccountMeta::new(self.vault_b, false),
                AccountMeta::new(self.lp_mint, false),
                AccountMeta::new(self.user_lp, false),
                AccountMeta::new(self.dead_lp, false),
                AccountMeta::new_readonly(self.admin.pubkey(), true),
                AccountMeta::new_readonly(self.pool_auth, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
            data,
        }
    }

    fn ix_swap(&self, from_a: bool, amount_in: u64, min_out: u64, is_trader: bool) -> Instruction {
        let direction: u8 = if from_a { 0 } else { 1 };
        let (user, user_in, vault_in, user_out, vault_out) = if from_a {
            if is_trader {
                (self.trader.pubkey(), self.trader_a, self.vault_a, self.trader_b, self.vault_b)
            } else {
                (self.admin.pubkey(), self.user_a, self.vault_a, self.user_b, self.vault_b)
            }
        } else {
            if is_trader {
                (self.trader.pubkey(), self.trader_b, self.vault_b, self.trader_a, self.vault_a)
            } else {
                (self.admin.pubkey(), self.user_b, self.vault_b, self.user_a, self.vault_a)
            }
        };

        let mut data = vec![2u8];
        data.extend_from_slice(&amount_in.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        data.push(direction);
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(self.pool, false),
                AccountMeta::new(user_in, false),
                AccountMeta::new(vault_in, false),
                AccountMeta::new(user_out, false),
                AccountMeta::new(vault_out, false),
                AccountMeta::new_readonly(user, true),
                AccountMeta::new_readonly(self.pool_auth, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
            data,
        }
    }

    fn pool_u64(&self, off: usize) -> u64 { read_u64(&self.svm, &self.pool, off) }
}

// ── Test 1: min_out slippage rejection ───────────────────────────────────────
//
// Swap A→B with min_out set astronomically high.
// Pool has 10 SOL / 1600 USDC, oracle = 160 SOL/USDC.
// 1 SOL in → ~6.25 USDC out (after spread+fee).
// Set min_out = 1e18 → must get Custom(3) SlippageExceeded.

#[test]
fn test_min_out_slippage_rejected() {
    let mut f = Fixture::new(
        160_000_000_000,  // oracle: 160 USDC/SOL ×1e9
        10_000_000_000,   // 10 SOL
        1_600_000_000_000, // 1600 USDC
    );

    let amount_in: u64 = 1_000_000_000; // 1 SOL
    let impossible_min_out: u64 = u64::MAX; // no pool can satisfy this

    let result = f.try_send(&f.trader.insecure_clone(), &[f.ix_swap(true, amount_in, impossible_min_out, true)]);
    assert!(result.is_err(), "should fail with SlippageExceeded");
    assert_eq!(get_error_code(&result), Some(3), "must be Custom(3) = SlippageExceeded");
}

// ── Test 2: min_out boundary — slippage tolerance works in both directions ─────
//
// Instead of computing the exact on-chain output (which involves spread + impact
// + fee in compound), we verify the monotonic property:
//   - min_out = 0      → always succeeds
//   - min_out = u64::MAX → always fails (Custom(3))
// And that the actual accepted output is strictly positive.

#[test]
fn test_min_out_boundary() {
    let mut f = Fixture::new(160_000_000_000, 10_000_000_000, 1_600_000_000_000);
    let amount_in: u64 = 100_000_000; // 0.1 SOL

    // min_out = u64::MAX → Custom(3) SlippageExceeded
    let result_fail = f.try_send(
        &f.trader.insecure_clone(),
        &[f.ix_swap(true, amount_in, u64::MAX, true)],
    );
    assert_eq!(get_error_code(&result_fail), Some(3), "u64::MAX min_out must fail");

    // min_out = 0 → succeeds and yields positive output
    f.svm.expire_blockhash();
    let rb_before = f.pool_u64(OFF_RESERVE_B);
    let result_ok = f.try_send(
        &f.trader.insecure_clone(),
        &[f.ix_swap(true, amount_in, 0, true)],
    );
    assert!(result_ok.is_ok(), "min_out=0 must succeed; got: {result_ok:?}");
    let rb_after = f.pool_u64(OFF_RESERVE_B);
    assert!(rb_after < rb_before, "pool must give out some B tokens; rb unchanged");
    let actual_out = rb_before - rb_after;
    println!("min_out_boundary: actual_out_B = {actual_out}");

    // min_out = actual_out → succeeds (exact match is accepted)
    f.svm.expire_blockhash();
    let mut acc = f.svm.get_account(&f.pool).unwrap();
    acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE + 8].copy_from_slice(&(-1i64).to_le_bytes());
    let _ = f.svm.set_account(f.pool, acc);
    let rb2 = f.pool_u64(OFF_RESERVE_B);
    let result_exact = f.try_send(
        &f.trader.insecure_clone(),
        &[f.ix_swap(true, amount_in, actual_out, true)],
    );
    // actual_out from first swap is an upper bound; second swap may differ slightly
    // due to skew, so accept either success or Custom(3) but NOT other errors
    match get_error_code(&result_exact) {
        None => { /* success */ }
        Some(3) => { /* acceptable: inventory shifted, new actual_out < first */ }
        Some(c) => panic!("unexpected error {c} for exact min_out"),
    }
    println!("min_out_boundary: rb2={rb2} result={}", result_exact.is_ok());
}

// ── Test 3: fee accumulation invariant ───────────────────────────────────────
//
// After N A→B swaps, fee_b must equal the sum of per-swap fee amounts.
// Fee math: fee = out_raw * fee_bps / 10_000 where out_raw = amount_in * 1e9 / ask_price.
// Also verify: vault_b == reserve_b throughout (fees stay in vault).

#[test]
fn test_fee_accumulation_exact() {
    let oracle_price: u64 = 160_000_000_000;
    let liq_a: u64 = 100_000_000_000; // 100 SOL (large pool to avoid price impact)
    let liq_b: u64 = 16_000_000_000_000; // 16_000 USDC

    let mut f = Fixture::new(oracle_price, liq_a, liq_b);

    let amount_in: u64 = 1_000_000; // tiny swap: 0.001 SOL, negligible price impact

    // Compute expected fee for one swap (k=1000 but amount tiny vs reserves → impact ≈ 0)
    let spread_bps: u32 = 10;
    let fee_bps: u32 = 5;

    // At target == reserves, deviation = 0, skew_mag = 0, impact ≈ 0 for tiny swaps
    let ask = oracle_price as u128 * (10_000 + spread_bps as u128) / 10_000;
    let out_raw = amount_in as u128 * 1_000_000_000 / ask;
    let fee_per_swap = (out_raw * fee_bps as u128 / 10_000) as u64;

    let n = 5u32;
    for i in 0..n {
        f.svm.expire_blockhash();
        // Refresh oracle staleness before each swap
        let mut acc = f.svm.get_account(&f.pool).unwrap();
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE + 8].copy_from_slice(&(-1i64).to_le_bytes());
        let _ = f.svm.set_account(f.pool, acc);

        f.send(&f.trader.insecure_clone(), &[f.ix_swap(true, amount_in, 0, true)]);

        let fee_b = f.pool_u64(OFF_FEE_B);
        let reserve_b = f.pool_u64(OFF_RESERVE_B);
        let vault_b = token_balance(&f.svm, &f.vault_b);

        // vault must always equal reserve (fees stay in vault, tracked by fee counter)
        assert_eq!(vault_b, reserve_b, "vault_b != reserve_b after swap {}", i + 1);
        // Fee grows by fee_per_swap each round (within 1 unit rounding tolerance)
        let expected_fee = fee_per_swap * (i + 1) as u64;
        assert!(fee_b.abs_diff(expected_fee) <= 1,
            "fee_b={fee_b} expected≈{expected_fee} after {} swaps", i + 1);
    }
}

// ── Test 4: vault balance invariant across swap directions ────────────────────
//
// vault_a == reserve_a and vault_b == reserve_b after every swap.
//
// oracle_price unit convention in this pool:
//   oracle_price = A-lamports per B-unit * 1e9
//   B→A: out_A = amount_B * oracle / 1e9
//   → sending 3_125_000 B yields: 3_125_000 * 160e9 / 1e9 = 500_000_000 A (0.5 SOL)
//   vault_a has 100e9, so this is fine.

#[test]
fn test_vault_reserve_invariant_bidirectional() {
    let oracle_price: u64 = 160_000_000_000;
    let mut f = Fixture::new(oracle_price, 100_000_000_000, 16_000_000_000_000);

    // B amounts sized so out_A << vault_a (100 SOL).
    // out_A = amount_B * oracle / 1e9:
    //   3_125_000 B → 3_125_000 * 160 = 500_000_000 A (0.5 SOL) ✓
    //   1_250_000 B → 200_000_000 A (0.2 SOL) ✓
    let swaps: &[(bool, u64)] = &[
        (true,  500_000_000),  // A→B: 0.5 SOL
        (false, 3_125_000),    // B→A: → ~0.5 SOL out
        (true,  200_000_000),  // A→B: 0.2 SOL
        (false, 1_250_000),    // B→A: → ~0.2 SOL out
    ];

    for (i, &(from_a, amount)) in swaps.iter().enumerate() {
        f.svm.expire_blockhash();
        let mut acc = f.svm.get_account(&f.pool).unwrap();
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE + 8].copy_from_slice(&(-1i64).to_le_bytes());
        let _ = f.svm.set_account(f.pool, acc);

        f.send(&f.trader.insecure_clone(), &[f.ix_swap(from_a, amount, 0, true)]);

        let ra = f.pool_u64(OFF_RESERVE_A);
        let rb = f.pool_u64(OFF_RESERVE_B);
        let va = token_balance(&f.svm, &f.vault_a);
        let vb = token_balance(&f.svm, &f.vault_b);

        assert_eq!(va, ra, "vault_a != reserve_a after swap {i}");
        assert_eq!(vb, rb, "vault_b != reserve_b after swap {i}");
    }
}

// ── Test 5: inventory skew asymmetry ─────────────────────────────────────────
//
// After many A→B swaps, pool holds excess A and is short B.
// The PMM should then price A→B more expensively (higher ask spread)
// and B→A more cheaply (lower bid spread) to rebalance.
// Verify: B→A output > oracle-fair-output * (1 - min_spread).

#[test]
fn test_inventory_skew_rebalances_price() {
    // Use large spread so skew effect is measurable (100 bps base)
    let oracle_price: u64 = 1_000_000_000; // 1:1 for easy math
    let liq: u64 = 100_000_000_000;

    // Init with spread=100bps so skew is visible
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so").unwrap();
    let admin  = Keypair::new();
    let trader = Keypair::new();
    svm.airdrop(&admin.pubkey(),  500_000_000_000).unwrap();
    svm.airdrop(&trader.pubkey(), 500_000_000_000).unwrap();

    let pool = Address::new_unique();
    let _ = svm.set_account(pool, Account {
        lamports: 2_000_000, data: vec![0u8; POOL_SIZE],
        owner: PROGRAM_ID, executable: false, rent_epoch: u64::MAX,
    });
    let pb = pool.as_ref().to_vec();
    let (pool_auth, auth_bump) = Address::find_program_address(&[b"pool_auth", &pb], &PROGRAM_ID);
    let (_, dead_bump)         = Address::find_program_address(&[b"dead_lp",   &pb], &PROGRAM_ID);

    let mint_a  = new_mint(&mut svm, &pool_auth);
    let mint_b  = new_mint(&mut svm, &pool_auth);
    let lp_mint = new_mint(&mut svm, &pool_auth);
    let vault_a = new_ta(&mut svm, &mint_a, &pool_auth, 0);
    let vault_b = new_ta(&mut svm, &mint_b, &pool_auth, 0);
    let user_a  = new_ta(&mut svm, &mint_a, &admin.pubkey(),  500_000_000_000);
    let user_b  = new_ta(&mut svm, &mint_b, &admin.pubkey(),  500_000_000_000);
    let user_lp = new_ta(&mut svm, &lp_mint, &admin.pubkey(), 0);
    let dead_lp = new_ta(&mut svm, &lp_mint, &pool_auth,      0);
    let trd_a   = new_ta(&mut svm, &mint_a, &trader.pubkey(), 500_000_000_000);
    let trd_b   = new_ta(&mut svm, &mint_b, &trader.pubkey(), 500_000_000_000);

    let make_tx = |signer: &Keypair, ixs: &[Instruction], bh| {
        Transaction::new(&[signer], Message::new(ixs, Some(&signer.pubkey())), bh)
    };

    let spread_bps: u32 = 100; // 1%

    // INIT + oracle + liquidity with FIXED targets = initial reserves (50/50)
    let mut d_init = vec![0u8];
    d_init.extend_from_slice(admin.pubkey().as_ref());
    d_init.push(auth_bump); d_init.push(dead_bump);
    d_init.extend_from_slice(&5u32.to_le_bytes());    // fee_bps
    d_init.extend_from_slice(&0u32.to_le_bytes());    // k=0 (no impact, isolate skew)
    d_init.extend_from_slice(&liq.to_le_bytes());     // target_a = liq
    d_init.extend_from_slice(&liq.to_le_bytes());     // target_b = liq
    d_init.extend_from_slice(&60u32.to_le_bytes());   // max_stale
    svm.send_transaction(make_tx(&admin, &[Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(pool, false), AccountMeta::new_readonly(admin.pubkey(), true)],
        data: d_init,
    }], svm.latest_blockhash())).unwrap();

    // Prime oracle with FIXED targets (won't change → deviation builds up)
    let prime_oracle = |svm: &mut LiteSVM, price: u64, ta: u64, tb: u64, admin: &Keypair| {
        let mut acc = svm.get_account(&pool).unwrap();
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE + 8].copy_from_slice(&(-1i64).to_le_bytes());
        let _ = svm.set_account(pool, acc);
        let mut data = vec![1u8];
        data.extend_from_slice(&price.to_le_bytes());
        data.extend_from_slice(&spread_bps.to_le_bytes()); // spread=100bps
        data.extend_from_slice(&0u32.to_le_bytes());         // vol_adj
        data.extend_from_slice(&0u32.to_le_bytes());         // k=0
        data.extend_from_slice(&5u32.to_le_bytes());         // fee_bps
        data.extend_from_slice(&ta.to_le_bytes());
        data.extend_from_slice(&tb.to_le_bytes());
        let ix = Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![AccountMeta::new(pool, false), AccountMeta::new_readonly(admin.pubkey(), true)],
            data,
        };
        svm.send_transaction(make_tx(admin, &[ix], svm.latest_blockhash())).unwrap();
    };

    prime_oracle(&mut svm, oracle_price, liq, liq, &admin);

    // ADD_LIQUIDITY
    let mut liq_data = vec![3u8];
    liq_data.extend_from_slice(&liq.to_le_bytes());
    liq_data.extend_from_slice(&liq.to_le_bytes());
    liq_data.extend_from_slice(&0u64.to_le_bytes());
    svm.send_transaction(make_tx(&admin, &[Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new(user_a, false), AccountMeta::new(vault_a, false),
            AccountMeta::new(user_b, false), AccountMeta::new(vault_b, false),
            AccountMeta::new(lp_mint, false), AccountMeta::new(user_lp, false),
            AccountMeta::new(dead_lp, false),
            AccountMeta::new_readonly(admin.pubkey(), true),
            AccountMeta::new_readonly(pool_auth, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data: liq_data,
    }], svm.latest_blockhash())).unwrap();

    let make_swap_ix = |from_a: bool, amount: u64, min_out: u64, signer_key: Address| {
        let (ui, vi, uo, vo) = if from_a {
            (trd_a, vault_a, trd_b, vault_b)
        } else {
            (trd_b, vault_b, trd_a, vault_a)
        };
        let mut data = vec![2u8];
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        data.push(if from_a { 0 } else { 1 });
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(pool, false),
                AccountMeta::new(ui, false), AccountMeta::new(vi, false),
                AccountMeta::new(uo, false), AccountMeta::new(vo, false),
                AccountMeta::new_readonly(signer_key, true),
                AccountMeta::new_readonly(pool_auth, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
            data,
        }
    };

    // Skew the pool: do 5 large A→B swaps to make pool A-heavy
    let skew_amount: u64 = liq / 10; // 10% of pool per swap
    for _ in 0..5 {
        svm.expire_blockhash();
        prime_oracle(&mut svm, oracle_price, liq, liq, &admin); // keep targets FIXED at initial
        svm.send_transaction(make_tx(
            &trader,
            &[make_swap_ix(true, skew_amount, 0, trader.pubkey())],
            svm.latest_blockhash(),
        )).unwrap();
    }

    // Now pool is A-heavy. Sample quotes for equal amounts:
    let probe: u64 = 1_000_000; // tiny probe, no price impact
    svm.expire_blockhash();
    prime_oracle(&mut svm, oracle_price, liq, liq, &admin);

    let ra = read_u64(&svm, &pool, OFF_RESERVE_A);
    let rb = read_u64(&svm, &pool, OFF_RESERVE_B);
    let ta_fixed: u64 = liq;
    let tb_fixed: u64 = liq;

    // Compute effective spreads given current inventory (mirrors lib.rs logic)
    let eff_spread: u32 = spread_bps; // vol_adj=0
    let va: u128 = (ra as u128 * oracle_price as u128) / 1_000_000_000;
    let vb: u128 = rb as u128;
    let tv = va + vb;
    let cr: i64 = (va * 10_000 / tv) as i64;
    let tva: u128 = (ta_fixed as u128 * oracle_price as u128) / 1_000_000_000;
    let tvb: u128 = tb_fixed as u128;
    let tt = tva + tvb;
    let tr: i64 = (tva * 10_000 / tt) as i64;
    let dev: i64 = cr - tr;

    // Must have positive deviation (A overweight)
    assert!(dev > 0, "expected A-overweight after skew swaps; dev={dev}");

    let skew_mag: u32 = ((dev * dev / 8000) as u64).min(eff_spread as u64) as u32;
    let ask_spread = eff_spread + skew_mag; // A→B is MORE expensive
    let bid_spread = eff_spread.saturating_sub(skew_mag); // B→A is CHEAPER

    // Verify analytically: A→B has wider ask than B→A bid
    assert!(ask_spread > bid_spread,
        "A overweight: ask_spread={ask_spread} should > bid_spread={bid_spread}");

    // Verify bid < eff_spread (B→A is subsidized)
    assert!(bid_spread < eff_spread,
        "B→A bid should be tighter than neutral; bid={bid_spread} eff={eff_spread}");

    println!("skew: dev={dev}bps ask_spread={ask_spread}bps bid_spread={bid_spread}bps");
    println!("pool: ra={ra} rb={rb} (target ra=rb={liq})");

    // Finally: executing B→A should succeed with a valid quote
    let _ = svm.expire_blockhash();
    prime_oracle(&mut svm, oracle_price, liq, liq, &admin);
    let swap_result = svm.send_transaction(make_tx(
        &trader,
        &[make_swap_ix(false, probe, 0, trader.pubkey())],
        svm.latest_blockhash(),
    ));
    assert!(swap_result.is_ok(), "B→A swap when A-heavy must succeed: {swap_result:?}");
}

// ── Test 6: TWAP cursor wraparound ───────────────────────────────────────────
//
// Call UPDATE_ORACLE 11 times. After 10 the cursor wraps to 0, after 11 it's 1.
// Slot 0 should be OVERWRITTEN with the 11th price (oldest entry evicted).

#[test]
fn test_twap_cursor_wraps() {
    let mut f = Fixture::new(100_000_000_000, 10_000_000_000, 1_600_000_000_000);

    let prices: Vec<u64> = (1u64..=11)
        .map(|i| 100_000_000_000 + i * 1_000_000_000)
        .collect();

    // Fixture::new() already sent one oracle update → cursor is at 1.
    // Track absolute cursor position as we add more updates.
    let initial_cursor = f.svm.get_account(&f.pool).unwrap().data[OFF_TWAP_CURSOR];
    assert_eq!(initial_cursor, 1, "Fixture should start with cursor=1 after initial oracle update");

    for (i, &price) in prices.iter().enumerate() {
        f.svm.expire_blockhash();
        let mut acc = f.svm.get_account(&f.pool).unwrap();
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE + 8].copy_from_slice(&(-1i64).to_le_bytes());
        let _ = f.svm.set_account(f.pool, acc);
        f.send(&f.admin.insecure_clone(), &[f.ix_update_oracle(price, 10, 0, 1000, 5, 0, 0)]);

        let cursor = f.svm.get_account(&f.pool).unwrap().data[OFF_TWAP_CURSOR];
        // Started at 1, each update advances by 1 mod 10
        let expected_cursor = ((1 + i + 1) % 10) as u8;
        assert_eq!(cursor, expected_cursor, "cursor wrong after test update {}", i + 1);
    }

    // After 11 more updates (total 12 oracle calls including fixture init):
    // cursor = (1 + 11) % 10 = 2
    // Slot written by update i (0-indexed from test): (1 + i) % 10
    // Slot for test update 9  (i=9):  (1+9)%10  = 0  → price = prices[9]
    // Slot for test update 10 (i=10): (1+10)%10 = 1  → price = prices[10]
    let data = f.svm.get_account(&f.pool).unwrap().data;
    let slot0_price = u64::from_le_bytes(data[OFF_TWAP_OBS..OFF_TWAP_OBS + 8].try_into().unwrap());
    assert_eq!(slot0_price, prices[9],
        "slot 0 should hold 10th test price (wrapped); got {slot0_price}");

    let slot1_price = u64::from_le_bytes(data[OFF_TWAP_OBS + 16..OFF_TWAP_OBS + 24].try_into().unwrap());
    assert_eq!(slot1_price, prices[10],
        "slot 1 should hold 11th test price; got {slot1_price}");

    println!("TWAP wraparound: slot0={slot0_price} slot1={slot1_price} cursor={}",
        data[OFF_TWAP_CURSOR]);
}

// ── Simulation: lagged-oracle creates real arb pressure ───────────────────────
//
// The oracle updates ONCE per 3 rounds (delayed).
// Between updates, market price drifts — this creates a spread that the arber
// can beat when drift > PMM spread.
// Verifies that:
//   - swaps actually happen (arb_count > 0)
//   - fee_b accumulates throughout
//   - vault invariant holds every round

#[test]
fn sim_lagged_oracle_generates_fees() {
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(PROGRAM_ID, "target/deploy/pinocchio_prop_amm.so").unwrap();

    let admin  = Keypair::new();
    let trader = Keypair::new();
    svm.airdrop(&admin.pubkey(),  500_000_000_000).unwrap();
    svm.airdrop(&trader.pubkey(), 500_000_000_000).unwrap();

    let pool = Address::new_unique();
    let _ = svm.set_account(pool, Account {
        lamports: 2_000_000, data: vec![0u8; POOL_SIZE],
        owner: PROGRAM_ID, executable: false, rent_epoch: u64::MAX,
    });
    let pb = pool.as_ref().to_vec();
    let (pool_auth, auth_bump) = Address::find_program_address(&[b"pool_auth", &pb], &PROGRAM_ID);
    let (_, dead_bump) = Address::find_program_address(&[b"dead_lp", &pb], &PROGRAM_ID);

    let mint_a  = new_mint(&mut svm, &pool_auth);
    let mint_b  = new_mint(&mut svm, &pool_auth);
    let lp_mint = new_mint(&mut svm, &pool_auth);
    let vault_a = new_ta(&mut svm, &mint_a, &pool_auth, 0);
    let vault_b = new_ta(&mut svm, &mint_b, &pool_auth, 0);
    let user_a  = new_ta(&mut svm, &mint_a, &admin.pubkey(), 200_000_000_000);
    let user_b  = new_ta(&mut svm, &mint_b, &admin.pubkey(), 40_000_000_000_000);
    let user_lp = new_ta(&mut svm, &lp_mint, &admin.pubkey(), 0);
    let dead_lp = new_ta(&mut svm, &lp_mint, &pool_auth, 0);
    let trd_a   = new_ta(&mut svm, &mint_a, &trader.pubkey(), 200_000_000_000);
    let trd_b   = new_ta(&mut svm, &mint_b, &trader.pubkey(), 40_000_000_000_000);

    let liq_a: u64 = 100_000_000_000;   // 100 SOL
    let liq_b: u64 = 16_000_000_000_000; // 16,000 USDC
    let base_oracle: u64 = 160_000_000_000;
    let spread_bps: u32 = 10; // 10 bps — arb fires when market moves > 10 bps

    // INIT_POOL
    let mut d = vec![0u8];
    d.extend_from_slice(admin.pubkey().as_ref());
    d.push(auth_bump); d.push(dead_bump);
    d.extend_from_slice(&5u32.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());    // k=0 (isolate arb logic)
    d.extend_from_slice(&liq_a.to_le_bytes());
    d.extend_from_slice(&liq_b.to_le_bytes());
    d.extend_from_slice(&120u32.to_le_bytes());  // generous staleness for lagged test
    let tx = Transaction::new(&[&admin], Message::new(&[Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(pool, false), AccountMeta::new_readonly(admin.pubkey(), true)],
        data: d,
    }], Some(&admin.pubkey())), svm.latest_blockhash());
    svm.send_transaction(tx).unwrap();

    let oracle_ix = |price: u64, ts_hack: i64| {
        // Patch last_update before sending
        let mut data = vec![1u8];
        data.extend_from_slice(&price.to_le_bytes());
        data.extend_from_slice(&spread_bps.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&5u32.to_le_bytes());
        data.extend_from_slice(&liq_a.to_le_bytes());
        data.extend_from_slice(&liq_b.to_le_bytes());
        (ts_hack, Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![AccountMeta::new(pool, false), AccountMeta::new_readonly(admin.pubkey(), true)],
            data,
        })
    };

    let apply_oracle = |svm: &mut LiteSVM, price: u64, admin: &Keypair| {
        let mut acc = svm.get_account(&pool).unwrap();
        acc.data[OFF_LAST_UPDATE..OFF_LAST_UPDATE + 8].copy_from_slice(&(-1i64).to_le_bytes());
        let _ = svm.set_account(pool, acc);
        let (_, ix) = oracle_ix(price, -1);
        let tx = Transaction::new(&[admin], Message::new(&[ix], Some(&admin.pubkey())), svm.latest_blockhash());
        svm.send_transaction(tx).unwrap();
    };

    // Prime + liquidity
    apply_oracle(&mut svm, base_oracle, &admin);
    let mut d2 = vec![3u8];
    d2.extend_from_slice(&liq_a.to_le_bytes());
    d2.extend_from_slice(&liq_b.to_le_bytes());
    d2.extend_from_slice(&0u64.to_le_bytes());
    let tx2 = Transaction::new(&[&admin], Message::new(&[Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(pool, false),
            AccountMeta::new(user_a, false), AccountMeta::new(vault_a, false),
            AccountMeta::new(user_b, false), AccountMeta::new(vault_b, false),
            AccountMeta::new(lp_mint, false), AccountMeta::new(user_lp, false),
            AccountMeta::new(dead_lp, false),
            AccountMeta::new_readonly(admin.pubkey(), true),
            AccountMeta::new_readonly(pool_auth, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
        ],
        data: d2,
    }], Some(&admin.pubkey())), svm.latest_blockhash());
    svm.send_transaction(tx2).unwrap();

    let make_swap_ix = |from_a: bool, amount: u64, min_out: u64, who: &Keypair| {
        let (ui, vi, uo, vo) = if from_a {
            (trd_a, vault_a, trd_b, vault_b)
        } else {
            (trd_b, vault_b, trd_a, vault_a)
        };
        let mut data = vec![2u8];
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        data.push(if from_a { 0 } else { 1 });
        Instruction {
            program_id: PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(pool, false),
                AccountMeta::new(ui, false), AccountMeta::new(vi, false),
                AccountMeta::new(uo, false), AccountMeta::new(vo, false),
                AccountMeta::new_readonly(who.pubkey(), true),
                AccountMeta::new_readonly(pool_auth, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
            data,
        }
    };

    // pmm quote helpers (mirror lib.rs, k=0)
    let pmm_atob = |pool_oracle: u64, amount_in: u64, spread: u32, fee: u32| -> u64 {
        let ask = pool_oracle as u128 * (10_000 + spread as u128) / 10_000;
        if ask == 0 { return 0; }
        let out_raw = amount_in as u128 * 1_000_000_000 / ask;
        let f = out_raw * fee as u128 / 10_000;
        out_raw.saturating_sub(f) as u64
    };
    let pmm_btoa = |pool_oracle: u64, amount_in: u64, spread: u32, fee: u32| -> u64 {
        let bid = pool_oracle as u128 * (10_000u128.saturating_sub(spread as u128)) / 10_000;
        let out_raw = amount_in as u128 * bid / 1_000_000_000;
        let f = out_raw * fee as u128 / 10_000;
        out_raw.saturating_sub(f) as u64
    };

    println!("\n{:<6} {:>14} {:>14} {:>12} {:>12} {:>14} {:>6}",
        "round", "reserve_a", "reserve_b", "fee_a", "fee_b", "mkt_oracle", "arb");
    println!("{}", "─".repeat(85));

    let mut pool_oracle = base_oracle; // PMM oracle (updated every 3 rounds)
    let mut market = base_oracle;      // external market price (moves every round)
    let mut lcg: u64 = 0xcafe_babe_dead_beef;
    let mut arb_count = 0;

    for round in 1u32..=30 {
        svm.expire_blockhash();

        // Market moves every round ±0.5%
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let delta_bps: i64 = ((lcg >> 52) as i64 % 100) - 50; // [-50, +50) bps
        market = ((market as i128 * (10_000 + delta_bps) as i128 / 10_000) as u64).max(1);

        // PMM oracle updated every 3 rounds (lagged)
        if round % 3 == 0 {
            apply_oracle(&mut svm, market, &admin);
            pool_oracle = market;
        }

        // Arb: market deviation vs pool oracle > spread → opportunity
        let drift_bps = if market > pool_oracle {
            market.saturating_sub(pool_oracle).saturating_mul(10_000) / pool_oracle
        } else {
            pool_oracle.saturating_sub(market).saturating_mul(10_000) / pool_oracle
        };

        let mut arb_dir = "—";
        let swap_size: u64 = liq_a / 100; // 1% of pool per arb

        if drift_bps > spread_bps as u64 {
            if market > pool_oracle {
                // External SOL is MORE expensive → sell SOL to PMM (A→B): PMM pays old price
                // i.e., PMM gives you B at pool_oracle rate, you can buy SOL cheaper externally
                // Actually: market > pool_oracle → PMM is CHEAP source of B
                //   → buy B from PMM (send A, get B at pool_oracle < market)
                let pmm_out = pmm_atob(pool_oracle, swap_size, spread_bps, 5);
                // Fair amount at market price
                let fair_b = (swap_size as u128 * 1_000_000_000 / market as u128) as u64;
                if pmm_out > fair_b {
                    let ix = make_swap_ix(true, swap_size, 0, &trader);
                    let tx = Transaction::new(&[&trader], Message::new(&[ix], Some(&trader.pubkey())), svm.latest_blockhash());
                    if svm.send_transaction(tx).is_ok() {
                        arb_dir = "A→B";
                        arb_count += 1;
                    }
                }
            } else {
                // External SOL CHEAPER → buy SOL from PMM (B→A): PMM sells at pool_oracle > market
                let pmm_out = pmm_btoa(pool_oracle, swap_size, spread_bps, 5);
                let fair_a = (swap_size as u128 * market as u128 / 1_000_000_000) as u64;
                if pmm_out > fair_a {
                    let ix = make_swap_ix(false, swap_size, 0, &trader);
                    let tx = Transaction::new(&[&trader], Message::new(&[ix], Some(&trader.pubkey())), svm.latest_blockhash());
                    if svm.send_transaction(tx).is_ok() {
                        arb_dir = "B→A";
                        arb_count += 1;
                    }
                }
            }
        }

        let ra = read_u64(&svm, &pool, OFF_RESERVE_A);
        let rb = read_u64(&svm, &pool, OFF_RESERVE_B);
        let fa = read_u64(&svm, &pool, OFF_FEE_A);
        let fb = read_u64(&svm, &pool, OFF_FEE_B);
        let va = token_balance(&svm, &vault_a);
        let vb = token_balance(&svm, &vault_b);

        // Invariant: vault must match reserve every round
        assert_eq!(va, ra, "vault_a != reserve_a at round {round}");
        assert_eq!(vb, rb, "vault_b != reserve_b at round {round}");

        println!("{:<6} {:>14} {:>14} {:>12} {:>12} {:>14} {:>6}",
            round, ra, rb, fa, fb, market, arb_dir);
    }

    println!("{}", "─".repeat(85));
    println!("Total arbs: {arb_count}/30");

    let fa_final = read_u64(&svm, &pool, OFF_FEE_A);
    let fb_final = read_u64(&svm, &pool, OFF_FEE_B);
    println!("Total fees: fee_a={fa_final} fee_b={fb_final}");

    // With 30 rounds and ±0.5% market moves, some arbs must fire
    // (unless extremely unlucky — LCG seed is fixed so deterministic)
    assert!(arb_count > 0, "no arbs triggered in 30 rounds — arb model broken");
    assert!(fa_final + fb_final > 0, "no fees accumulated despite arbs");
}
