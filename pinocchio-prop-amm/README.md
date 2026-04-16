# Pinocchio Prop AMM

A PMM (Proactive Market Maker) on Solana. The on-chain program manages a two-token pool with oracle-driven pricing. The off-chain bot keeps the oracle price fresh, rebalances inventory, and captures spread via Jito bundles.

## How it works

The pool uses an oracle mid-price as the fair value reference instead of a pure constant-product formula. Spreads widen automatically when Pyth confidence or historical volatility is high. When inventory drifts too far from target, the bot adds or removes liquidity rather than just swapping.

## Structure

```
programs/pinocchio-prop-amm/   On-chain program (Pinocchio 0.10)
  src/lib.rs                   7 instructions: INIT_POOL, UPDATE_ORACLE, SWAP,
                               ADD_LIQUIDITY, REMOVE_LIQUIDITY, COLLECT_FEES, SET_ADMIN
  src/state.rs                 Pool state layout (304 bytes), constants, MAGIC
  src/instructions/            Account layout + data format docs for each instruction
  tests/test_basic.rs          16 unit tests (litesvm)

bot/                           Off-chain Rust bot
  src/main.rs                  Entry point — 3 tokio tasks + graceful shutdown
  src/oracle_arm.rs            Pyth fetch → UPDATE_ORACLE every UPDATE_INTERVAL_MS
  src/trading_arm.rs           Jupiter swaps + ADD/REMOVE rebalancing + Jito bundles
  src/risk.rs                  Shared state, TWAP sanity filter, circuit breaker
  src/metrics.rs               HTTP /health + /metrics (JSON) on METRICS_PORT
  src/config.rs                All config from environment variables
  src/pyth.rs                  Hermes price fetch, historical volatility estimate
  .env.example                 All env vars documented with defaults

scripts/                       Node.js admin scripts
  init-pool.js                 Deploy and initialize a new pool
  add-liquidity.js             Deposit tokens, receive LP
  swap.js                      Execute a swap
  remove-liquidity.js          Burn LP, withdraw tokens
  collect-fees.js              Sweep accumulated fees to admin wallets
  update-oracle.js             Manually set oracle price (without running the bot)
  status.js                    Pretty-print all pool state (reserves, TWAP, fees, etc.)
```

## Quick start

### 1. Deploy the program

```bash
cd programs/pinocchio-prop-amm
cargo build-sbf
solana program deploy target/deploy/pinocchio_prop_amm.so
```

### 2. Initialize a pool

```bash
cd scripts
cp ../bot/.env.example ../bot/.env
# Fill in PRIVATE_KEY, PROGRAM_ID, MINT_A, MINT_B in .env
node init-pool.js
# Saves pool-state.json with all account addresses
```

### 3. Add liquidity

```bash
node add-liquidity.js
```

### 4. Configure and run the bot

```bash
cd ../bot
# Fill in all remaining fields in .env (see .env.example for full docs)
# Minimum required: PRIVATE_KEY, PROGRAM_ID, POOL_PUBKEY, POOL_AUTH,
#   VAULT_A, VAULT_B, LP_MINT, MINT_A, MINT_B, USER_A, USER_B, USER_LP, DEAD_LP_ACCOUNT
cargo run --release
```

The bot logs each oracle update and trade action. Check health at:
```
curl http://localhost:3000/health
curl http://localhost:3000/metrics
```

Ctrl-C for graceful shutdown (waits up to 10s for in-flight transactions).

### 5. Collect fees

```bash
cd scripts
node collect-fees.js
```

## On-chain program

Pool state is 304 bytes. Key byte offsets:

| Field | Offset | Type |
|---|---|---|
| magic (`PMMA`) | 0 | u32 |
| admin | 8 | [u8;32] |
| reserve_a | 48 | u64 |
| reserve_b | 56 | u64 |
| lp_supply | 80 | u64 |
| oracle_price (1e9) | 104 | u64 |
| spread_bps | 112 | u32 |
| last_oracle_update | 128 | i64 |
| twap_obs (10 slots) | 144 | [(u64,i64); 10] |

## Tests

```bash
cd programs/pinocchio-prop-amm
cargo test
# 16/16 pass — covers PMM math, LP mint, oracle validation, TWAP staleness
```

To rebuild the `.so` after changing `lib.rs`:
```bash
cargo build-sbf
```