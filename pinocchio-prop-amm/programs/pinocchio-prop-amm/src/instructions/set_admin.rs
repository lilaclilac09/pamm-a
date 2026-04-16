//! Set admin instruction handler for Pinocchio Prop AMM
//!
//! Replaces the admin pubkey in pool state. Only the current admin can call this.
//!
//! Real implementation lives in lib.rs:set_admin().
//! This file documents the account layout and data format.
//!
//! Accounts (2):
//!   0  pool          (writable)
//!   1  current_admin (signer)
//!
//! Data (33 bytes total):
//!   [0]      discriminant = 6
//!   [1..33]  new_admin_pubkey  [u8; 32]
