pub fn remove_liquidity(ctx: Context<RemoveLiquidity>, lp_amount: u64) -> Result<()> {
use quasar::prelude::*;
use crate::state::Pool;

#[derive(Accounts)]
pub struct RemoveLiquidity<'info> {
    #[account(mut)]
    pub pool: Account<'info, Pool>,
    // pub user: Signer<'info>,
    // pub user_lp_token: Account<'info, TokenAccount>,
    // pub pool_token_a: Account<'info, TokenAccount>,
    // pub pool_token_b: Account<'info, TokenAccount>,
    // pub user_token_a: Account<'info, TokenAccount>,
    // pub user_token_b: Account<'info, TokenAccount>,
    // pub lp_mint: Account<'info, Mint>,
    // Omitted detailed accounts, fill in for deployment
}

#[quasar::instruction]
pub fn remove_liquidity(ctx: Context<RemoveLiquidity>, lp_amount: u64) -> Result<()> {
    let pool = &mut ctx.accounts.pool;
    // 1. Calculate token_a, token_b to return (proportional to share)
    // 2. Burn user's LP tokens
    // 3. CPI transfer pool assets to user
    // 4. Update pool reserves
    // Pseudocode skeleton only
    Ok(())
}
