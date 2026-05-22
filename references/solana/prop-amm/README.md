# Solana Prop AMM intel

Static intel for 10 closed-source proprietary AMMs on Solana mainnet. Consumed by:

- [`visualization/terminal.html`](../../../visualization/terminal.html) → "Prop AMM Radar" panel
- [`jupiter-mm-bot/`](../../../jupiter-mm-bot/) → off-chain flow signal (B in the plan)
- [`swap-compare/`](../../../swap-compare/) → competitor-coverage column (E)
- [`flow-aware-ewma/`](../../../flow-aware-ewma/) → on-chain v3 strategy hook

## Files

| File | Purpose |
|---|---|
| `programs.json` | 10 program IDs + their pool `dataSize` for `getProgramAccounts` |
| `account-layouts.json` | byte offsets for `mint1`, `mint2` inside each pool account. `humidifi` / `aquifier` are TODO — run `scripts/reverse-mint-offsets.py` |
| `known-mints.json` | base58 mint → `{symbol, decimals}` for human-readable display |
| `sample-active-markets.json` | static fallback the radar UI uses when no live snapshot exists |
| `snapshots/*.json` | live scan output (gitignored), produced by `scripts/scan-prop-amm.sh` |

## How to refresh

```bash
export IRONFORGE_KEY=...   # get one at https://www.ironforge.network/
bash scripts/scan-prop-amm.sh
# writes references/solana/prop-amm/snapshots/active-markets-<UTC>.json
```

The dashboard auto-loads the newest snapshot if served from the same origin. If no snapshot exists it falls back to `sample-active-markets.json`.

## Source

Originally reverse-engineered by `mubarizkyc` — see <https://gist.github.com/mubarizkyc/959ac9b33dae4f3a86c6e00c331a9901>. Verified `2026-05-22`. The 8 published offsets are copied verbatim; the 2 missing ones are blank until reversed locally.

## Verify before trusting

Layouts drift. Before relying on these in live trading:

1. Pick one pool per AMM from `snapshots/`.
2. Fetch its raw account data via `getAccountInfo` with `encoding: base64`.
3. Decode bytes at the documented offset and base58-encode → confirm it matches a real SPL mint.
4. If wrong, re-reverse with `scripts/reverse-mint-offsets.py`.
