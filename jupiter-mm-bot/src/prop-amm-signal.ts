// prop-amm-signal.ts — compute the off-chain "flow pressure" signal that the
// flow-aware EWMA v3 strategy consumes (see ../../flow-aware-ewma/strategy.rs).
//
// Reads the latest snapshot produced by scripts/scan-prop-amm.sh and, for a
// given (mintA, mintB) pair, computes how many of the 10 prop AMMs are currently
// quoting it. Many competitors on the same pair = concentrated informed flow =
// fees should tighten.
//
// Output: u64 1e9-scaled, clamped to [0, 3_000_000] so contribution stays within
// the v3 strategy's budget (FLOW_MULT=4 → max 12 bps).

import { readFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const INTEL_DIR = join(HERE, '..', '..', 'references', 'solana', 'prop-amm');
const LATEST = join(INTEL_DIR, 'snapshots', 'active-markets-latest.json');
const SAMPLE = join(INTEL_DIR, 'sample-active-markets.json');

const PRESSURE_CAP = 3_000_000;       // 1e9-scaled, matches strategy.rs comment
const PRESSURE_SCALE = 1_000_000;     // tuned so n=10 → ~2.4e6, n=2 → ~1.1e6

export interface ActiveMarketsSnapshot {
  generated_at: string;
  current_slot: number;
  active_window_slots: number;
  by_amm: Record<string, Array<{ pubkey: string; mint1: string | null; mint2: string | null }>>;
}

let cached: ActiveMarketsSnapshot | null = null;
let cachedFrom: string | null = null;

function loadSnapshot(): ActiveMarketsSnapshot {
  const path = existsSync(LATEST) ? LATEST : SAMPLE;
  if (cached && cachedFrom === path) return cached;
  cached = JSON.parse(readFileSync(path, 'utf-8'));
  cachedFrom = path;
  return cached!;
}

export function refreshSnapshot(): ActiveMarketsSnapshot {
  cached = null;
  cachedFrom = null;
  return loadSnapshot();
}

/** Returns the set of AMM names currently quoting the (mintA, mintB) pair. */
export function ammsOnPair(mintA: string, mintB: string): string[] {
  const snap = loadSnapshot();
  const hit: string[] = [];
  for (const [amm, pools] of Object.entries(snap.by_amm)) {
    const found = pools.some(p =>
      (p.mint1 === mintA && p.mint2 === mintB) ||
      (p.mint1 === mintB && p.mint2 === mintA)
    );
    if (found) hit.push(amm);
  }
  return hit;
}

/**
 * Flow pressure for a given pair, 1e9-scaled u64 ready to send via tag 5.
 *
 *   pressure = clamp(SCALE * log(1 + n), 0, CAP)
 *
 * n is the count of distinct prop AMMs holding a pool on that pair right now.
 * The log shape means going from 1→3 AMMs is a bigger signal than 7→10.
 */
export function flowPressure(mintA: string, mintB: string): bigint {
  const n = ammsOnPair(mintA, mintB).length;
  if (n === 0) return 0n;
  const raw = PRESSURE_SCALE * Math.log(1 + n);
  return BigInt(Math.min(Math.round(raw), PRESSURE_CAP));
}

/** Diagnostic: what we'd push, and why. Suitable for stdout logging. */
export function explain(mintA: string, mintB: string): {
  pressure: bigint;
  amms: string[];
  ageSeconds: number;
  from: 'live' | 'sample';
} {
  const snap = loadSnapshot();
  const amms = ammsOnPair(mintA, mintB);
  return {
    pressure: flowPressure(mintA, mintB),
    amms,
    ageSeconds: Math.round((Date.now() - new Date(snap.generated_at).getTime()) / 1000),
    from: cachedFrom === LATEST ? 'live' : 'sample',
  };
}

// ── Standalone CLI for ad-hoc inspection ────────────────────────────────────
// node --loader ts-node/esm src/prop-amm-signal.ts <mintA> <mintB>
if (import.meta.url === `file://${process.argv[1]}`) {
  const [mintA, mintB] = process.argv.slice(2);
  if (!mintA || !mintB) {
    console.error('usage: prop-amm-signal.ts <mintA> <mintB>');
    process.exit(2);
  }
  console.log(JSON.stringify(explain(mintA, mintB), (_, v) =>
    typeof v === 'bigint' ? v.toString() : v, 2));
}
