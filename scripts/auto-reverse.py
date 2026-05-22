#!/usr/bin/env python3
"""auto-reverse.py — fully automated mint-offset reversal.

For a given prop-AMM program + account size:
  1. getProgramAccounts → list pool pubkeys.
  2. Pick first N pools (default 3).
  3. For each pool, scan every 8-byte-aligned 32-byte window in its raw data,
     base58-encode each window as a candidate pubkey.
  4. Batch-check the candidates via getMultipleAccounts:
     a candidate is a Mint if its owner is the SPL Token (or Token-2022)
     program. Track the byte offset where each Mint was found.
  5. The two offsets that contain Mint accounts CONSISTENTLY across all N
     pools are the answer.

Usage:
  python3 scripts/auto-reverse.py --program <PID> --size <N> [--pools 3] \
      [--rpc-url https://api.mainnet-beta.solana.com]

Outputs JSON to stdout:
  { "mint1": <offset>, "mint2": <offset>,
    "samples": [{"pool": "...", "mint1": "...", "mint2": "..."}, ...] }
"""
from __future__ import annotations
import argparse, base64, json, sys, time, urllib.request

try:
    import base58
except ImportError:
    sys.exit("pip install base58")

TOKEN_PROGRAM      = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN_2022_PROGRAM = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
MINT_OWNERS        = {TOKEN_PROGRAM, TOKEN_2022_PROGRAM}

def rpc(url: str, method: str, params) -> dict:
    body = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req  = urllib.request.Request(url, data=body, headers={"content-type":"application/json"})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.loads(r.read())
        except Exception as e:
            if attempt == 2: raise
            time.sleep(1 + attempt)
    raise RuntimeError("unreachable")

def list_pools(url: str, program: str, size: int) -> list[str]:
    res = rpc(url, "getProgramAccounts", [
        program,
        {"encoding":"base64", "filters":[{"dataSize":size}],
         "withContext":False, "dataSlice":{"offset":0,"length":0}},
    ])
    if "error" in res: sys.exit(f"getProgramAccounts error: {res['error']}")
    return [p["pubkey"] for p in res.get("result", [])]

def fetch_data(url: str, pool: str) -> bytes:
    res = rpc(url, "getAccountInfo", [pool, {"encoding":"base64"}])
    if "error" in res: sys.exit(f"getAccountInfo({pool}) error: {res['error']}")
    val = res["result"]["value"]
    if val is None: sys.exit(f"account {pool} not found")
    return base64.b64decode(val["data"][0])

def candidates_with_offsets(data: bytes, step: int = 8) -> list[tuple[int,str]]:
    """All 8-byte-aligned 32-byte windows, base58-encoded."""
    out = []
    for off in range(0, len(data) - 31, step):
        window = data[off:off+32]
        # Skip all-zero windows (default field, not a real pubkey).
        if window == b"\x00" * 32:
            continue
        try:
            pk = base58.b58encode(window).decode()
            out.append((off, pk))
        except Exception:
            continue
    return out

def find_mints_at(url: str, cands: list[tuple[int,str]]) -> dict[int,str]:
    """Returns {offset: mint_pubkey} for every candidate that resolves to a Mint."""
    # getMultipleAccounts max 100 per call. Dedupe by pubkey first.
    pk_to_offsets: dict[str,list[int]] = {}
    for off, pk in cands:
        pk_to_offsets.setdefault(pk, []).append(off)
    pks = list(pk_to_offsets.keys())

    hits: dict[int,str] = {}
    for i in range(0, len(pks), 100):
        chunk = pks[i:i+100]
        res = rpc(url, "getMultipleAccounts", [chunk, {"encoding":"base64"}])
        if "error" in res:
            print(f"WARN: getMultipleAccounts error: {res['error']}", file=sys.stderr)
            continue
        vals = res["result"]["value"]
        for pk, val in zip(chunk, vals):
            if val is None: continue
            owner = val.get("owner")
            if owner not in MINT_OWNERS: continue
            raw = base64.b64decode(val["data"][0])
            # SPL mint is exactly 82 bytes; Token-2022 mints start at 82 + ext.
            if len(raw) < 82: continue
            for off in pk_to_offsets[pk]:
                hits[off] = pk
    return hits

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--program", required=True)
    ap.add_argument("--size", type=int, required=True)
    ap.add_argument("--pools", type=int, default=3, help="pools to verify across")
    ap.add_argument("--rpc-url", default="https://api.mainnet-beta.solana.com")
    args = ap.parse_args()

    print(f"# listing pools for {args.program} (size={args.size})", file=sys.stderr)
    pools = list_pools(args.rpc_url, args.program, args.size)
    print(f"#   got {len(pools)} pools", file=sys.stderr)
    if len(pools) < args.pools:
        sys.exit(f"need {args.pools} pools, only got {len(pools)}")
    samples = pools[:args.pools]

    # For each sample, find mints + their offsets.
    per_pool: list[dict[int,str]] = []
    for pool in samples:
        print(f"# scanning {pool}…", file=sys.stderr)
        data = fetch_data(args.rpc_url, pool)
        cands = candidates_with_offsets(data, step=8)
        # Also try unaligned offsets — some layouts pack non-aligned.
        cands += candidates_with_offsets(data, step=1)
        # Dedupe (off, pk)
        cands = list({(o,p): None for o,p in cands}.keys())
        hits  = find_mints_at(args.rpc_url, cands)
        print(f"#   {len(hits)} mint hits at offsets {sorted(hits.keys())}", file=sys.stderr)
        per_pool.append(hits)

    # Offsets present in ALL pools.
    common_offsets = set(per_pool[0].keys())
    for h in per_pool[1:]:
        common_offsets &= set(h.keys())

    # Filter: each consistent-offset must hold a DIFFERENT mint per pool
    # (i.e. it varies across pools — that proves it's a real pool field, not
    # a shared program constant).
    variable = []
    for off in sorted(common_offsets):
        mints_seen = {h[off] for h in per_pool}
        variable.append((off, len(mints_seen), mints_seen))
    # A mint slot should yield >1 distinct mint when sampling different pools
    # (unless all sampled pools happen to share a quote token like USDC, which
    # is common). Keep slots with at least one variation OR exactly 2 such
    # offsets remaining — that's our pair.

    print("\n# candidate offsets (offset, distinct_mints_across_pools):", file=sys.stderr)
    for off, n, mints in variable:
        print(f"#   {off:5d}  n={n}  examples={list(mints)[:2]}", file=sys.stderr)

    if len(variable) < 2:
        sys.exit("not enough candidate offsets — try more pools")

    # Heuristic: pick the 2 offsets with highest variation; tiebreaker = lower offset first.
    variable.sort(key=lambda x: (-x[1], x[0]))
    top2 = sorted(variable[:2], key=lambda x: x[0])
    if len(top2) == 2 and top2[0][1] == 1 and top2[1][1] == 1:
        # Both invariant — all sample pools share both mints. Fall back to
        # taking the two lowest-offset hits.
        top2 = sorted(variable[:2], key=lambda x: x[0])

    off_a, off_b = top2[0][0], top2[1][0]
    result = {
        "program": args.program,
        "size": args.size,
        "mint1_offset": off_a,
        "mint2_offset": off_b,
        "samples": [
            {"pool": p, f"at_{off_a}": per_pool[i][off_a], f"at_{off_b}": per_pool[i][off_b]}
            for i, p in enumerate(samples)
        ],
    }
    print(json.dumps(result, indent=2))
    return 0

if __name__ == "__main__":
    sys.exit(main())
