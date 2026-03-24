# Prop AMM Project Structure and Implementation Guide

This document provides a detailed overview of the Prop AMM project, including both on-chain (Quasar) and off-chain (Rust Oracle Bot) components, best practices, and development recommendations.

---

## 1. Directory Structure

```
prop-amm-full/
├── programs/
│   └── prop-amm/                  # Quasar on-chain contract
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
├── bot/                           # Off-chain Oracle Bot
│   ├── Cargo.toml
│   ├── .env.example
│   └── src/
│       └── main.rs
├── scripts/
│   └── deploy.sh
├── README.md
```

---

## 2. On-chain Contract (Quasar)

- **state.rs**: Pool structure, LP token state, PDA creation
- **swap.rs**: Dynamic spread + inventory skew logic
- **update_oracle.rs**: Oracle parameter updates from off-chain bot
- **add_liquidity.rs / remove_liquidity.rs**: LP token mint/burn logic

---

## 3. Off-chain Oracle Bot (Rust)

- Real-time reading of on-chain pool reserves (reserve0/reserve1)
- Fetching latest price from Pyth/Chainlink every 3 seconds
- Dynamic calculation of volatility, spread, and skew parameters
- Building and sending UpdateOracle instructions
- Jito Bundle + Helius Sender for prioritized submission (tip optimization, simulateBundle before send)
- Tip/CU ratio auto-adjustment, tip instruction as last transaction in bundle
- Simultaneous submission to Jito and Harmonic relays

---

## 4. Key Optimizations

- **LP Token**: Add/Remove liquidity, share tracking
- **PDA Creation**: Auto-initialize pool/LP accounts
- **Tip Optimization**: Simulate and auto-adjust tip/CU
- **Bundle Structure**: Max 5 transactions, tip last, avoid LUT
- **Simulation First**: Use simulateBundle to ensure CU and no conflicts

---

## 5. Example Off-chain Bot Loop (Pseudocode)

```rust
loop {
    // 1. Read on-chain pool reserves
    let (reserve0, reserve1) = get_pool_reserves(&client, &pool_pubkey);

    // 2. Fetch latest price from Pyth/Chainlink
    let price = get_latest_price().await?;

    // 3. Calculate volatility, spread, skew
    let vol = calc_volatility(&price_history);
    let spread = base_spread + vol_factor * vol / 10000;
    let skew = calc_skew(reserve0, reserve1, target_ratio);

    // 4. Build UpdateOracle instruction
    let ix = build_update_oracle_ix(...);

    // 5. Build tip instruction
    let tip_ix = build_tip_ix(tip_lamports);

    // 6. Build bundle (main tx + tip, max 5, tip last)
    let bundle = build_bundle(vec![ix, tip_ix]);

    // 7. Simulate bundle for CU
    if simulate_bundle(&bundle) {
        // 8. Send to Jito + Harmonic
        send_bundle(&bundle, jito_url);
        send_bundle(&bundle, harmonic_url);
    }

    sleep(Duration::from_secs(3)).await;
}
```

---

## 6. Next Steps

- Fill in the actual implementation for price feeds, volatility, and bundle submission
- Use this structure as a starting point for further development and optimization
- For more details, see README.md or ask for specific code examples
