# Project Structure (Pinocchio)

- `programs/pinocchio-prop-amm/`
  - `src/`
    - `lib.rs` — Entry point for Pinocchio contract
    - `state.rs` — Pool state, LP token logic
    - `instructions/` — Modular instruction handlers
      - `swap.rs`, `add_liquidity.rs`, `remove_liquidity.rs`, `update_oracle.rs`
  - `tests/` — Integration tests
- `bot/` — Off-chain oracle/bundle bot
- `docs/` — Documentation

See `docs/en/structure.md` for English documentation.