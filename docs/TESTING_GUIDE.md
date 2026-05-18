# Unified Testing Guide: Quasar, Pinocchio, Anchor Prop AMM

This guide explains how to build and test all three AMM implementations locally. All steps and comments are in English.

---

## 1. Prerequisites
- Rust toolchain (latest stable)
- Solana CLI tools
- Anchor CLI (for Anchor version)
- LiteSVM (for Quasar/Pinocchio local testing)
- Node.js (for Anchor tests)

---

## 2. Build & Test: Quasar Version
```sh
cd quasar-prop-amm/programs/quasar-prop-amm
cargo build-bpf
# For local simulation (if using LiteSVM):
litesvm run target/deploy/quasar_prop_amm.so
```

---

## 3. Build & Test: Pinocchio Version
```sh
cd pinocchio-prop-amm/programs/pinocchio-prop-amm
cargo build-bpf
# For local simulation (if using LiteSVM):
litesvm run target/deploy/pinocchio_prop_amm.so
```

---

## 4. Build & Test: Anchor Version
```sh
cd anchor-prop-amm/programs/anchor-prop-amm
anchor build
anchor test
# For Anchor localnet:
anchor localnet
```

---

## 5. Off-chain Bot Testing (All Versions)
- Each version has a `bot/` folder with a Rust script for oracle updates and inventory monitoring.
- Set environment variables (e.g., `RPC_URL`, `POOL_PUBKEY`) as needed.
- Run with:
```sh
cd [quasar|pinocchio|anchor]-prop-amm/bot
cargo run
```

---

## 6. Notes
- All code, comments, and logs are in English.
- For LiteSVM, see https://github.com/litesvm/litesvm for install and usage.
- For Anchor, see https://book.anchor-lang.com/ for details.
- If you need example test scripts or want to automate tests, let me know!
