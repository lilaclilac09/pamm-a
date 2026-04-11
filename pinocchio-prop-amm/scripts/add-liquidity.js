/**
 * add-liquidity.js
 *
 * Adds liquidity to an already-initialised pool.
 * Reads pool addresses from pool-state.json (written by init-pool.js).
 * Creates the LP mint and user LP account on first run.
 *
 * Usage:
 *   node add-liquidity.js [amount_a] [amount_b]
 *
 * Default amounts: 100_000_000 each (100 tokens @ 6 decimals)
 */

const {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
  LAMPORTS_PER_SOL,
} = require("@solana/web3.js");
const {
  createMint,
  createAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} = require("@solana/spl-token");
const bs58 = require("bs58");
const fs = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL = process.env.RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);
const POOL_PUBKEY = new PublicKey(process.env.POOL_PUBKEY);
const STATE_FILE = "pool-state.json";
const KEYPAIR_FILE = "pool-keypair.json";

// ─── helpers ──────────────────────────────────────────────────────────────────

function loadPayer() {
  const raw = process.env.PRIVATE_KEY;
  if (!raw) throw new Error("PRIVATE_KEY not set in bot/.env");
  const bytes = bs58.default ? bs58.default.decode(raw) : bs58.decode(raw);
  return Keypair.fromSecretKey(bytes);
}

function loadPoolKeypair() {
  if (!fs.existsSync(KEYPAIR_FILE)) {
    throw new Error(`${KEYPAIR_FILE} not found — run init-pool.js first`);
  }
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(KEYPAIR_FILE))));
}

function loadState() {
  if (!fs.existsSync(STATE_FILE)) {
    throw new Error(`${STATE_FILE} not found — run init-pool.js first`);
  }
  return JSON.parse(fs.readFileSync(STATE_FILE));
}

function saveState(state) {
  fs.writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));
}

function readReserves(data) {
  const ra = Number(data.readBigUInt64LE(8));
  const rb = Number(data.readBigUInt64LE(16));
  const lp = Number(data.readBigUInt64LE(24));
  return { ra, rb, lp };
}

// ─── main ─────────────────────────────────────────────────────────────────────

async function main() {
  const amountA = BigInt(process.argv[2] ?? "100000000");
  const amountB = BigInt(process.argv[3] ?? "100000000");

  const conn = new Connection(RPC_URL, "confirmed");
  const payer = loadPayer();
  const poolKeypair = loadPoolKeypair();
  const state = loadState();

  console.log("Payer:       ", payer.publicKey.toBase58());
  console.log("Pool:        ", POOL_PUBKEY.toBase58());
  console.log("Pool auth:   ", poolKeypair.publicKey.toBase58());

  const mintA  = new PublicKey(state.mintA);
  const mintB  = new PublicKey(state.mintB);
  const vaultA = new PublicKey(state.vaultA);
  const vaultB = new PublicKey(state.vaultB);

  // ── 1. Ensure LP mint exists ─────────────────────────────────────────────
  let lpMint;
  if (state.lpMint) {
    lpMint = new PublicKey(state.lpMint);
    console.log("LP mint (existing):", lpMint.toBase58());
  } else {
    console.log("\nCreating LP mint (pool authority is mint authority)...");
    lpMint = await createMint(conn, payer, poolKeypair.publicKey, null, 6);
    state.lpMint = lpMint.toBase58();
    saveState(state);
    console.log("LP mint:", lpMint.toBase58());
  }

  // ── 2. Ensure user token accounts exist and are funded ───────────────────
  let userA, userB;
  if (state.userA) {
    userA = new PublicKey(state.userA);
  } else {
    userA = (await getOrCreateAssociatedTokenAccount(conn, payer, mintA, payer.publicKey)).address;
    state.userA = userA.toBase58();
    saveState(state);
  }
  if (state.userB) {
    userB = new PublicKey(state.userB);
  } else {
    userB = (await getOrCreateAssociatedTokenAccount(conn, payer, mintB, payer.publicKey)).address;
    state.userB = userB.toBase58();
    saveState(state);
  }

  // Check balances and mint more if needed
  const accA = await getAccount(conn, userA);
  const accB = await getAccount(conn, userB);
  if (accA.amount < amountA) {
    const needed = amountA - accA.amount;
    console.log(`Minting ${needed} token A to user...`);
    await mintTo(conn, payer, mintA, userA, payer, needed);
  }
  if (accB.amount < amountB) {
    const needed = amountB - accB.amount;
    console.log(`Minting ${needed} token B to user...`);
    await mintTo(conn, payer, mintB, userB, payer, needed);
  }

  // ── 3. Ensure user LP account exists ────────────────────────────────────
  const userLpAcc = await getOrCreateAssociatedTokenAccount(conn, payer, lpMint, payer.publicKey);
  const userLp = userLpAcc.address;
  console.log("User LP account:", userLp.toBase58());

  // ── 4. Read current reserves ─────────────────────────────────────────────
  const poolAcc = await conn.getAccountInfo(POOL_PUBKEY);
  if (!poolAcc) throw new Error("Pool account not found");
  const { ra, rb, lp } = readReserves(Buffer.from(poolAcc.data));
  console.log(`\nReserves before: A=${ra}  B=${rb}  LP supply=${lp}`);

  // ── 5. Build ADD_LIQUIDITY instruction ───────────────────────────────────
  //
  // Accounts (matches lib.rs add_liquidity layout):
  //   0  pool        writable
  //   1  user_a      writable
  //   2  vault_a     writable
  //   3  user_b      writable
  //   4  vault_b     writable
  //   5  lp_mint     writable
  //   6  user_lp     writable
  //   7  user        signer
  //   8  pool_auth   signer  (mint authority)
  //
  // Data: [3] ++ amountA_le8 ++ amountB_le8

  const data = Buffer.allocUnsafe(17);
  data.writeUInt8(3, 0);
  data.writeBigUInt64LE(amountA, 1);
  data.writeBigUInt64LE(amountB, 9);

  const ix = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: POOL_PUBKEY,             isSigner: false, isWritable: true  },
      { pubkey: userA,                   isSigner: false, isWritable: true  },
      { pubkey: vaultA,                  isSigner: false, isWritable: true  },
      { pubkey: userB,                   isSigner: false, isWritable: true  },
      { pubkey: vaultB,                  isSigner: false, isWritable: true  },
      { pubkey: lpMint,                  isSigner: false, isWritable: true  },
      { pubkey: userLp,                  isSigner: false, isWritable: true  },
      { pubkey: payer.publicKey,         isSigner: true,  isWritable: false },
      { pubkey: poolKeypair.publicKey,   isSigner: true,  isWritable: false },
    ],
    data,
  });

  const tx = new Transaction().add(ix);

  console.log(`\nAdding liquidity: ${amountA} token A, ${amountB} token B...`);
  const sig = await sendAndConfirmTransaction(conn, tx, [payer, poolKeypair]);
  console.log("ADD_LIQUIDITY sig:", sig);

  // ── 6. Read reserves after ───────────────────────────────────────────────
  const poolAfter = await conn.getAccountInfo(POOL_PUBKEY);
  const after = readReserves(Buffer.from(poolAfter.data));
  console.log(`\nReserves after:  A=${after.ra}  B=${after.rb}  LP supply=${after.lp}`);
  console.log(`LP minted this tx: ${after.lp - lp}`);

  const userLpAfter = await getAccount(conn, userLp);
  console.log(`User LP balance:   ${userLpAfter.amount}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
