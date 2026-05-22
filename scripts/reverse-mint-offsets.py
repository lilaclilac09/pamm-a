#!/usr/bin/env python3
"""reverse-mint-offsets.py — find the byte offsets of mint1/mint2 inside a prop AMM
pool account, given a known (mint1, mint2) pair.

Usage:
    python3 scripts/reverse-mint-offsets.py \\
        --pool <POOL_PUBKEY> \\
        --mint1 <BASE58_MINT> \\
        --mint2 <BASE58_MINT> \\
        --rpc-url "$RPC_URL"

Strategy:
    1. Fetch the pool's raw account data (base64).
    2. base58-decode each mint to 32 raw bytes.
    3. Search the account bytes for both 32-byte windows.
    4. Print all (offset_mint1, offset_mint2) pairs found.

Tip — to find a known pool: pick the AMM's program ID from
references/solana/prop-amm/programs.json, fetch one of its pools, then look it
up on Birdeye / Solscan to get the mint pair. Or just inspect a recent swap
transaction touching that program.

After you find consistent offsets across multiple pools (recommended: verify
with at least 3), patch references/solana/prop-amm/account-layouts.json.
"""
from __future__ import annotations

import argparse
import base64
import json
import sys
import urllib.request

import base58


def fetch_account(rpc_url: str, pool: str) -> bytes:
    req = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "getAccountInfo",
        "params": [pool, {"encoding": "base64"}],
    }).encode()
    r = urllib.request.urlopen(urllib.request.Request(
        rpc_url, data=req, headers={"content-type": "application/json"},
    ))
    body = json.loads(r.read())
    if "error" in body:
        raise SystemExit(f"RPC error: {body['error']}")
    value = body["result"]["value"]
    if value is None:
        raise SystemExit(f"account not found: {pool}")
    return base64.b64decode(value["data"][0])


def find_all(haystack: bytes, needle: bytes) -> list[int]:
    out, start = [], 0
    while True:
        i = haystack.find(needle, start)
        if i == -1:
            return out
        out.append(i)
        start = i + 1


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--pool", required=True, help="pool account pubkey (base58)")
    ap.add_argument("--mint1", required=True, help="first mint pubkey (base58)")
    ap.add_argument("--mint2", required=True, help="second mint pubkey (base58)")
    ap.add_argument("--rpc-url", required=True)
    args = ap.parse_args()

    data = fetch_account(args.rpc_url, args.pool)
    print(f"account size: {len(data)} bytes")

    m1_raw = base58.b58decode(args.mint1)
    m2_raw = base58.b58decode(args.mint2)
    if len(m1_raw) != 32 or len(m2_raw) != 32:
        raise SystemExit("mint pubkeys must decode to 32 bytes")

    o1 = find_all(data, m1_raw)
    o2 = find_all(data, m2_raw)

    print(f"mint1 ({args.mint1}) offsets: {o1}")
    print(f"mint2 ({args.mint2}) offsets: {o2}")

    if not o1 or not o2:
        print("\nNo match. Possible causes:")
        print("  • Wrong mint pair for this pool")
        print("  • Mints stored hashed / packed (not raw 32-byte pubkey)")
        print("  • Account decoded differently (e.g. ZeroCopy alignment shifts)")
        return 1

    print("\nCandidate layouts (try each, then verify across 3+ pools):")
    for a in o1:
        for b in o2:
            print(f"  {{ \"mint1\": {a}, \"mint2\": {b} }}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
