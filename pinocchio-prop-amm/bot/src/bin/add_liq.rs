//! One-shot devnet liquidity injection.
//!
//! Reads current pool reserves, mints 50x their amounts to user_a / user_b
//! (we are the mint authority on devnet), then calls ADD_LIQUIDITY.
//!
//! Run from pinocchio-prop-amm/bot/ with:
//!   http_proxy=... https_proxy=... cargo run --bin add_liq

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
    transaction::Transaction,
};
use std::str::FromStr;

// ── State offsets (must match lib.rs / state.rs) ──────────────────────────────
const OFF_RESERVE_A: usize = 48;
const OFF_RESERVE_B: usize = 56;
const OFF_LP_SUPPLY: usize = 64;

const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

fn main() -> Result<()> {
    dotenv::dotenv().ok();

    // ── Load config ───────────────────────────────────────────────────────────
    let key_str = std::env::var("PRIVATE_KEY").context("PRIVATE_KEY")?;
    let key_bytes = bs58::decode(&key_str).into_vec().context("base58 decode")?;
    let wallet = solana_sdk::signature::Keypair::from_bytes(&key_bytes)
        .context("keypair from bytes")?;

    let p = |name: &str| -> Result<Pubkey> {
        Pubkey::from_str(&std::env::var(name).context(format!("{name} not set"))?)
            .context(format!("invalid pubkey {name}"))
    };

    let rpc_url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "https://api.devnet.solana.com".into());
    let program_id   = p("PROGRAM_ID")?;
    let pool_pubkey  = p("POOL_PUBKEY")?;
    let pool_auth    = p("POOL_AUTH")?;
    let vault_a      = p("VAULT_A")?;
    let vault_b      = p("VAULT_B")?;
    let lp_mint      = p("LP_MINT")?;
    let mint_a       = p("MINT_A")?;
    let mint_b       = p("MINT_B")?;
    let user_a       = p("USER_A")?;
    let user_b       = p("USER_B")?;
    let user_lp      = p("USER_LP")?;
    let dead_lp      = p("DEAD_LP_ACCOUNT")?;
    let pool_auth_bump: u8 = std::env::var("POOL_AUTH_BUMP")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(255);

    let client = RpcClient::new(rpc_url);
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID).unwrap();

    // ── Read pool state ───────────────────────────────────────────────────────
    println!("reading pool state...");
    let pool_acc = client.get_account(&pool_pubkey).context("get pool account")?;
    let d = &pool_acc.data;
    let reserve_a  = read_u64(d, OFF_RESERVE_A);
    let reserve_b  = read_u64(d, OFF_RESERVE_B);
    let lp_supply  = read_u64(d, OFF_LP_SUPPLY);

    println!("  reserve_a  = {reserve_a}");
    println!("  reserve_b  = {reserve_b}");
    println!("  lp_supply  = {lp_supply}");

    if reserve_a == 0 || reserve_b == 0 {
        anyhow::bail!("pool has zero reserves — cannot compute proportional amounts");
    }

    // ── Compute injection amounts (50× current reserves) ─────────────────────
    // This keeps the pool ratio unchanged while giving the trading arm plenty
    // of buffer. At oracle ~$85/SOL with 6-decimal mints, 50× reserve_a gives
    // ~39M A-units which comfortably covers max_trade_lamports=100_000 B→A.
    let multiplier: u64 = 50;
    let add_a = reserve_a.saturating_mul(multiplier);
    let add_b = reserve_b.saturating_mul(multiplier);

    println!("injecting: add_a={add_a}  add_b={add_b}  (×{multiplier} current reserves)");

    // ── Mint A → user_a ───────────────────────────────────────────────────────
    let bh = client.get_latest_blockhash().context("get_latest_blockhash")?;
    println!("minting {add_a} A to user_a...");
    let mint_a_ix = mint_to_ix(&token_program, &mint_a, &user_a, &wallet.pubkey(), add_a);
    let tx = Transaction::new_signed_with_payer(
        &[mint_a_ix], Some(&wallet.pubkey()), &[&wallet], bh,
    );
    let sig = client.send_and_confirm_transaction(&tx).context("mint A")?;
    println!("  mint A sig: {sig}");

    // ── Mint B → user_b ───────────────────────────────────────────────────────
    let bh = client.get_latest_blockhash().context("get_latest_blockhash")?;
    println!("minting {add_b} B to user_b...");
    let mint_b_ix = mint_to_ix(&token_program, &mint_b, &user_b, &wallet.pubkey(), add_b);
    let tx = Transaction::new_signed_with_payer(
        &[mint_b_ix], Some(&wallet.pubkey()), &[&wallet], bh,
    );
    let sig = client.send_and_confirm_transaction(&tx).context("mint B")?;
    println!("  mint B sig: {sig}");

    // ── ADD_LIQUIDITY ─────────────────────────────────────────────────────────
    // Accounts (11): pool, user_a, vault_a, user_b, vault_b,
    //                lp_mint, user_lp, dead_lp_account,
    //                user(signer), pool_auth, TOKEN_PROGRAM
    // Data (25 bytes): [3] + amount_a(8) + amount_b(8) + min_lp_out(8)
    let bh = client.get_latest_blockhash().context("get_latest_blockhash")?;
    println!("calling add_liquidity...");

    let mut data = Vec::with_capacity(25);
    data.push(3u8);
    data.extend_from_slice(&add_a.to_le_bytes());
    data.extend_from_slice(&add_b.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // min_lp_out = 0

    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(pool_pubkey,   false),
            AccountMeta::new(user_a,        false),
            AccountMeta::new(vault_a,       false),
            AccountMeta::new(user_b,        false),
            AccountMeta::new(vault_b,       false),
            AccountMeta::new(lp_mint,       false),
            AccountMeta::new(user_lp,       false),
            AccountMeta::new(dead_lp,       false),
            AccountMeta::new_readonly(wallet.pubkey(), true),
            AccountMeta::new_readonly(pool_auth, false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data,
    };

    // Prepend compute budget: 400_000 CUs (add_liquidity does 3 CPIs)
    let cu_ix = solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000);
    let tx = Transaction::new_signed_with_payer(
        &[cu_ix, ix], Some(&wallet.pubkey()), &[&wallet], bh,
    );
    let sig = client.send_and_confirm_transaction(&tx).context("add_liquidity")?;
    println!("  add_liquidity sig: {sig}");

    // ── Verify new reserves ───────────────────────────────────────────────────
    let pool_acc2 = client.get_account(&pool_pubkey).context("re-read pool")?;
    let d2 = &pool_acc2.data;
    let new_a = read_u64(d2, OFF_RESERVE_A);
    let new_b = read_u64(d2, OFF_RESERVE_B);
    let new_lp = read_u64(d2, OFF_LP_SUPPLY);
    println!("\npool after injection:");
    println!("  reserve_a  = {new_a}  (+{})", new_a - reserve_a);
    println!("  reserve_b  = {new_b}  (+{})", new_b - reserve_b);
    println!("  lp_supply  = {new_lp}");
    println!("\ndone — trading_arm should now be able to execute swaps");

    // Sanity: print how big a B→A swap the pool can now handle
    // expected_a_per_100k_b = 100_000 * oracle_price / 1e9
    // But we don't have oracle here; just check raw reserve_a headroom
    println!("  reserve_a headroom for B→A swaps: {new_a} A-units");

    // suppress unused var warning for pool_auth_bump (used in add_liquidity CPI by program)
    let _ = pool_auth_bump;

    Ok(())
}

// ── MintTo instruction (Token Program discriminant = 7) ───────────────────────
fn mint_to_ix(
    token_program: &Pubkey,
    mint:          &Pubkey,
    destination:   &Pubkey,
    authority:     &Pubkey,
    amount:        u64,
) -> Instruction {
    let mut data = vec![7u8]; // MintTo discriminant
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: *token_program,
        accounts: vec![
            AccountMeta::new(*mint,        false),
            AccountMeta::new(*destination, false),
            AccountMeta::new_readonly(*authority, true),
        ],
        data,
    }
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
