/**
 * update-oracle.js
 *
 * Sends an UPDATE_ORACLE instruction to set the oracle price, spread, and
 * targets on the live pool.
 *
 * Usage:
 *   node update-oracle.js [oracle_price_1e9] [spread_bps] [vol_adj]
 *
 *   oracle_price_1e9  price scaled to 1e9 (e.g. 100000000000 = $100.00)
 *   spread_bps        base spread in bps (default: 10)
 *   vol_adj           vol-adjusted spread override in bps (default: same as spread_bps)
 *
 * Reads pool address and admin keypair from .env + pool-state.json.
 * Target A/B default to current pool reserves (read on-chain before sending).
 *
 * UPDATE_ORACLE data layout (41 bytes):
 *   [0]      discriminant = 1
 *   [1..9]   oracle_price  u64 le
 *   [9..13]  spread_bps    u32 le
 *   [13..17] vol_adj       u32 le
 *   [17..21] k_param       u32 le
 *   [21..25] fee_bps       u32 le
 *   [25..33] target_a      u64 le
 *   [33..41] target_b      u64 le
 */

const {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} = require("@solana/web3.js");
const bs58 = require("bs58");
const fs   = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL    = process.env.RPC_URL    || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);
const K_PARAM    = parseInt(process.env.K_PARAM  || "1000");
const FEE_BPS    = parseInt(process.env.FEE_BPS  || "5");

// CLI args
const ORACLE_PRICE_1E9 = BigInt(process.argv[2] || "0");
const SPREAD_BPS       = parseInt(process.argv[3] || process.env.BASE_SPREAD_BPS || "10");
const VOL_ADJ          = parseInt(process.argv[4] || SPREAD_BPS);

// ── Pool layout byte offsets (must match state.rs) ───────────────────────────
const OFF_RESERVE_A = 48;
const OFF_RESERVE_B = 56;

function readReserves(data) {
  return {
    ra: data.readBigUInt64LE(OFF_RESERVE_A),
    rb: data.readBigUInt64LE(OFF_RESERVE_B),
  };
}

function buildUpdateOracleData(oraclePrice, spreadBps, volAdj, kParam, feeBps, targetA, targetB) {
  const buf = Buffer.alloc(41);
  buf.writeUInt8(1, 0);                               // discriminant
  buf.writeBigUInt64LE(oraclePrice, 1);               // oracle_price
  buf.writeUInt32LE(spreadBps, 9);                    // spread_bps
  buf.writeUInt32LE(volAdj, 13);                      // vol_adj
  buf.writeUInt32LE(kParam, 17);                      // k_param
  buf.writeUInt32LE(feeBps, 21);                      // fee_bps
  buf.writeBigUInt64LE(targetA, 25);                  // target_a
  buf.writeBigUInt64LE(targetB, 33);                  // target_b
  return buf;
}

async function main() {
  if (ORACLE_PRICE_1E9 === 0n) {
    console.error("Usage: node update-oracle.js <oracle_price_1e9> [spread_bps] [vol_adj]");
    console.error("  oracle_price_1e9: price scaled to 1e9 (e.g. 170000000000 = $170.00 for SOL/USD)");
    process.exit(1);
  }

  const conn = new Connection(RPC_URL, "confirmed");

  // Load admin wallet
  const privKey = process.env.PRIVATE_KEY;
  if (!privKey) throw new Error("PRIVATE_KEY not set in .env");
  const adminKp = Keypair.fromSecretKey(bs58.decode(privKey));

  // Load pool state
  const state = JSON.parse(fs.readFileSync("pool-state.json", "utf8"));
  const poolPubkey = new PublicKey(state.pool);

  // Read current reserves from on-chain (use as targets unless overridden)
  const poolInfo = await conn.getAccountInfo(poolPubkey);
  if (!poolInfo) throw new Error("pool account not found");
  const { ra, rb } = readReserves(poolInfo.data);

  const targetA = ra;
  const targetB = rb;

  console.log("Pool:         ", poolPubkey.toBase58());
  console.log("Admin:        ", adminKp.publicKey.toBase58());
  console.log("Oracle price: ", ORACLE_PRICE_1E9.toString(), "(1e9 scale =", Number(ORACLE_PRICE_1E9) / 1e9, "USD)");
  console.log("spread_bps:   ", SPREAD_BPS);
  console.log("vol_adj:      ", VOL_ADJ);
  console.log("k_param:      ", K_PARAM);
  console.log("fee_bps:      ", FEE_BPS);
  console.log("target_a:     ", targetA.toString());
  console.log("target_b:     ", targetB.toString());

  const data = buildUpdateOracleData(
    ORACLE_PRICE_1E9,
    SPREAD_BPS,
    VOL_ADJ,
    K_PARAM,
    FEE_BPS,
    targetA,
    targetB,
  );

  const ix = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: poolPubkey,           isSigner: false, isWritable: true  },
      { pubkey: adminKp.publicKey,    isSigner: true,  isWritable: false },
    ],
    data,
  });

  const tx = new Transaction().add(ix);
  console.log("\nSending UPDATE_ORACLE...");
  const sig = await sendAndConfirmTransaction(conn, tx, [adminKp]);
  console.log("Confirmed:", sig);

  // Print updated pool state
  const updated = await conn.getAccountInfo(poolPubkey);
  const { ra: ra2, rb: rb2 } = readReserves(updated.data);
  console.log("\nPool reserves after:");
  console.log("  reserve_a:", ra2.toString());
  console.log("  reserve_b:", rb2.toString());
}

main().catch((e) => { console.error(e); process.exit(1); });
