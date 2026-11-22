# Anchor Prop AMM

Status: LOCAL ONLY — research implementation, not verified production-ready software; next human action: review the Anchor prerequisites and validate the selected flows in a controlled local environment before any deployment.

This project contains a research Prop AMM (Automated Market Maker) implementation using the Anchor framework for Solana. It includes on-chain smart contracts and an off-chain oracle/bot for price updates, bundle submission, and features like dynamic spread and inventory skew. Production readiness has not been established by this audit.

## Structure
- `programs/anchor-prop-amm/` — On-chain contract (Anchor)
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

**Note:** This is the Anchor version. For Quasar and Pinocchio versions, see their respective folders.