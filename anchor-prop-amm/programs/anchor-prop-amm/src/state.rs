//! Pool state and LP token logic for Anchor Prop AMM

use anchor_lang::prelude::*;

#[account]
pub struct Pool {
    pub lp_supply: u64,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub oracle_price: u64,
    pub last_update: i64,
}
