/**
 * swap.js
 *
 * Sends a SWAP instruction against the live pool and prints reserve changes.
 *
 * Usage:
 *   POOL_PUBKEY=<> VAULT_A=<> VAULT_B=<> USER_A=<> USER_B=<> \
 *   PRIVATE_KEY=<base58> PROGRAM_ID=<> node swap.js [amount_in] [direction]
 *
 *   direction: "a_to_b" (default) or "b_to_a"
 *   amount_in: integer token units (default 10_000_000 = 10 tokens at 6 decimals)
 */

const {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} = require("@solana/web3.js");
const { TOKEN_PROGRAM_ID } = require("@solana/spl-token");
const bs58 = require("bs58");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL   = process.env.RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);
const POOL_PUBKEY = new PublicKey(process.env.POOL_PUBKEY);
const VAULT_A   = new PublicKey(process.env.VAULT_A);
const VAULT_B   = new PublicKey(process.env.VAULT_B);
const USER_A    = new PublicKey(process.env.USER_A);
const USER_B    = new PublicKey(process.env.USER_B);

const AMOUNT_IN   = BigInt(process.argv[2] || "10000000"); // 10 tokens
const DIRECTION   = process.argv[3] || "a_to_b"; // "a_to_b" or "b_to_a"
const MIN_OUT     = 0n; // no slippage guard for testing

async function readReserves(conn) {
  const info = await conn.getAccountInfo(POOL_PUBKEY);
  const d = info.data;
  return {
    reserve_in:  d.readBigUInt64LE(8),
    reserve_out: d.readBigUInt64LE(16),
  };
}

async function main() {
  const conn = new Connection(RPC_URL, "confirmed");

  const keyBytes = bs58.default
    ? bs58.default.decode(process.env.PRIVATE_KEY)
    : bs58.decode(process.env.PRIVATE_KEY);
  const payer = Keypair.fromSecretKey(keyBytes);

  // Load pool keypair for vault_out authority
  const fs = require("fs");
  const poolKpPath = __dirname + "/pool-keypair.json";
  if (!fs.existsSync(poolKpPath)) {
    console.error("pool-keypair.json not found — run init-pool.js first");
    process.exit(1);
  }
  const poolKeypair = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(fs.readFileSync(poolKpPath)))
  );

  // Before
  const before = await readReserves(conn);
  console.log(`\nBefore: reserve_a=${before.reserve_in}  reserve_b=${before.reserve_out}`);

  // SWAP = 2, [amount_in u64le, min_out u64le]
  const data = Buffer.allocUnsafe(17);
  data.writeUInt8(2, 0);
  data.writeBigUInt64LE(AMOUNT_IN, 1);
  data.writeBigUInt64LE(MIN_OUT,   9);

  // Direction: a→b uses vault_in=VAULT_A, vault_out=VAULT_B (and vice versa)
  const [userIn, vaultIn, userOut, vaultOut] =
    DIRECTION === "a_to_b"
      ? [USER_A, VAULT_A, USER_B, VAULT_B]
      : [USER_B, VAULT_B, USER_A, VAULT_A];

  const ix = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: POOL_PUBKEY,            isSigner: false, isWritable: true  },
      { pubkey: userIn,                 isSigner: false, isWritable: true  },
      { pubkey: vaultIn,                isSigner: false, isWritable: true  },
      { pubkey: userOut,                isSigner: false, isWritable: true  },
      { pubkey: vaultOut,               isSigner: false, isWritable: true  },
      { pubkey: payer.publicKey,        isSigner: true,  isWritable: false },
      { pubkey: poolKeypair.publicKey,  isSigner: true,  isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID,       isSigner: false, isWritable: false },
    ],
    data,
  });

  const tx = new Transaction().add(ix);
  const sig = await sendAndConfirmTransaction(conn, tx, [payer, poolKeypair]);
  console.log(`Swap tx (${DIRECTION}): ${sig}`);

  // After
  const after = await readReserves(conn);
  console.log(`After:  reserve_a=${after.reserve_in}  reserve_b=${after.reserve_out}`);
  console.log(`Delta:  reserve_a ${after.reserve_in - before.reserve_in > 0n ? "+" : ""}${after.reserve_in - before.reserve_in}  reserve_b ${after.reserve_out - before.reserve_out > 0n ? "+" : ""}${after.reserve_out - before.reserve_out}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
