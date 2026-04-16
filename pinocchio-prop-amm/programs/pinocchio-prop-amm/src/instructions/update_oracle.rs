//! UPDATE_ORACLE instruction — discriminant 1
//!
//! Sets oracle_price, spread_bps, vol_adj, k_param, fee_bps, and
//! target_a/target_b in pool state. Advances the TWAP ring buffer.
//! Only callable by the admin stored in pool state.
//!
//! Accounts (2):
//!   0  pool   (writable)
//!   1  admin  (signer, must match pool.admin)
//!
//! Data (41 bytes total):
//!   [0]      discriminant = 1
//!   [1..9]   oracle_price  u64 le   (price scaled to 1e9)
//!   [9..13]  spread_bps    u32 le
//!   [13..17] vol_adj       u32 le
//!   [17..21] k_param       u32 le
//!   [21..25] fee_bps       u32 le
//!   [25..33] target_a      u64 le
//!   [33..41] target_b      u64 le
//!
//! Implementation: lib.rs:update_oracle()
