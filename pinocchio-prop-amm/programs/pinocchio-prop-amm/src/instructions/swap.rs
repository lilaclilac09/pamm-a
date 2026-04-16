//! SWAP instruction — discriminant 2
//!
//! AMM swap using PMM pricing formula. Oracle must not be stale.
//! Applies fee_bps, accrues fee_a / fee_b in pool state.
//!
//! Accounts (7):
//!   0  pool      (writable)
//!   1  user_in   (writable)   source token account
//!   2  vault_in  (writable)   pool vault for input token
//!   3  user_out  (writable)   destination token account
//!   4  vault_out (writable)   pool vault for output token
//!   5  user      (signer)
//!   6  pool_auth (PDA signer, seeds: ["pool_auth", pool_pubkey, bump])
//!
//! Data (18 bytes total):
//!   [0]      discriminant = 2
//!   [1..9]   amount_in    u64 le
//!   [9..17]  min_out      u64 le   slippage guard
//!   [17]     direction    u8       0 = A→B, 1 = B→A
//!
//! Implementation: lib.rs:swap()
