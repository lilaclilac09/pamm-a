use quasar::prelude::*;
use std::collections::BTreeMap;

#[account(zero_copy)]
pub struct Pool {
    pub authority: Pubkey,
    pub reserve_in: u64,
    pub reserve_out: u64,
    pub base_spread: u32,   // Base spread, in bps
    pub vol_factor: u32,    // Volatility factor
    pub skew_factor: u32,   // Inventory skew factor
    pub target_ratio: u64,  // Target inventory ratio (in 1e4)
    pub lp_total_supply: u64, // LP token total supply
    // User LP shares (for zero-copy compatibility, use PDA or separate account in production)
    // pub user_shares: BTreeMap<Pubkey, u64>,
}

impl Pool {
    pub fn current_ratio(&self) -> u64 {
        if self.reserve_in + self.reserve_out == 0 {
            10000
        } else {
            (self.reserve_in * 10000) / (self.reserve_in + self.reserve_out)
        }
    }
}
