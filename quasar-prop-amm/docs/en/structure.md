# Prop AMM Project Structure (English)

## Directory Layout

```
prop-amm-full/
├── programs/
│   └── prop-amm/
│       ├── Cargo.toml
│       ├── Quasar.toml
│       └── src/
│           ├── lib.rs
│           ├── state.rs
│           └── instructions/
│               ├── swap.rs
│               ├── update_oracle.rs
│               ├── add_liquidity.rs
│               └── remove_liquidity.rs
├── bot/
│   ├── Cargo.toml
│   ├── .env.example
│   └── src/
│       └── main.rs
├── scripts/
│   └── deploy.sh
├── README.md
```

## On-chain Contract
- Pool struct, LP token, PDA
- Swap, update_oracle, add/remove liquidity instructions

## Off-chain Oracle Bot
- Real-time pool monitoring
- Fetch price from Pyth/Chainlink
- Dynamic volatility, spread, skew
- Jito/Harmonic bundle, tip optimization
- Simulate bundle, dual relay

## Optimizations
- LP token share tracking
- PDA auto-creation
- Tip/CU simulation
- Bundle structure: max 5 tx, tip last
- Simulation before sending
