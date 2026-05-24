#!/usr/bin/env python3
"""auto-reverse-reserves.py — find (reserve_x, reserve_y) byte offsets in a
prop AMM pool account, fully automated, public RPC only.

Strategy (per pool):
  1. getSignaturesForAddress → latest swap tx touching the pool.
  2. From that tx's preTokenBalances, identify the two vault accounts
     (token accounts that the pool's swap CPI'd into).
  3. getAccountInfo on each vault → read its current `amount` u64 (offset 64
     of a 165-byte SPL token account).
  4. Scan the pool's raw data for u64 little-endian windows matching either
     vault amount → record the offsets.

Across 3 sample pools, the offsets that consistently hold the vault amount
are reserve_x and reserve_y.

Falls back to TIER-2 (snapshot diff) if TIER-1 fails:
  - For one pool, take two snapshots a few seconds apart, find u64 offsets
    where the value changed by a small-but-nonzero delta consistent with
    swaps. These are the reserves even if stored as virtual values that
    don't equal vault balances directly.

Usage:
  python3 scripts/auto-reverse-reserves.py \
      --program <PID> --size <DATA_SIZE> [--pools 3] [--mint-offsets m1,m2]
"""
from __future__ import annotations
import argparse, base64, json, sys, time, urllib.request

RPC = "https://api.mainnet-beta.solana.com"
TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN_2022    = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"

def rpc(method: str, params, retries: int = 3) -> dict:
    body = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req  = urllib.request.Request(RPC, data=body, headers={"content-type":"application/json"})
    for i in range(retries):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            if e.code == 429 and i < retries - 1:
                time.sleep(2 ** i + 1)
                continue
            raise
        except Exception:
            if i == retries - 1: raise
            time.sleep(1.5)
    raise RuntimeError("unreachable")

def list_pools(program: str, size: int) -> list[str]:
    res = rpc("getProgramAccounts", [
        program,
        {"encoding":"base64","filters":[{"dataSize":size}],
         "withContext":False,"dataSlice":{"offset":0,"length":0}},
    ])
    if "error" in res: sys.exit(f"getProgramAccounts: {res['error']}")
    return [p["pubkey"] for p in res.get("result", [])]

def fetch_data(pool: str) -> bytes:
    res = rpc("getAccountInfo", [pool, {"encoding":"base64"}])
    v = res["result"]["value"]
    if v is None: raise RuntimeError(f"account {pool} not found")
    return base64.b64decode(v["data"][0])

def find_vaults_for_pool(pool: str, lookback: int = 30) -> list[str]:
    """Find token accounts the pool's swap CPI touched recently.
    Returns deduped list of vault pubkeys observed in preTokenBalances."""
    sigs = rpc("getSignaturesForAddress",[pool,{"limit":lookback}]).get("result",[])
    vaults: set[str] = set()
    for s in sigs:
        if vaults and len(vaults) >= 2: break
        tx = rpc("getTransaction",[s["signature"],
                 {"encoding":"jsonParsed","maxSupportedTransactionVersion":0}]).get("result")
        if not tx: continue
        meta = tx.get("meta") or {}
        bals = (meta.get("preTokenBalances") or []) + (meta.get("postTokenBalances") or [])
        if not bals: continue
        # accountIndex points into the message's accountKeys list
        msg = tx["transaction"]["message"]
        keys = []
        for k in msg.get("accountKeys",[]):
            keys.append(k["pubkey"] if isinstance(k,dict) else k)
        loaded = meta.get("loadedAddresses",{}) or {}
        keys += loaded.get("writable",[]) + loaded.get("readonly",[])
        for b in bals:
            idx = b.get("accountIndex")
            if idx is None or idx >= len(keys): continue
            pk = keys[idx]
            if pk == pool: continue
            vaults.add(pk)
        time.sleep(0.3)  # gentle on public RPC
    return list(vaults)

def vault_amount(vault: str) -> int | None:
    """Read SPL token account .amount (u64 at offset 64). Token-2022 same layout."""
    res = rpc("getAccountInfo",[vault,{"encoding":"base64"}])
    v = res.get("result",{}).get("value")
    if not v: return None
    owner = v["owner"]
    if owner not in (TOKEN_PROGRAM, TOKEN_2022): return None
    data = base64.b64decode(v["data"][0])
    if len(data) < 72: return None
    return int.from_bytes(data[64:72], "little")

def u64_offsets_matching(data: bytes, value: int) -> list[int]:
    """All 8-byte aligned offsets where the LE u64 equals `value`."""
    target = value.to_bytes(8, "little")
    out: list[int] = []
    for off in range(0, len(data) - 7, 8):
        if data[off:off+8] == target:
            out.append(off)
    return out

def tier1(pool: str) -> dict:
    """For one pool: return {vault_pubkey: {amount, offsets_in_pool}}"""
    vaults = find_vaults_for_pool(pool)
    print(f"  vaults: {vaults}", file=sys.stderr)
    pool_data = fetch_data(pool)
    out = {}
    for v in vaults:
        amt = vault_amount(v)
        if amt is None or amt == 0: continue
        offs = u64_offsets_matching(pool_data, amt)
        out[v] = {"amount": amt, "offsets": offs}
    return out

def tier2(pool: str, wait_seconds: float = 6.0) -> list[int]:
    """Two snapshots, find u64 offsets that changed (small non-zero delta)."""
    snap1 = fetch_data(pool)
    print(f"  tier2: waiting {wait_seconds}s for next swap…", file=sys.stderr)
    time.sleep(wait_seconds)
    snap2 = fetch_data(pool)
    diffs = []
    for off in range(0, min(len(snap1), len(snap2)) - 7, 8):
        a = int.from_bytes(snap1[off:off+8], "little")
        b = int.from_bytes(snap2[off:off+8], "little")
        if a == b or a == 0 or b == 0: continue
        # Plausible reserve range, and small relative delta (< 50%)
        if not (1_000 < a < 10**18 and 1_000 < b < 10**18): continue
        delta = abs(a - b) / max(a, b)
        if delta > 0.5: continue
        diffs.append((off, a, b, delta))
    return diffs

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--program", required=True)
    ap.add_argument("--size", type=int, required=True)
    ap.add_argument("--pools", type=int, default=3)
    args = ap.parse_args()

    print(f"# {args.program} size={args.size}", file=sys.stderr)
    pools = list_pools(args.program, args.size)
    print(f"# {len(pools)} pools found", file=sys.stderr)
    if len(pools) < args.pools:
        sys.exit(f"need {args.pools} pools, have {len(pools)}")

    # TIER 1
    per_pool_offsets: list[set[int]] = []
    samples = []
    for pool in pools[:args.pools]:
        print(f"# pool {pool}", file=sys.stderr)
        try:
            result = tier1(pool)
        except Exception as e:
            print(f"  tier1 error: {e}", file=sys.stderr); result = {}
        # Collect ALL offsets where any vault amount matched
        all_offsets = set()
        for v, info in result.items():
            for o in info["offsets"]:
                all_offsets.add(o)
        print(f"  matched offsets: {sorted(all_offsets)}", file=sys.stderr)
        per_pool_offsets.append(all_offsets)
        samples.append({"pool": pool, "vaults": result})
        time.sleep(1)

    common = set.intersection(*per_pool_offsets) if per_pool_offsets else set()
    print(f"# common offsets across {args.pools} pools: {sorted(common)}", file=sys.stderr)

    if len(common) >= 2:
        result = {
            "program": args.program, "size": args.size,
            "method": "tier1 (vault-balance match)",
            "reserve_offsets": sorted(common)[:2],
            "samples": samples,
        }
        print(json.dumps(result, indent=2))
        return 0

    # TIER 2 — fallback
    print(f"# tier1 insufficient ({len(common)} common offsets); trying tier2…", file=sys.stderr)
    diffs = tier2(pools[0])
    print(f"# tier2 candidates ({len(diffs)} u64 offsets changed):", file=sys.stderr)
    for off, a, b, d in diffs[:10]:
        print(f"  off={off:5d}  {a} → {b}  ({d*100:.3f}%)", file=sys.stderr)

    if len(diffs) >= 2:
        # Pick the two with the smallest relative deltas (most "swap-like")
        diffs.sort(key=lambda x: x[3])
        top2 = sorted([d[0] for d in diffs[:2]])
        result = {
            "program": args.program, "size": args.size,
            "method": "tier2 (snapshot diff)",
            "reserve_offsets": top2,
            "tier2_candidates": [{"offset":o,"v1":a,"v2":b,"delta_pct":round(d*100,4)} for o,a,b,d in diffs[:10]],
        }
        print(json.dumps(result, indent=2))
        return 0

    print(json.dumps({"program":args.program,"size":args.size,
                      "reserve_offsets":None,"reason":"both tiers failed"}, indent=2))
    return 1

if __name__ == "__main__":
    sys.exit(main())
