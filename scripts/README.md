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

## auto-reverse-reserves.py

Attempts to find `(reserve_x, reserve_y)` offsets inside a pool account via two tiers:

1. **vault-balance match**: read recent tx history, find token-vault accounts the pool's swap touched, read each vault's current `amount`, search the pool's raw data for matching u64 LE.
2. **snapshot diff**: take two pool snapshots a few seconds apart, find u64 offsets where the value changed by a small-but-nonzero delta consistent with a swap.

```bash
python3 scripts/auto-reverse-reserves.py --program <PID> --size <DSIZE> --pools 3
```

Works cleanly for classic CPMMs. Fails for CLMMs / virtual-reserve AMMs (e.g. aquifier) where the stored values are scaled / Q64.64 fixed-point and don't equal vault balances directly. For TVL display in the dashboard, see `build-vault-registry.py` below — it's the cleaner approach.

## build-vault-registry.py

For each prop-AMM pool, finds its two token-vault accounts via tx history (no struct decode needed) and writes a `vault-registry.json` that maps `pool → (vault_a, vault_b, mint_a, mint_b)`.

```bash
python3 scripts/build-vault-registry.py --max-pools-per-amm 50
# → references/solana/prop-amm/vault-registry.json
```

At display time, downstream consumers (dashboard, swap-compare) just read each vault's `amount` via `getAccountInfo` — works uniformly across all 9 transparent prop AMMs, no per-AMM struct knowledge. Humidifi is excluded (its swaps don't touch pool accounts as transferors).

## humidifi-watch.py / humidifi-decode.py

Two-pass investigation of humidifi's 1728-byte pool accounts — the one AMM that doesn't store mints on chain at all.

```bash
# 1) record 120s of pool snapshots + Pyth feeds
python3 scripts/humidifi-watch.py --mode record \
  --pool <ACTIVE_HUMIDIFI_POOL> --duration 120 --interval 2.0 \
  --out trace.jsonl

# 2) quick analysis — byte-change frequency + Pyth correlation
python3 scripts/humidifi-watch.py --mode analyze --input trace.jsonl

# 3) deep structural decode — width=1/2/4/8 classification per hot range
python3 scripts/humidifi-decode.py --input trace.jsonl
```

See `docs/blog-humidifi-decoded.md` for what these recover (~3.3% of the account is live, in 6 narrow ranges; two u16 ticks classify as "price-like", a u64 in the 624-660 range is monotonic). Pick an active humidifi pool — most of the 89 historically-allocated accounts haven't moved in months. Sample by `getSignaturesForAddress` on the program ID to find currently-touched pools.
