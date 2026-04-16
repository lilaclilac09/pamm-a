/**
 * status.js
 *
 * Pretty-prints the full state of a live pool: reserves, LP supply,
 * oracle price, TWAP observations, pending fees, admin, and config params.
 *
 * Usage:
 *   node status.js
 *
 * Reads pool address from pool-state.json.
 */

const { Connection, PublicKey } = require("@solana/web3.js");
const fs = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL = process.env.RPC_URL || "https://api.devnet.solana.com";

// ── Pool layout byte offsets (must match state.rs) ───────────────────────────
const OFF_MAGIC        = 0;   // u32
const OFF_VERSION      = 4;   // u8
const OFF_ADMIN        = 8;   // [u8;32]
const OFF_AUTH_BUMP    = 40;  // u8
const OFF_DEAD_LP_BUMP = 41;  // u8
const OFF_RESERVE_A    = 48;  // u64
const OFF_RESERVE_B    = 56;  // u64
const OFF_TARGET_A     = 64;  // u64
const OFF_TARGET_B     = 72;  // u64
const OFF_LP_SUPPLY    = 80;  // u64
const OFF_FEE_A        = 88;  // u64
const OFF_FEE_B        = 96;  // u64
const OFF_ORACLE_PRICE = 104; // u64
const OFF_SPREAD_BPS   = 112; // u32
const OFF_K_PARAM      = 116; // u32
const OFF_FEE_BPS      = 120; // u32
const OFF_LAST_UPDATE  = 128; // i64
const OFF_TWAP_CURSOR  = 136; // u8
const OFF_TWAP_OBS     = 144; // 10 × (u64 price + i64 ts) = 160 bytes
const TWAP_SLOTS       = 10;
const POOL_SIZE        = 304;

const EXPECTED_MAGIC = 0x414d4d50; // "PMMA" little-endian

function r32(buf, off)  { return buf.readUInt32LE(off); }
function r64(buf, off)  { return buf.readBigUInt64LE(off); }
function ri64(buf, off) { return buf.readBigInt64LE(off); }
function pubkey(buf, off) {
  const { PublicKey } = require("@solana/web3.js");
  return new PublicKey(buf.slice(off, off + 32)).toBase58();
}

function fmtAge(unixTs) {
  if (unixTs === 0n) return "never";
  const now = BigInt(Math.floor(Date.now() / 1000));
  const age = now - unixTs;
  if (age < 0n) return `${-age}s in future`;
  if (age < 60n) return `${age}s ago`;
  return `${age / 60n}m ${age % 60n}s ago`;
}

async function main() {
  const conn = new Connection(RPC_URL, "confirmed");

  const state = JSON.parse(fs.readFileSync("pool-state.json", "utf8"));
  const poolPubkey = new PublicKey(state.pool);

  console.log("Pool:", poolPubkey.toBase58());
  console.log("RPC: ", RPC_URL);
  console.log("");

  const info = await conn.getAccountInfo(poolPubkey);
  if (!info) {
    console.error("Pool account not found on-chain.");
    process.exit(1);
  }

  const d = info.data;
  if (d.length < POOL_SIZE) {
    console.error(`Pool data too short: ${d.length} < ${POOL_SIZE}`);
    process.exit(1);
  }

  const magic = r32(d, OFF_MAGIC);
  if (magic !== EXPECTED_MAGIC) {
    console.error(`Bad magic: 0x${magic.toString(16)} (expected 0x${EXPECTED_MAGIC.toString(16)})`);
    process.exit(1);
  }

  const oraclePrice = r64(d, OFF_ORACLE_PRICE);
  const lastUpdate  = ri64(d, OFF_LAST_UPDATE);
  const twapCursor  = d[OFF_TWAP_CURSOR];

  console.log("── Identity ──────────────────────────────────────────────");
  console.log("  magic:          0x" + magic.toString(16).toUpperCase());
  console.log("  version:        " + d[OFF_VERSION]);
  console.log("  admin:          " + pubkey(d, OFF_ADMIN));
  console.log("  auth_bump:      " + d[OFF_AUTH_BUMP]);
  console.log("  dead_lp_bump:   " + d[OFF_DEAD_LP_BUMP]);

  console.log("");
  console.log("── Reserves ──────────────────────────────────────────────");
  console.log("  reserve_a:      " + r64(d, OFF_RESERVE_A).toLocaleString());
  console.log("  reserve_b:      " + r64(d, OFF_RESERVE_B).toLocaleString());
  console.log("  target_a:       " + r64(d, OFF_TARGET_A).toLocaleString());
  console.log("  target_b:       " + r64(d, OFF_TARGET_B).toLocaleString());
  console.log("  lp_supply:      " + r64(d, OFF_LP_SUPPLY).toLocaleString());

  console.log("");
  console.log("── Pending fees ──────────────────────────────────────────");
  console.log("  fee_a:          " + r64(d, OFF_FEE_A).toLocaleString());
  console.log("  fee_b:          " + r64(d, OFF_FEE_B).toLocaleString());

  console.log("");
  console.log("── Oracle ────────────────────────────────────────────────");
  const priceFloat = Number(oraclePrice) / 1e9;
  console.log("  oracle_price:   " + oraclePrice.toString() + "  (" + priceFloat.toFixed(4) + " USD)");
  console.log("  last_update:    " + fmtAge(lastUpdate));
  console.log("  spread_bps:     " + r32(d, OFF_SPREAD_BPS));
  console.log("  k_param:        " + r32(d, OFF_K_PARAM));
  console.log("  fee_bps:        " + r32(d, OFF_FEE_BPS));

  console.log("");
  console.log("── TWAP observations (cursor=" + twapCursor + ") ───────────────────────");
  for (let i = 0; i < TWAP_SLOTS; i++) {
    const base  = OFF_TWAP_OBS + i * 16;
    const price = r64(d, base);
    const ts    = ri64(d, base + 8);
    const marker = i === twapCursor ? " ← next write" : "";
    if (price === 0n) {
      console.log(`  [${i}] (empty)${marker}`);
    } else {
      const p = (Number(price) / 1e9).toFixed(4);
      console.log(`  [${i}] price=${price} (${p}) ts=${ts} ${fmtAge(ts)}${marker}`);
    }
  }
}

main().catch((e) => { console.error(e); process.exit(1); });
