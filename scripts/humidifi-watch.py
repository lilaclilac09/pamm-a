#!/usr/bin/env python3
"""humidifi-watch.py — poll one humidifi pool account at short intervals,
record every byte-level change, and cross-reference simultaneous Pyth feed
ticks to back out which bytes encode what.

Two-pass design:
  Pass 1 (--mode record):
    Poll the pool every `--interval` seconds for `--duration` seconds.
    Also poll a list of Pyth price accounts at the same cadence. Persist
    every snapshot + timestamp to a JSONL file.

  Pass 2 (--mode analyze):
    Load the JSONL. For every byte offset that ever changed, report:
      - changes/sec  → how active that byte is
      - value range  → min/max observed
      - cardinality  → distinct values
    Then for every 8-byte u64-LE window that changed, compute Pearson
    correlation against each Pyth feed's price series. Highest-r windows
    are the likely "price" fields.

Default RPC: public mainnet. Polls obey 250 ms minimum spacing.

Usage:
  python3 scripts/humidifi-watch.py --mode record   --pool <POOL_PK> --duration 60 --out trace.jsonl
  python3 scripts/humidifi-watch.py --mode analyze  --in trace.jsonl
"""
from __future__ import annotations
import argparse, base64, json, math, sys, time, urllib.request
from pathlib import Path
from collections import defaultdict

DEFAULT_RPC = "https://api.mainnet-beta.solana.com"

# Curated Pyth price accounts (Solana mainnet). Add more as needed.
# Source: https://pyth.network/developers/price-feed-ids
PYTH_FEEDS = {
    "SOL/USD":  "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG",
    "BTC/USD":  "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU",
    "ETH/USD":  "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB",
    "USDC/USD": "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD",
    "USDT/USD": "3vxLXJqLqF3JG5TCbYycbKWRBbCJQLxQmBGCkyqEEefL",
    "BONK/USD": "8ihFLu5FimgTQ1Unh4dVyEHUGodJ5gJQCrQf4KUVB9bN",
}

def rpc(url: str, method: str, params, retries: int = 4) -> dict:
    body = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req  = urllib.request.Request(url, data=body, headers={"content-type":"application/json"})
    for i in range(retries):
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            if e.code == 429 and i < retries - 1:
                time.sleep(2 ** i)
                continue
            raise
        except Exception:
            if i == retries - 1: raise
            time.sleep(1)
    raise RuntimeError("unreachable")

# ── Pass 1: record ──────────────────────────────────────────────────────────

def pyth_price(url: str, feed_pubkey: str) -> tuple[float, int] | None:
    """Read Pyth V2 price account. Returns (price_float, slot)."""
    res = rpc(url, "getAccountInfo", [feed_pubkey, {"encoding":"base64"}])
    v = res.get("result", {}).get("value")
    if not v: return None
    data = base64.b64decode(v["data"][0])
    # Pyth V2 layout (PriceAccount):
    # offset 208: price (i64), 216: confidence (u64), 224: exponent (i32)
    # 228: pub_slot (u64)
    if len(data) < 240: return None
    price = int.from_bytes(data[208:216], "little", signed=True)
    expo  = int.from_bytes(data[224:228], "little", signed=True)
    slot  = int.from_bytes(data[228:236], "little")
    return price * (10 ** expo), slot

def record(args) -> int:
    out = Path(args.out)
    out.write_text("")  # truncate
    targets = [args.pool] + list(PYTH_FEEDS.values())
    feed_keys = ["pool"] + list(PYTH_FEEDS.keys())
    n_ticks = int(args.duration / args.interval)
    print(f"# recording {n_ticks} ticks @ {args.interval}s into {args.out}", file=sys.stderr)
    t0 = time.time()
    for i in range(n_ticks):
        target_t = t0 + i * args.interval
        wait = target_t - time.time()
        if wait > 0: time.sleep(wait)
        try:
            res = rpc(args.rpc_url, "getMultipleAccounts",
                      [targets, {"encoding":"base64"}])
            values = res["result"]["value"]
            ts = time.time()
            pool_data = base64.b64decode(values[0]["data"][0]) if values[0] else None
            tick = {
                "i": i, "ts": ts,
                "pool_hex": pool_data.hex() if pool_data else None,
            }
            for k, v in zip(feed_keys[1:], values[1:]):
                if not v: continue
                d = base64.b64decode(v["data"][0])
                if len(d) >= 236:
                    px = int.from_bytes(d[208:216], "little", signed=True)
                    ex = int.from_bytes(d[224:228], "little", signed=True)
                    tick[k] = px * (10 ** ex)
            with out.open("a") as f:
                f.write(json.dumps(tick) + "\n")
            if i % 10 == 0:
                changed = "(first)" if i == 0 else ""
                print(f"  [{i:4d}/{n_ticks}] ts={ts:.2f} {changed}", file=sys.stderr)
        except Exception as e:
            print(f"  [{i:4d}] err: {e}", file=sys.stderr)
    print(f"# done. wrote {n_ticks} snapshots.", file=sys.stderr)
    return 0

# ── Pass 2: analyze ─────────────────────────────────────────────────────────

def pearson(xs: list[float], ys: list[float]) -> float:
    n = len(xs)
    if n < 3: return 0.0
    mx, my = sum(xs)/n, sum(ys)/n
    num = sum((x-mx)*(y-my) for x,y in zip(xs,ys))
    dx2 = sum((x-mx)**2 for x in xs)
    dy2 = sum((y-my)**2 for y in ys)
    den = math.sqrt(dx2 * dy2)
    return num / den if den > 0 else 0.0

def analyze(args) -> int:
    ticks = []
    for line in Path(args.input).read_text().splitlines():
        if not line.strip(): continue
        ticks.append(json.loads(line))
    if len(ticks) < 5:
        sys.exit("need ≥5 ticks")
    print(f"# {len(ticks)} ticks loaded", file=sys.stderr)

    pool_bytes = [bytes.fromhex(t["pool_hex"]) for t in ticks if t.get("pool_hex")]
    n = len(pool_bytes)
    size = len(pool_bytes[0])
    print(f"# pool account = {size} bytes", file=sys.stderr)

    # 1) byte-level change frequency
    changes = [0] * size
    for i in range(1, n):
        for off in range(size):
            if pool_bytes[i][off] != pool_bytes[i-1][off]:
                changes[off] += 1

    # 2) cluster contiguous "hot" runs
    print("\n## byte-level change frequency (hot ranges)\n", file=sys.stderr)
    in_run = False; run_start = 0
    runs = []
    for off in range(size):
        hot = changes[off] > 0
        if hot and not in_run: in_run = True; run_start = off
        elif not hot and in_run: in_run = False; runs.append((run_start, off-1))
    if in_run: runs.append((run_start, size-1))
    for s, e in runs:
        avg_chg = sum(changes[s:e+1]) / (e - s + 1)
        print(f"  bytes [{s:4d}–{e:4d}]  width={e-s+1:3d}  avg_changes/snap={avg_chg/n:.3f}", file=sys.stderr)

    # 3) for every 8-byte aligned u64-LE window that changed, build a time series
    series: dict[int, list[int]] = {}
    for off in range(0, size - 7, 8):
        vals = [int.from_bytes(b[off:off+8], "little") for b in pool_bytes]
        if len(set(vals)) >= 3:  # only varying windows
            series[off] = vals

    print(f"\n## {len(series)} varying u64-LE windows", file=sys.stderr)

    # 4) Pyth feeds: build aligned series (one per feed name)
    feeds: dict[str, list[float]] = {}
    for tick in ticks:
        for feed in PYTH_FEEDS:
            if feed in tick:
                feeds.setdefault(feed, []).append(tick[feed])

    # 5) correlate each u64 window with each Pyth feed
    print("\n## top Pearson |r| per Pyth feed vs u64 windows\n", file=sys.stderr)
    summary = {}
    for feed, fvals in feeds.items():
        if len(fvals) != n: continue
        scores = []
        for off, vals in series.items():
            f = [float(v) for v in vals]
            r = pearson(f, fvals)
            scores.append((off, abs(r), r))
        scores.sort(key=lambda x: -x[1])
        top = scores[:5]
        summary[feed] = [{"offset":o, "abs_r":round(ar,4), "r":round(r,4)} for o,ar,r in top]
        print(f"  {feed}", file=sys.stderr)
        for o, ar, r in top:
            ex = series[o][:3]
            print(f"    off={o:4d}  |r|={ar:.4f}  r={r:+.4f}  examples={ex}", file=sys.stderr)

    out = {
        "n_snapshots": n,
        "pool_size": size,
        "varying_u64_offsets": sorted(series.keys()),
        "byte_change_runs": runs,
        "pyth_correlation_top5": summary,
    }
    print(json.dumps(out, indent=2))
    return 0

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", required=True, choices=["record","analyze"])
    ap.add_argument("--rpc-url", default=DEFAULT_RPC)
    # record
    ap.add_argument("--pool", help="pool pubkey to watch")
    ap.add_argument("--duration", type=float, default=60.0)
    ap.add_argument("--interval", type=float, default=1.0)
    ap.add_argument("--out", help="output JSONL path")
    # analyze
    ap.add_argument("--input", help="input JSONL path")
    args = ap.parse_args()
    if args.mode == "record":
        if not args.pool or not args.out: sys.exit("record requires --pool and --out")
        if args.interval < 0.25: sys.exit("interval ≥ 0.25s (rate limit)")
        return record(args)
    else:
        if not args.input: sys.exit("analyze requires --input")
        return analyze(args)

if __name__ == "__main__":
    sys.exit(main())
