// prop-amm-coverage.ts — for each pair in `compare.ts`, report how many of the
// 10 prop AMMs currently host a pool on it. A high count = the pair is hot in
// the prop-MM circuit, which usually means tighter spreads available + more
// toxic flow risk.
//
// Stand-alone: does not depend on swap-compare's network layer. Reads the
// same snapshot the radar UI does (references/solana/prop-amm/...).

import 'dotenv/config';
import { readFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const INTEL_DIR = join(HERE, '..', '..', 'references', 'solana', 'prop-amm');
const LATEST = join(INTEL_DIR, 'snapshots', 'active-markets-latest.json');
const SAMPLE = join(INTEL_DIR, 'sample-active-markets.json');
const MINTS_FILE = join(INTEL_DIR, 'known-mints.json');

type Pool = { pubkey: string; mint1: string | null; mint2: string | null };
type Snap = { generated_at: string; by_amm: Record<string, Pool[]> };

const TOKENS: Record<string, string> = {
  SOL:  'So11111111111111111111111111111111111111112',
  USDC: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v',
  USDT: 'Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB',
  JUP:  'JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN',
  BONK: 'DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263',
  mSOL: 'mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So',
  WIF:  'EKpQGSJtjMFqKZ9KQanSqYXRcF8fBopzLHYxdM65zcjm',
  ORCA: 'orcaEKTdK7LKz57vaAYr9QeNsVEPfiu6QeMU1kektZE',
};

const PAIRS: Array<[string, string]> = [
  ['SOL',  'USDC'],
  ['JUP',  'USDC'],
  ['BONK', 'USDC'],
  ['mSOL', 'SOL' ],
  ['WIF',  'USDC'],
  ['USDC', 'USDT'],
];

function loadSnap(): { snap: Snap; from: 'live' | 'sample' } {
  if (existsSync(LATEST)) {
    return { snap: JSON.parse(readFileSync(LATEST, 'utf-8')), from: 'live' };
  }
  return { snap: JSON.parse(readFileSync(SAMPLE, 'utf-8')), from: 'sample' };
}

function ammsOnPair(snap: Snap, a: string, b: string): string[] {
  const hit: string[] = [];
  for (const [name, pools] of Object.entries(snap.by_amm)) {
    if (pools.some(p =>
      (p.mint1 === a && p.mint2 === b) || (p.mint1 === b && p.mint2 === a)
    )) hit.push(name);
  }
  return hit;
}

function main() {
  const { snap, from } = loadSnap();
  const age = Math.round((Date.now() - new Date(snap.generated_at).getTime()) / 1000);
  const allAmms = Object.keys(snap.by_amm).length;

  console.log(`prop-amm coverage  (snapshot: ${from}, ${age}s old, ${allAmms} AMMs scanned)\n`);
  console.log(['pair'.padEnd(14), 'count'.padStart(6), 'AMMs'].join('  '));
  console.log('-'.repeat(80));

  for (const [from_, to] of PAIRS) {
    const amms = ammsOnPair(snap, TOKENS[from_], TOKENS[to]);
    const label = `${from_}/${to}`.padEnd(14);
    const count = String(amms.length).padStart(6);
    console.log([label, count, amms.join(', ') || '—'].join('  '));
  }

  console.log('\nInterpretation:');
  console.log('  count ≥ 5 → hot pair; expect tight spreads + higher toxic-flow probability');
  console.log('  count 2-4 → competitive but reasonable');
  console.log('  count ≤ 1 → either niche or stale; check separately');
}

if (import.meta.url === `file://${process.argv[1]}`) main();
