use quasar::prelude::*;
mod state;
pub mod instructions;

quasar::program!(
    name = "quasar-prop-amm",
    instructions = [
        instructions::swap::swap,
        instructions::add_liquidity::add_liquidity,
        instructions::remove_liquidity::remove_liquidity,
    ]
);
