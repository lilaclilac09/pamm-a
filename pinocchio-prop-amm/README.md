# Pinocchio Prop AMM

This project implements a production-ready Prop AMM (Automated Market Maker) using the Pinocchio framework for Solana. It includes both on-chain smart contracts and an off-chain oracle/bot for real-time price updates, bundle submission, and advanced features like dynamic spread and inventory skew.

## Structure
- `programs/pinocchio-prop-amm/` — On-chain contract (Pinocchio)
  - `src/` — Rust contract source code
  - `src/instructions/` — Modular instruction handlers (swap, add/remove liquidity, update_oracle)
  - `src/state.rs` — Pool state, LP token logic
  - `tests/` — Integration tests
- `bot/` — Off-chain Rust oracle/bundle bot
- `docs/` — Documentation

## Features
- Dynamic spread and inventory skew
- Real-time oracle updates (Pyth/Chainlink)
- Jito/Harmonic bundle submission
- Dual-relay, tip/CU optimization
- Simulation-first workflow
- Modular, well-documented code

## Documentation
See `docs/` and `docs/en/` for structure, optimizations, and bot loop examples.

---

**Note:** This is the Pinocchio version. For the Quasar version, see the `quasar-prop-amm` folder.