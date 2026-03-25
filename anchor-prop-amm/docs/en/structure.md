# Project Structure (English)

## On-chain (Anchor)
- `programs/anchor-prop-amm/`
  - `src/lib.rs`: Anchor contract entry
  - `src/state.rs`: Pool state, LP token logic
  - `src/instructions/`: Modular instructions (swap, add/remove liquidity, update_oracle)
  - `tests/`: Integration tests

## Off-chain
- `bot/`: Oracle/bundle bot (Rust)

## Documentation
- `docs/`: Structure, optimizations, bot loop examples
