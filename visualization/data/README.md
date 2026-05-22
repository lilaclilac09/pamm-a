# visualization/data/

Static data the dashboard fetches at runtime. Mirror of canonical sources in
[`../../references/solana/prop-amm/`](../../references/solana/prop-amm/) — kept
here because Vercel only serves files under `visualization/`.

| File | Source of truth |
|---|---|
| `sample-active-markets.json` | `references/solana/prop-amm/sample-active-markets.json` |
| `known-mints.json` | `references/solana/prop-amm/known-mints.json` |
| `active-markets-latest.json` | produced by `scripts/scan-prop-amm.sh`; copy from `references/solana/prop-amm/snapshots/active-markets-latest.json` |

When the scanner produces a fresh snapshot, refresh the live file:

```bash
cp references/solana/prop-amm/snapshots/active-markets-latest.json \
   visualization/data/active-markets-latest.json
```

If `active-markets-latest.json` is absent the radar falls back to
`sample-active-markets.json`.
