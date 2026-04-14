/**
 * add-liquidity.js
 *
 * Adds liquidity to an already-initialised pool.
 * Reads pool addresses from pool-state.json (written by init-pool.js).
 *
 * Usage:
 *   node add-liquidity.js [amount_a] [amount_b] [min_lp_out]
 *
 * Default: 100_000_000 each, min_lp_out = 0
 */

const {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} = require("@solana/web3.js");
const {
  getOrCreateAssociatedTokenAccount,
  getAccount,
  mintTo,
} = require("@solana/spl-token");
const bs58 = require("bs58");
const fs = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL    = process.env.RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);
const POOL_PUBKEY = new PublicKey(process.env.POOL_PUBKEY);
const STATE_FILE = "pool-state.json";

// ── Pool layout byte offsets (must match state.rs) ───────────────────────────
const OFF_RESERVE_A = 48;
const OFF_RESERVE_B = 56;
const OFF_LP_SUPPLY = 80;

function loadPayer() {
  const raw = process.env.PRIVATE_KEY;
  if (!raw) throw new Error("PRIVATE_KEY not set");
  const bytes = bs58.default ? bs58.default.decode(raw) : bs58.decode(raw);
  return Keypair.fromSecretKey(bytes);
}

function loadState() {
  if (!fs.existsSync(STATE_FILE))
    throw new Error(`${STATE_FILE} not found — run init-pool.js first`);
  return JSON.parse(fs.readFileSync(STATE_FILE));
}

function readReserves(data) {
  return {
    ra: Number(data.readBigUInt64LE(OFF_RESERVE_A)),
    rb: Number(data.readBigUInt64LE(OFF_RESERVE_B)),
    lp: Number(data.readBigUInt64LE(OFF_LP_SUPPLY)),
  };
}

async function main() {
  const amountA   = BigInt(process.argv[2] ?? "100000000");
  const amountB   = BigInt(process.argv[3] ?? "100000000");
  const minLpOut  = BigInt(process.argv[4] ?? "0");

  const conn   = new Connection(RPC_URL, "confirmed");
  const payer  = loadPayer();
  const state  = loadState();

  console.log("Payer:    ", payer.publicKey.toBase58());
  console.log("Pool:     ", POOL_PUBKEY.toBase58());
  console.log("PoolAuth: ", state.poolAuth);

  const mintA  = new PublicKey(state.mintA);
  const mintB  = new PublicKey(state.mintB);
  const vaultA = new PublicKey(state.vaultA);
  const vaultB = new PublicKey(state.vaultB);
  const lpMint = new PublicKey(state.lpMint);
  const poolAuthPda = new PublicKey(state.poolAuth);
  const deadLpAccount = new PublicKey(state.deadLpAccount);

  // Ensure user token accounts exist and are funded
  const userAAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintA, payer.publicKey);
  const userBAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintB, payer.publicKey);
  const userA = userAAcc.address;
  const userB = userBAcc.address;

  const accA = await getAccount(conn, userA);
  const accB = await getAccount(conn, userB);
  if (accA.amount < amountA) {
    await mintTo(conn, payer, mintA, userA, payer, amountA - accA.amount);
  }
  if (accB.amount < amountB) {
    await mintTo(conn, payer, mintB, userB, payer, amountB - accB.amount);
  }

  const userLpAcc = await getOrCreateAssociatedTokenAccount(conn, payer, lpMint, payer.publicKey);
  const userLp = userLpAcc.address;

  // Before
  const poolAccBefore = await conn.getAccountInfo(POOL_PUBKEY);
  if (!poolAccBefore) throw new Error("Pool account not found");
  const before = readReserves(Buffer.from(poolAccBefore.data));
  console.log(`\nReserves before: A=${before.ra}  B=${before.rb}  LP=${before.lp}`);

  // ADD_LIQUIDITY (discriminant = 3)
  // Data (25 bytes): [3] + amount_a(8) + amount_b(8) + min_lp_out(8)
  // Accounts (10): pool, user_a, vault_a, user_b, vault_b, lp_mint, user_lp,
  //                dead_lp_account, user(signer), pool_auth(PDA)
  const data = Buffer.allocUnsafe(25);
  data.writeUInt8(3, 0);
  data.writeBigUInt64LE(amountA, 1);
  data.writeBigUInt64LE(amountB, 9);
  data.writeBigUInt64LE(minLpOut, 17);

  const ix = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: POOL_PUBKEY,      isSigner: false, isWritable: true  },
      { pubkey: userA,            isSigner: false, isWritable: true  },
      { pubkey: vaultA,           isSigner: false, isWritable: true  },
      { pubkey: userB,            isSigner: false, isWritable: true  },
      { pubkey: vaultB,           isSigner: false, isWritable: true  },
      { pubkey: lpMint,           isSigner: false, isWritable: true  },
      { pubkey: userLp,           isSigner: false, isWritable: true  },
      { pubkey: deadLpAccount,    isSigner: false, isWritable: true  },
      { pubkey: payer.publicKey,  isSigner: true,  isWritable: false },
      { pubkey: poolAuthPda,      isSigner: false, isWritable: false },
    ],
    data,
  });

  console.log(`\nAdding liquidity: ${amountA} token A, ${amountB} token B, min_lp=${minLpOut}...`);
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ix), [payer]);
  console.log("ADD_LIQUIDITY sig:", sig);

  const poolAccAfter = await conn.getAccountInfo(POOL_PUBKEY);
  const after = readReserves(Buffer.from(poolAccAfter.data));
  console.log(`Reserves after:  A=${after.ra}  B=${after.rb}  LP=${after.lp}`);
  console.log(`LP minted this tx: ${after.lp - before.lp}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
