#!/usr/bin/env python3
"""humidifi-decode.py — second-pass analyzer for the trace JSONL produced by
humidifi-watch.py --mode record.

Goes deeper than humidifi-watch.py --mode analyze:
  · Maps EVERY changing byte, not just 8-byte aligned u64 windows.
  · For each contiguous "hot range", tries multiple interpretations
    (u8, u16-LE, u32-LE, u64-LE) and reports which one looks "structured":
      - monotonic increasing? (likely sequence / slot)
      - small relative delta per tick? (likely price tick / EMA)
      - bounded value? (likely flags / side / index)
  · Shows a per-tick timeline for the top changing fields.

Usage:
  python3 scripts/humidifi-decode.py --input /tmp/humidifi/trace-active.jsonl
"""
from __future__ import annotations
import argparse, json, sys
from pathlib import Path

def load_ticks(path: str) -> list[dict]:
    return [json.loads(l) for l in Path(path).read_text().splitlines() if l.strip()]

def hot_ranges(snapshots: list[bytes]) -> list[tuple[int,int,float]]:
    """Find contiguous runs of bytes that change at least once."""
    size = len(snapshots[0])
    n = len(snapshots)
    chgs = [0] * size
    for i in range(1, n):
        for off in range(size):
            if snapshots[i][off] != snapshots[i-1][off]: chgs[off] += 1
    runs = []
    in_run = False; rs = 0
    for off in range(size):
        if chgs[off] > 0 and not in_run: in_run = True; rs = off
        elif chgs[off] == 0 and in_run:
            in_run = False
            avg = sum(chgs[rs:off]) / (off - rs) / (n - 1)
            runs.append((rs, off - 1, avg))
    if in_run:
        avg = sum(chgs[rs:size]) / (size - rs) / (n - 1)
        runs.append((rs, size - 1, avg))
    return runs

def interpret_as(snapshots: list[bytes], off: int, width: int) -> list[int]:
    out = []
    for s in snapshots:
        out.append(int.from_bytes(s[off:off+width], "little"))
    return out

def is_monotonic(series: list[int]) -> bool:
    return all(series[i] <= series[i+1] for i in range(len(series)-1)) or \
           all(series[i] >= series[i+1] for i in range(len(series)-1))

def small_rel_delta(series: list[int]) -> bool:
    """All consecutive deltas < 5% of value."""
    for i in range(1, len(series)):
        if series[i-1] == 0: return False
        d = abs(series[i] - series[i-1]) / series[i-1]
        if d > 0.05: return False
    return True

def classify(series: list[int]) -> str:
    uniq = len(set(series))
    if uniq == 1:           return "constant"
    if is_monotonic(series): return "monotonic ↑"
    if small_rel_delta(series): return "smooth (price-like)"
    if uniq <= 8:           return f"discrete ({uniq} vals)"
    return "noisy"

def analyze_range(snapshots: list[bytes], lo: int, hi: int) -> dict:
    """Try width=1,2,4,8 starting at every offset in [lo, hi+1-width].
    Return per-width best candidates."""
    width_results = {}
    for width in (8, 4, 2, 1):
        if hi - lo + 1 < width: continue
        candidates = []
        for off in range(lo, hi + 2 - width):
            series = interpret_as(snapshots, off, width)
            uniq = len(set(series))
            if uniq < 2: continue
            cls = classify(series)
            candidates.append((off, uniq, cls, series[:5]))
        width_results[width] = candidates
    return width_results

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", required=True)
    args = ap.parse_args()
    ticks = load_ticks(args.input)
    pool_bytes = [bytes.fromhex(t["pool_hex"]) for t in ticks if t.get("pool_hex")]
    n = len(pool_bytes); size = len(pool_bytes[0])
    print(f"# {n} snapshots, pool account = {size} bytes\n")

    runs = hot_ranges(pool_bytes)
    print(f"## {len(runs)} hot ranges (bytes that ever changed)\n")
    print(f"  {'range':<14} {'width':>5}  {'change/snap':>11}")
    for s, e, avg in runs:
        print(f"  [{s:4d}–{e:4d}]   {e-s+1:>5}  {avg:>11.3f}")

    print(f"\n## structural interpretation per hot range\n")
    for s, e, _ in runs:
        print(f"### bytes [{s}–{e}]")
        widths = analyze_range(pool_bytes, s, e)
        # Per width, show top candidates by "interesting" classification
        for width in (8, 4, 2, 1):
            if width not in widths: continue
            cands = widths[width]
            # Prefer monotonic > smooth > discrete > noisy
            order = {"monotonic ↑":0, "smooth (price-like)":1}
            cands.sort(key=lambda x: (order.get(x[2], 2 + (10000 - x[1]) if "discrete" in x[2] else 100), x[0]))
            print(f"  width={width}: {len(cands)} varying positions")
            for off, uniq, cls, ex in cands[:3]:
                print(f"    off={off:4d}  uniq={uniq:3d}  class={cls:<22}  examples={ex}")
        print()

    # Aggregate stats
    total_changing_bytes = sum(e - s + 1 for s, e, _ in runs)
    print(f"## summary")
    print(f"  total bytes ever changing: {total_changing_bytes} / {size}  ({100*total_changing_bytes/size:.1f}%)")
    return 0

if __name__ == "__main__":
    sys.exit(main())
