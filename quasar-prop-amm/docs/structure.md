# Prop AMM Project Structure and Design

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

## 2. On-chain Contract (Quasar)
- **state.rs**: Pool struct, LP token state, PDA creation
- **swap.rs**: Dynamic spread + inventory skew logic
- **update_oracle.rs**: Oracle parameter update from bot
- **add_liquidity.rs/remove_liquidity.rs**: LP token mint/burn

## 3. Off-chain Oracle Bot (Rust)
- Real-time pool reserve monitoring (reserve0/reserve1)
- Fetch latest price from Pyth/Chainlink every 3 seconds
- Dynamic volatility, spread, skew calculation
- Build and send update_oracle instruction
- Jito Bundle + Helius Sender (tip optimization, simulateBundle)
- Tip/CU auto adjustment, tip as last bundle instruction
- Dual relay support: Jito + Harmonic

## 4. Key Optimizations
- **LP Token**: Add/remove liquidity, share tracking
- **PDA Creation**: Auto-initialize pool/LP accounts
- **Tip Optimization**: Tip/CU simulation, auto adjustment
- **Bundle Structure**: Max 5 tx, tip last, avoid LUT
- **Simulation First**: simulateBundle to ensure CU/avoid conflicts

## 5. Example Bot Loop (Pseudocode)

```rust
loop {
    // 1. Read pool reserves
    let (reserve0, reserve1) = get_pool_reserves(&client, &pool_pubkey);

    // 2. Fetch latest price
    let price = get_latest_price().await?;

    // 3. Calculate volatility, spread, skew
    let vol = calc_volatility(&price_history);
    let spread = base_spread + vol_factor * vol / 10000;
    let skew = calc_skew(reserve0, reserve1, target_ratio);

    // 4. Build update_oracle instruction
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

For further details or implementation help, see README.md or contact the project maintainer.
