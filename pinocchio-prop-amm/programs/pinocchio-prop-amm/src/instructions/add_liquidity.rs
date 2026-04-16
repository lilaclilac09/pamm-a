//! ADD_LIQUIDITY instruction — discriminant 3
//!
//! Deposits token A and B proportionally, mints LP tokens to the user.
//! First deposit mints MINIMUM_LIQUIDITY=1000 to dead_lp_account to prevent
//! LP manipulation attacks.
//!
//! Accounts (10):
//!   0  pool           (writable)
//!   1  user_a         (writable)   user's token-A account
//!   2  vault_a        (writable)
//!   3  user_b         (writable)   user's token-B account
//!   4  vault_b        (writable)
//!   5  lp_mint        (writable)
//!   6  user_lp        (writable)   user's LP token account
//!   7  dead_lp_account(writable)   ATA of pool_auth for LP mint
//!   8  user           (signer)
//!   9  pool_auth      (PDA signer, seeds: ["pool_auth", pool_pubkey, bump])
//!
//! Data (25 bytes total):
//!   [0]      discriminant = 3
//!   [1..9]   amount_a     u64 le
//!   [9..17]  amount_b     u64 le
//!   [17..25] min_lp_out   u64 le
//!
//! Implementation: lib.rs:add_liquidity()
