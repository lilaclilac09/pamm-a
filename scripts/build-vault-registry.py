#!/usr/bin/env python3
"""build-vault-registry.py — discover token-vault accounts for every pool of
every prop AMM, by inspecting recent swap transactions.

Output schema (writes to references/solana/prop-amm/vault-registry.json):
  {
    "generated_at": "...",
    "by_pool": {
      "<pool_pubkey>": {
        "amm": "solfi",
        "vaults": ["VaultA...", "VaultB..."],
        "vault_mints": ["So111...", "EPjFW..."],
        "vault_owner": "<authority PDA, if discernible>"
      },
      ...
    }
  }

Why this and not reverse_layouts.json:
  Different prop AMMs use different internal accounting (CPMM raw reserves,
  CLMM virtual ticks, Q64.64 fixed-point). Finding "the reserve offset"
  inside the pool account requires per-AMM struct knowledge. But every AMM
  custodies tokens in standard SPL token accounts owned by a pool authority
  — and those vault accounts ALWAYS expose .amount as a u64 at byte 64.

  So we don't decode the pool. We just remember which two vaults each pool
  uses, and at display time we read the vaults directly. Works uniformly
  across all 9 transparent prop AMMs (skips humidifi, which doesn't move
  tokens through pool accounts).

Usage:
  python3 scripts/build-vault-registry.py [--max-pools-per-amm 50] [--rpc-url ...]
  → writes references/solana/prop-amm/vault-registry.json
"""
from __future__ import annotations
import argparse, base64, json, sys, time, urllib.request
from pathlib import Path
from datetime import datetime, timezone

DEFAULT_RPC = "https://api.mainnet-beta.solana.com"
TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN_2022    = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
TOKEN_OWNERS  = {TOKEN_PROGRAM, TOKEN_2022}

# Programs we skip — humidifi doesn't transfer tokens through pool accounts.
SKIP_PROGRAMS = {"humidifi"}

ROOT = Path(__file__).resolve().parent.parent
INTEL = ROOT / "references" / "solana" / "prop-amm"

def rpc_call(url: str, method: str, params, retries: int = 4) -> dict:
    body = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req  = urllib.request.Request(url, data=body, headers={"content-type":"application/json"})
    for i in range(retries):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            if e.code == 429 and i < retries - 1:
                back = 3 + i * 4
                time.sleep(back)
                continue
            raise
        except Exception:
            if i == retries - 1: raise
            time.sleep(2)
    raise RuntimeError("unreachable")

def list_pools(url: str, program: str, size: int) -> list[str]:
    res = rpc_call(url, "getProgramAccounts", [
        program,
        {"encoding":"base64","filters":[{"dataSize":size}],
         "withContext":False,"dataSlice":{"offset":0,"length":0}},
    ])
    if "error" in res: raise RuntimeError(f"getProgramAccounts: {res['error']}")
    return [p["pubkey"] for p in res.get("result", [])]

def find_vaults_via_tx(url: str, pool: str, lookback: int = 10) -> tuple[list[str], list[str]]:
    """Return (vault_pubkeys, vault_mints) inferred from recent swap tx."""
    sigs = rpc_call(url, "getSignaturesForAddress", [pool,{"limit":lookback}]).get("result", [])
    seen_vaults: dict[str, str] = {}  # vault_pk -> mint
    for s in sigs:
        if len(seen_vaults) >= 2: break
        time.sleep(0.4)  # polite spacing
        try:
            tx = rpc_call(url, "getTransaction",
                          [s["signature"],{"encoding":"jsonParsed","maxSupportedTransactionVersion":0}]).get("result")
        except Exception as e:
            print(f"      tx fetch err: {e}", file=sys.stderr); continue
        if not tx: continue
        meta = tx.get("meta") or {}
        bals = (meta.get("preTokenBalances") or []) + (meta.get("postTokenBalances") or [])
        if not bals: continue
        msg = tx["transaction"]["message"]
        keys = [k["pubkey"] if isinstance(k,dict) else k for k in msg.get("accountKeys",[])]
        loaded = meta.get("loadedAddresses",{}) or {}
        keys += loaded.get("writable",[]) + loaded.get("readonly",[])
        for b in bals:
            idx = b.get("accountIndex")
            if idx is None or idx >= len(keys): continue
            pk = keys[idx]
            if pk == pool: continue
            seen_vaults.setdefault(pk, b.get("mint",""))
    return list(seen_vaults.keys()), list(seen_vaults.values())

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--max-pools-per-amm", type=int, default=50)
    ap.add_argument("--rpc-url", default=DEFAULT_RPC)
    ap.add_argument("--output", default=str(INTEL / "vault-registry.json"))
    args = ap.parse_args()

    programs = json.loads((INTEL / "programs.json").read_text())["programs"]
    out: dict[str, dict] = {}
    skipped: list[str] = []

    for prog in programs:
        name, pid, size = prog["name"], prog["program_id"], prog["account_size"]
        if name in SKIP_PROGRAMS:
            skipped.append(name); continue
        print(f"\n[{name}] listing pools (size={size})…", file=sys.stderr)
        try:
            pools = list_pools(args.rpc_url, pid, size)
        except Exception as e:
            print(f"  ERR: {e}", file=sys.stderr); continue
        print(f"  {len(pools)} pools; scanning first {min(args.max_pools_per_amm, len(pools))}", file=sys.stderr)
        for i, pool in enumerate(pools[:args.max_pools_per_amm]):
            try:
                vaults, mints = find_vaults_via_tx(args.rpc_url, pool)
            except Exception as e:
                print(f"    [{i+1}] {pool[:8]}… ERR {e}", file=sys.stderr); continue
            if len(vaults) >= 2:
                out[pool] = {"amm": name, "vaults": vaults[:2], "vault_mints": mints[:2]}
                print(f"    [{i+1}] {pool[:8]}… vaults=({vaults[0][:8]}…, {vaults[1][:8]}…)", file=sys.stderr)
            else:
                print(f"    [{i+1}] {pool[:8]}… no vaults found", file=sys.stderr)
            time.sleep(0.3)
        time.sleep(2)  # gap between AMMs

    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "rpc_url": args.rpc_url.split("?")[0],
        "by_pool": out,
        "skipped_programs": skipped,
        "_note": "Vault discovery via tx-history. Skipped programs don't move tokens through pool accounts (oracle/quote model).",
    }
    Path(args.output).write_text(json.dumps(payload, indent=2))
    print(f"\nwrote {len(out)} pool→vault mappings to {args.output}", file=sys.stderr)
    return 0

if __name__ == "__main__":
    sys.exit(main())
