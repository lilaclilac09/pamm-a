//! REMOVE_LIQUIDITY instruction — discriminant 4
//!
//! Burns LP tokens and withdraws proportional token A and B from vaults.
//!
//! Accounts (9):
//!   0  pool      (writable)
//!   1  user_lp   (writable)   user's LP token account
//!   2  lp_mint   (writable)
//!   3  vault_a   (writable)
//!   4  user_a    (writable)
//!   5  vault_b   (writable)
//!   6  user_b    (writable)
//!   7  user      (signer)
//!   8  pool_auth (PDA signer, seeds: ["pool_auth", pool_pubkey, bump])
//!
//! Data (25 bytes total):
//!   [0]      discriminant = 4
//!   [1..9]   lp_amount    u64 le   LP tokens to burn
//!   [9..17]  min_a_out    u64 le   slippage guard
//!   [17..25] min_b_out    u64 le   slippage guard
//!
//! Implementation: lib.rs:remove_liquidity()
