# PAMM Terminal

A single-file risk cockpit for monitoring the prop AMM in real-time.

Live at **[pamm.aileena.xyz](https://pamm.aileena.xyz)** — sim mode runs without any bot.

---

## Layout

```
┌─ PAMM TERMINAL ── pool: SOL/USDC ── bot: SIM ── slot ── rpc ── tx ──────────┐
│ Pool State    │  Fee Model Trace (ewma_vol · shock decay · fee_bps)          │ Signals   │
│ oracle price  │  ─────────────────── 30 bps floor ────────────────────────  │ SHOCK     │
│ spread / fee  │                                                               │ VOL       │
│ reserves bar  ├───────────────────────────────────────────────────────────── │ ORACLE    │
│               │  Price & Reserve Imbalance                                   │ IMBALANCE │
│ MM Strategy   ├───────────────────────────────────────────────────────────── │ CB        │
│ circuit breaker│  Reader Bot — Cumulative PnL & Volume                       │           │
│ skew / pnl    │                                                               │ Event Log │
│               │                                                               │           │
│ Reader Bot    │                                                               │           │
│ trades / PnL  │                                                               │           │
└───────────────┴───────────────────────────────────────────────────────────────┴───────────┘
```

**Left** — three sections: Pool State (price, spread, reserves), MM Strategy (circuit breaker, skew, LP PnL), Reader Bot (trades, MtM PnL, volume, last signal)

**Center** — three chart rows:
- Fee model trace: total `fee_bps` (filled) + `base+vol` floor (dashed) + 30 bps reference line
- Price + value-adjusted reserve imbalance (dual y-axis)
- Reader bot cumulative PnL + trade volume (teal, dual y-axis)

**Right** — five signal badges + unified event log (MM swaps, reader trades, oracle updates, rebalances)

---

## Fee model

Implements `src/lib.rs` constants exactly in JS:

```
fee_bps = BASE(8) + ewmaVol × 20000 + shockSteps × 4   (cap: 100 bps)

ewmaVol  = 0.20 × |ΔP/P| + 0.80 × prev          (EWMA α = 0.20)
shock    = reset to 8 steps when |ΔP/P| ≥ 0.5%   (SHOCK_DECAY_STEPS = 8)
```

At calm (vol ≈ 0.1%): fee ≈ 28 bps — stays below the 30 bps normalizer floor, attracting retail flow.

---

## Data source

**Sim mode** (default): JS simulation runs the fee model with randomized price walks and reader bot momentum logic. No bot required.

**Live mode**: connects to `http://127.0.0.1:19001/feed` (SSE, same format as `dashboard.html`). Auto-switches when the bot is running. Accepts the `snapshot` event with this shape:

```json
{
  "oracle":  { "price": 90280000000, "last_update_age_s": 2, "spread_bps": 28, "cycles": 142 },
  "pool":    { "reserve_a": 195000000, "reserve_b": 503000000 },
  "trading": { "total_swaps": 87 },
  "lp":      { "pnl_bps": 12 },
  "reader":  { "trades": 14, "pnl_bps": -3.2, "mtm_pnl_b": -0.04, "volume_b": 890000,
               "spread_paid_b": 12.4, "last_move_bps": 12, "last_sig": "3Kpx" },
  "uptime_s": 3420,
  "halted":   false,
  "ts":       1747568400
}
```

---

## Files

| File | Description |
|------|-------------|
| `terminal.html` | **Main** — unified terminal (deploy this) |
| `risk-terminal.html` | Fee model focused view, standalone |
| `sim-viz.html` | Static devnet simulation snapshot (Apr 2026) |
| `dashboard.html` | Original live MM bot dashboard |

---

## Deploy

Deployed via Vercel from this directory. `vercel.json` routes `/` → `terminal.html`.

```sh
# one-time
vercel deploy --prod

# custom domain: add pamm.aileena.xyz in Vercel dashboard,
# then CNAME pamm → cname.vercel-dns.com at your DNS provider
```
