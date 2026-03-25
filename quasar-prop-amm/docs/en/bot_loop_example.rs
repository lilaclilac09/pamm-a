// Example: Off-chain Oracle Bot main loop (English)

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
