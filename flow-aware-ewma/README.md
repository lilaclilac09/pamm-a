# flow-aware-ewma

Reference implementation of **EWMA Dynamic Fee v3** — extends [`src/lib.rs`](../src/lib.rs) with an off-chain "flow pressure" signal derived from the prop-AMM radar.

This is **not** the competition submission — `src/` stays locked. v3 is the next iteration if you want to deploy your own pool with the flow hook wired up.

## The signal

Many prop AMMs reverse-engineered in [`references/solana/prop-amm/`](../references/solana/prop-amm/) compete for the same retail flow. When *several* of them simultaneously list the same mint pair, that pair is likely seeing concentrated informed/toxic flow — exactly the kind of trade you want to charge more for.

```
flow_pressure_1e9 = clamp(scale * log(1 + n_amms_on_pair), 0, 3e6)
```

Where `n_amms_on_pair` comes from the latest snapshot in
`references/solana/prop-amm/snapshots/active-markets-latest.json`.
The computation lives off-chain in
[`jupiter-mm-bot/src/prop-amm-signal.ts`](../jupiter-mm-bot/src/prop-amm-signal.ts).

## Wire-up sequence

1. **Off-chain (every ~6s):** bot reads latest snapshot, computes
   `flow_pressure_1e9` for the pair it's currently quoting, and sends a
   `set_flow_pressure` (tag `5`) instruction to the on-chain program.
2. **On-chain (each swap):** `compute_swap` adds
   `flow_pressure * FLOW_MULT` (≤ 12 bps) to the fee.
3. **Staleness:** if the bot hasn't refreshed within `FLOW_STALE_SLOTS` (200
   slots ≈ 80s), `after_swap` zeros the pressure so a paused bot doesn't
   strand the floor.

## Storage diff vs v2

```
[0..8]   ewma_vol            (unchanged)
[8..16]  last_rx             (unchanged)
[16..24] last_ry             (unchanged)
[24..32] shock_steps         (unchanged)
[32..40] flow_pressure_1e9   ← new
[40..48] flow_last_set_slot  ← new
[48..]   reserved
```

## Why a separate dir, not a patch

The competition submission in `src/` is frozen — modifying it risks breaking the
submission contract. Keeping v3 here lets:

- swap-compare benchmark v2 vs v3 head-to-head
- reviewers compare diffs in isolation
- the dashboard radar drive a "what if" simulation without touching live code
