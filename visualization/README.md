# PAMM Terminal

A single-file risk cockpit for monitoring the prop AMM in real-time.

Live at **[pamm.aileena.xyz](https://pamm.aileena.xyz)** — sim mode runs without any bot.

---

## Layout

```
┌─ PAMM TERMINAL ── pool ── mm ── reader ── slot[◀ ▶ LIVE] ── builder ── rpc ── tx ── fills ┐
│ Pool State    │  Fee Model (ewma_vol · shock decay · fee_bps · 30 bps floor)  │ Signals   │ ── strategy
│ MM Strategy   │  Price & Reserve Imbalance                                    │ Event Log │    cockpit
│ Reader Bot    │  Reader Bot — Cumulative PnL & Volume                         │           │    (top 60%)
├───────────────┴───────────────────────────────────────────────────────────────┴───────────┤
│ Slot Detail   │  Markout per Fill (5s bars · 30s line)                                    │ ── execution
│ slot/builder  ├───────────────────────────────────────────────────────────────────────────│    layer
│ build_ms · CU │  FILLS TABLE — slot · +ms · sig · src · side · size · fee · sim · landed  │    (bottom 40%)
│ Builder Mix   │                · builder · mk 5s · mk 30s                                 │
│ Avg Mk5/build │  (sortable headers · click row to pin slot)                               │
└───────────────┴───────────────────────────────────────────────────────────────────────────┘
```

**Top half — strategy cockpit:**
- **Left**: Pool State (price, spread, reserves), MM Strategy (circuit breaker, skew, LP PnL), Reader Bot (trades, MtM PnL, volume, last signal)
- **Center**: Fee model trace (`fee_bps` filled + `base+vol` floor dashed + 30 bps reference), Price + value-adjusted reserve imbalance, Reader bot cumulative PnL + volume
- **Right**: Five signal badges (SHOCK / VOL / ORACLE / IMBAL / CB) + unified event log

**Bottom half — execution layer** (Peekaboo / Dissonance-style):
- **Left**: Slot Detail (slot #, builder chip, build_ms, fills count, CU used + bar, avg mk5/mk30), Builder Mix (last 120 slots stacked by BAM/Harmonic/Jito/Frank/Rakurai), Avg Markout 5s by Builder
- **Right**: Markout per Fill chart (5s bars colored by sign + 30s line overlay), FILLS TABLE with sortable columns (`slot · +ms · sig · src · side · size_b · fee · sim ms · landed ms · builder · mk 5s · mk 30s`). Click any row to pin that slot to the cockpit; click ▶ twice (or the PINNED tag) to resume live.

**Topbar**: slot navigation (◀ ▶), current builder chip, RPC latency, tx success %, fills count, LIVE / PINNED indicator.

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
| `terminal.html` | **Main** — unified strategy + execution terminal (deploy this) |
| `risk-terminal.html` | Fee model focused view, standalone |
| `sim-viz.html` | Static devnet simulation snapshot (Apr 2026) |
| `dashboard.html` | Original live MM bot dashboard |

---

## Execution layer (bottom half of `terminal.html`)

Inspired by Jito Peekaboo and Harmonic Dissonance. Adds slot / CU / builder / markout visibility to the same single-file dash — no separate page, no tab switching.

**Builders modeled** (stake shares Feb–Mar 2026, public Blockworks Research data): BAM 27%, Harmonic 17%, Jito 38%, Frankendancer 11%, Rakurai 7%. Per-builder priors:
- **build_ms**: triangular (min, mode, max) — BAM `[180, 362, 420]`, Harmonic `[200, 407, 560]`, Jito `[200, 395, 470]`
- **markout 5s**: gaussian — BAM `μ=+1.8 σ=2.4`, Harmonic `μ=+0.4 σ=3.2` (heavier left tail), Jito `μ=+1.0 σ=2.6`
- **fail rate bias**: BAM 0.7×, Harmonic 1.4×, Jito 1.0×

Every strategy tick = 1 sim slot. Each MM swap and reader trade fired by `simTick()` is decorated with builder/sim/landed/markout metadata and pushed into the fills table — the strategy and execution views always stay in sync.

### Future live SSE schema

When the bot is wired up to emit per-fill data, send a `fill` event alongside `snapshot`:

```json
{
  "kind": "fill",
  "slot": 420890019,
  "slot_offset_ms": 142,
  "builder": "bam",
  "sig": "5Kpx...d7",
  "side": "buy",
  "size_b": 12500,
  "fee_bps": 28.3,
  "sim_duration_ms": 0.101,
  "validate_fees_ms": 0.003,
  "load_ms": 0.001,
  "landed_ms": 23.5,
  "markout_5s_bps": 1.2,
  "markout_30s_bps": -0.4
}
```

---

## Deploy

Deployed via Vercel from this directory. `vercel.json` routes `/` → `terminal.html`.

```sh
# one-time
vercel deploy --prod

# custom domain: add pamm.aileena.xyz in Vercel dashboard,
# then CNAME pamm → cname.vercel-dns.com at your DNS provider
```
