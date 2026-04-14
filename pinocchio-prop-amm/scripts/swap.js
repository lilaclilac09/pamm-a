/**
 * swap.js
 *
 * Sends a SWAP instruction against the live pool and prints reserve changes.
 *
 * Usage:
 *   node swap.js [amount_in] [direction]
 *
 *   direction: "a_to_b" (default) or "b_to_a"
 *   amount_in: integer token units (default 10_000_000 = 10 tokens at 6 decimals)
 *
 * Reads pool addresses from pool-state.json. Ensure UPDATE_ORACLE has been
 * called recently (oracle must not be stale).
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
const fs = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL    = process.env.RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);
const POOL_PUBKEY = new PublicKey(process.env.POOL_PUBKEY);

const AMOUNT_IN = BigInt(process.argv[2] || "10000000");
const DIRECTION = process.argv[3] || "a_to_b"; // "a_to_b" or "b_to_a"
const MIN_OUT   = 0n;

// ── Pool layout byte offsets (must match state.rs) ───────────────────────────
const OFF_RESERVE_A = 48;
const OFF_RESERVE_B = 56;

function readReserves(data) {
  return {
    ra: data.readBigUInt64LE(OFF_RESERVE_A),
    rb: data.readBigUInt64LE(OFF_RESERVE_B),
  };
}

async function main() {
  const conn = new Connection(RPC_URL, "confirmed");

  const keyBytes = bs58.default
    ? bs58.default.decode(process.env.PRIVATE_KEY)
    : bs58.decode(process.env.PRIVATE_KEY);
  const payer = Keypair.fromSecretKey(keyBytes);

  const statePath = __dirname + "/pool-state.json";
  if (!fs.existsSync(statePath)) {
    console.error("pool-state.json not found — run init-pool.js first");
    process.exit(1);
  }
  const state = JSON.parse(fs.readFileSync(statePath));

  const vaultA = new PublicKey(state.vaultA);
  const vaultB = new PublicKey(state.vaultB);
  const userA  = new PublicKey(state.userA);
  const userB  = new PublicKey(state.userB);
  const poolAuthPda = new PublicKey(state.poolAuth);

  // Before
  const infoB = await conn.getAccountInfo(POOL_PUBKEY);
  if (!infoB) throw new Error("Pool account not found");
  const before = readReserves(Buffer.from(infoB.data));
  console.log(`\nBefore: reserve_a=${before.ra}  reserve_b=${before.rb}`);

  // SWAP = 2
  // Data (18 bytes): [2] + amount_in(8) + min_out(8) + direction(1)
  const dir = DIRECTION === "a_to_b" ? 0 : 1;
  const data = Buffer.allocUnsafe(18);
  data.writeUInt8(2, 0);
  data.writeBigUInt64LE(AMOUNT_IN, 1);
  data.writeBigUInt64LE(MIN_OUT, 9);
  data.writeUInt8(dir, 17);

  // Accounts (7): pool, user_in, vault_in, user_out, vault_out, user(signer), pool_auth(PDA)
  const [userIn, vaultIn, userOut, vaultOut] =
    DIRECTION === "a_to_b"
      ? [userA, vaultA, userB, vaultB]
      : [userB, vaultB, userA, vaultA];

  const ix = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: POOL_PUBKEY,       isSigner: false, isWritable: true  },
      { pubkey: userIn,            isSigner: false, isWritable: true  },
      { pubkey: vaultIn,           isSigner: false, isWritable: true  },
      { pubkey: userOut,           isSigner: false, isWritable: true  },
      { pubkey: vaultOut,          isSigner: false, isWritable: true  },
      { pubkey: payer.publicKey,   isSigner: true,  isWritable: false },
      { pubkey: poolAuthPda,       isSigner: false, isWritable: false },
    ],
    data,
  });

  console.log(`Swapping ${AMOUNT_IN} via ${DIRECTION}...`);
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ix), [payer]);
  console.log(`Swap tx (${DIRECTION}): ${sig}`);

  const infoA = await conn.getAccountInfo(POOL_PUBKEY);
  const after = readReserves(Buffer.from(infoA.data));
  console.log(`After:  reserve_a=${after.ra}  reserve_b=${after.rb}`);
  const da = BigInt(after.ra) - BigInt(before.ra);
  const db = BigInt(after.rb) - BigInt(before.rb);
  console.log(`Delta:  reserve_a ${da >= 0n ? "+" : ""}${da}  reserve_b ${db >= 0n ? "+" : ""}${db}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
