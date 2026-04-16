//! Collect fees instruction handler for Pinocchio Prop AMM
//!
//! Transfers accumulated fee_a and fee_b from the vaults to the admin's
//! token accounts, then zeroes fee_a and fee_b in pool state.
//!
//! Real implementation lives in lib.rs:collect_fees().
//! This file documents the account layout and data format.
//!
//! Accounts (7):
//!   0  pool      (writable)
//!   1  vault_a   (writable)
//!   2  admin_a   (writable)  — admin's token-A account
//!   3  vault_b   (writable)
//!   4  admin_b   (writable)  — admin's token-B account
//!   5  admin     (signer)
//!   6  pool_auth (PDA signer, seeds: ["pool_auth", pool_pubkey, bump])
//!
//! Data: 1 byte (discriminant = 5 only, no payload)
