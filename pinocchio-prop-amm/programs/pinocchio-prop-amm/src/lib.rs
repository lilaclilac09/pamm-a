//! Pinocchio Prop AMM entrypoint

mod state;
pub mod instructions;

use pinocchio::prelude::*;

#[program]
pub mod pinocchio_prop_amm {
    use super::*;

    pub fn swap(ctx: Context<Swap>, amount_in: u64) -> Result<()> {
        instructions::swap::handle(ctx, amount_in)
    }

    pub fn add_liquidity(ctx: Context<AddLiquidity>, amount: u64) -> Result<()> {
        instructions::add_liquidity::handle(ctx, amount)
    }

    pub fn remove_liquidity(ctx: Context<RemoveLiquidity>, amount: u64) -> Result<()> {
        instructions::remove_liquidity::handle(ctx, amount)
    }

    pub fn update_oracle(ctx: Context<UpdateOracle>, price: u64) -> Result<()> {
        instructions::update_oracle::handle(ctx, price)
    }
}
