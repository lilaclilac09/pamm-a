# Humidifi, Decoded — Part II findings

Companion notes for the published article at <https://aileena.xyz/blog/humidifi-decoded>. This file holds the raw technical findings; the article is the readable version.

## Scope

Part I of [The Pool That Wasn't a Pool](./blog-prop-amm-dict.md) established that humidifi's 1728-byte accounts don't store mint pubkeys. Part II goes back in and maps what *is* in those bytes.

## Reproducibility

Two scripts, public RPC, ~3 minutes wall time:

```bash
python3 scripts/humidifi-watch.py --mode record \
  --pool FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH \
  --duration 120 --interval 2.0 --out trace.jsonl
python3 scripts/humidifi-decode.py --input trace.jsonl
```

## Sampling note (the gotcha)

Most humidifi pool accounts are dormant. The pool used in Part I, `41cK7v1uJYpQ69xP31kU75hzxBHmvpZUeDnopsL2duRN`, had not moved in 166 days at the time of Part II. Sample currently-active pools by walking program-level `getSignaturesForAddress` and counting which pool pubkey each tx's humidifi instruction touches first.

Active pools observed during the Part II recording window:

| pool | role |
|---|---|
| `FksffEqnBRixYGR791Qw2MgdU7zNCpHVFYBL4Fa4qVuH` | primary recording target |
| `DB3sUCP2H4icbeKmK6yb6nUxU5ogbcRHtGuq7W2RoRwW` | cross-validation |

## Byte map

Across 60 snapshots (2-second cadence) on the primary pool, six contiguous ranges showed any change. Total live bytes: **57 of 1728 (3.3%)**.

| range | width | avg changes / snap |
|---|---:|---:|
| 576 – 580 | 5  | 0.81 |
| 600 – 603 | 4  | 0.78 |
| 616 – 617 | 2  | 0.52 |
| 624 – 660 | 37 | 0.37 |
| 672 – 676 | 5  | 0.35 |
| 680 – 683 | 4  | 0.40 |

Reproduced on the second pool with 6/6 of the same ranges plus 2 additional smaller ranges (688, 800-801) that look instrument-class specific.

## Structural classification per range

For each range, every u8 / u16-LE / u32-LE / u64-LE offset was classified by its time-series pattern. Top picks:

| offset | width | class | examples |
|---:|---:|---|---|
| 616 | u16 LE | smooth (price-like) | `[26416, 26423, 26376, 26381, 26374, …]` |
| 675 | u16 LE | smooth (price-like) | `[28110, 28099, 28037, 28037, 28064, …]` |
| 657 | u32 LE | monotonic ↑ | `[1560757277, 1573412782, 1575438882, …]` |
| 653 | u64 LE | monotonic ↑, e18 | `[6703401462834817804, 6757756443098909634, 6766458476050285304, …]` |
| 660 | u8     | constant (93) | flag / sentinel |
| 676 | u8     | constant (109) | flag / sentinel |

The two u16 price-like fields have different magnitudes (~26k and ~28k), so they aren't bid/ask of the same instrument. Most likely they're a (price, depth) pair or two scaled references.

## Negative result: on-chain Pyth correlation is zero

All six Pyth feeds polled (SOL/USD, BTC/USD, ETH/USD, USDC/USD, USDT/USD, BONK/USD) returned constant values across the 120-second window. The legacy v2 push receivers on mainnet are not being refreshed often enough to drive humidifi's tick cadence. Conclusion: **humidifi sources its price from off chain**, not from on-chain Pyth.

## What this unlocks

- `accountSubscribe` on the active pool set → real-time humidifi quote-change firehose
- Fingerprint constant-byte signatures (`93` at 660, `109` at 676, …) to cluster pools by instrument class
- Once u16 ticks at 616/675 are pinned to instruments (via a non-legacy price feed), emit a public humidifi quote feed
