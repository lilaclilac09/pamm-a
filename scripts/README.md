# scripts/

Helper scripts that feed downstream consumers (radar panel, flow-aware EWMA, swap-compare). Each one stands alone — none are required for the competition submission in `src/`.

## scan-prop-amm.sh

Enumerate active pools across the 10 Solana prop AMMs listed in [`references/solana/prop-amm/programs.json`](../references/solana/prop-amm/programs.json) and emit a JSON snapshot.

```bash
export IRONFORGE_KEY=...
bash scripts/scan-prop-amm.sh
# → references/solana/prop-amm/snapshots/active-markets-<UTC>.json
# → references/solana/prop-amm/snapshots/active-markets-latest.json (symlink)
```

Requires `jq`, `curl`, `python3`, and `pip install base58`. Public RPCs throttle `getProgramAccounts`; use Ironforge / Helius / QuickNode / Triton.

## reverse-mint-offsets.py

Recover the (mint1_offset, mint2_offset) layout for a prop AMM whose offsets aren't published — given a known (pool, mint1, mint2) triple.

```bash
python3 scripts/reverse-mint-offsets.py \
  --pool <POOL_PUBKEY> \
  --mint1 <BASE58_MINT> \
  --mint2 <BASE58_MINT> \
  --rpc-url "$RPC_URL"
```

Verify the candidate offsets across at least 3 different pools before patching `references/solana/prop-amm/account-layouts.json`.

## auto-reverse.py

Fully automated mint-offset reversal — **no prior knowledge** of which pairs the AMM holds. Brute-forces every 32-byte window in raw account data, batch-checks them via `getMultipleAccounts`, keeps the offsets where every sample pool holds a different SPL Token mint.

```bash
python3 scripts/auto-reverse.py \
  --program <PROG_ID> --size <DATA_SIZE> --pools 3 \
  [--rpc-url https://api.mainnet-beta.solana.com]
```

Works for any prop AMM that stores mints as raw pubkeys at fixed offsets (8 of 10 do — see `docs/blog-prop-amm-dict.md` for the one exception, `humidifi`, which uses an off-chain mint registry instead).
