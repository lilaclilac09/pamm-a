use quasar::prelude::*;
use crate::state::Pool;

#[derive(Accounts)]
pub struct Swap<'info> {
    #[account(mut)]
    pub pool: Account<'info, Pool>,
    // Extend with user, vault, etc. accounts as needed
}

#[quasar::instruction]
pub fn swap(ctx: Context<Swap>, amount_in: u64, min_out: u64) -> Result<()> {
    let pool = &mut ctx.accounts.pool;

    // Dynamic spread + inventory skew
    let volatility = 4500u32; // Example: replace with actual volatility
    let effective_spread = pool.base_spread + (pool.vol_factor * volatility / 10000);

    let current_ratio = pool.current_ratio();
    let deviation = current_ratio as i64 - pool.target_ratio as i64;
    let skew_adj = (deviation * deviation / 5000) * pool.skew_factor as i64 / 10000;
    let final_spread = (effective_spread as i64 + skew_adj).max(0) as u32;

    // Calculate output
    let base_out = (amount_in * pool.reserve_out) / (pool.reserve_in + amount_in);
    let spread_adj = (base_out * final_spread as u64) / 10000;
    let amount_out = base_out - spread_adj;

    require!(amount_out >= min_out, ErrorCode::SlippageExceeded);

    // TODO: Token CPI transfer logic here

    // Update reserves
    pool.reserve_in = pool.reserve_in.checked_add(amount_in).unwrap();
    pool.reserve_out = pool.reserve_out.checked_sub(amount_out).unwrap();

    Ok(())
}

#[error_code]
pub enum ErrorCode {
    #[msg("Slippage check failed")] 
    SlippageExceeded,
}
