/**
 * init-pool.js
 *
 * Creates two devnet token mints, funds user token accounts, creates pool
 * account, calls INIT_POOL, creates LP mint, then calls ADD_LIQUIDITY to
 * seed the pool.
 *
 * Usage:
 *   node init-pool.js
 *
 * Reads PRIVATE_KEY / PROGRAM_ID from ../bot/.env
 * Writes pool-keypair.json and pool-state.json for the other scripts.
 */

const {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
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
} = require("@solana/spl-token");
const bs58 = require("bs58");
const fs = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL = process.env.RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);

async function main() {
  const conn = new Connection(RPC_URL, "confirmed");

  const keyBytes = bs58.default
    ? bs58.default.decode(process.env.PRIVATE_KEY)
    : bs58.decode(process.env.PRIVATE_KEY);
  const payer = Keypair.fromSecretKey(keyBytes);
  console.log("Payer:", payer.publicKey.toBase58());

  // Airdrop if balance is low
  const bal = await conn.getBalance(payer.publicKey);
  if (bal < 0.5 * LAMPORTS_PER_SOL) {
    console.log("Airdropping 2 SOL...");
    const { blockhash, lastValidBlockHeight } =
      await conn.getLatestBlockhash();
    const sig = await conn.requestAirdrop(payer.publicKey, 2 * LAMPORTS_PER_SOL);
    await conn.confirmTransaction({ signature: sig, blockhash, lastValidBlockHeight });
    console.log("Airdrop done");
  }

  // ── 1. Token mints ────────────────────────────────────────────────────────
  console.log("\nCreating mint A...");
  const mintA = await createMint(conn, payer, payer.publicKey, null, 6);
  console.log("Mint A:", mintA.toBase58());

  console.log("Creating mint B...");
  const mintB = await createMint(conn, payer, payer.publicKey, null, 6);
  console.log("Mint B:", mintB.toBase58());

  // ── 2. Pool account + INIT_POOL ───────────────────────────────────────────
  const poolKeypair = Keypair.generate();
  console.log("\nPool keypair:", poolKeypair.publicKey.toBase58());

  const poolLamports = await conn.getMinimumBalanceForRentExemption(100);
  const createPoolIx = SystemProgram.createAccount({
    fromPubkey: payer.publicKey,
    newAccountPubkey: poolKeypair.publicKey,
    lamports: poolLamports,
    space: 100,
    programId: PROGRAM_ID,
  });

  const initPoolIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [{ pubkey: poolKeypair.publicKey, isSigner: false, isWritable: true }],
    data: Buffer.from([0]),
  });

  const tx1 = new Transaction().add(createPoolIx, initPoolIx);
  const sig1 = await sendAndConfirmTransaction(conn, tx1, [payer, poolKeypair]);
  console.log("Pool created + INIT_POOL:", sig1);

  // ── 3. Vaults (owned by pool keypair) ─────────────────────────────────────
  console.log("\nCreating vault A...");
  const vaultA = await createAccount(conn, payer, mintA, poolKeypair.publicKey);
  console.log("Vault A:", vaultA.toBase58());

  console.log("Creating vault B...");
  const vaultB = await createAccount(conn, payer, mintB, poolKeypair.publicKey);
  console.log("Vault B:", vaultB.toBase58());

  // ── 4. LP mint (pool keypair is mint authority) ────────────────────────────
  console.log("\nCreating LP mint...");
  const lpMint = await createMint(conn, payer, poolKeypair.publicKey, null, 6);
  console.log("LP mint:", lpMint.toBase58());

  // ── 5. User token accounts + initial balance ───────────────────────────────
  console.log("\nCreating user token accounts...");
  const userAAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintA, payer.publicKey);
  const userBAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintB, payer.publicKey);
  const userA = userAAcc.address;
  const userB = userBAcc.address;

  const SEED_AMOUNT = 1_000_000_000n; // 1000 tokens
  await mintTo(conn, payer, mintA, userA, payer, SEED_AMOUNT);
  await mintTo(conn, payer, mintB, userB, payer, SEED_AMOUNT);
  console.log(`Minted ${SEED_AMOUNT} of each token to user accounts`);

  // ── 6. User LP account ─────────────────────────────────────────────────────
  const userLpAcc = await getOrCreateAssociatedTokenAccount(conn, payer, lpMint, payer.publicKey);
  const userLp = userLpAcc.address;
  console.log("User LP account:", userLp.toBase58());

  // ── 7. ADD_LIQUIDITY ───────────────────────────────────────────────────────
  // Accounts (matches lib.rs add_liquidity):
  //   0  pool        writable
  //   1  user_a      writable
  //   2  vault_a     writable
  //   3  user_b      writable
  //   4  vault_b     writable
  //   5  lp_mint     writable
  //   6  user_lp     writable
  //   7  user        signer
  //   8  pool_auth   signer  (LP mint authority)
  const liquidityAmount = 500_000_000n;
  const addLiqData = Buffer.allocUnsafe(17);
  addLiqData.writeUInt8(3, 0);
  addLiqData.writeBigUInt64LE(liquidityAmount, 1);
  addLiqData.writeBigUInt64LE(liquidityAmount, 9);

  const addLiqIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: poolKeypair.publicKey, isSigner: false, isWritable: true  },
      { pubkey: userA,                 isSigner: false, isWritable: true  },
      { pubkey: vaultA,                isSigner: false, isWritable: true  },
      { pubkey: userB,                 isSigner: false, isWritable: true  },
      { pubkey: vaultB,                isSigner: false, isWritable: true  },
      { pubkey: lpMint,                isSigner: false, isWritable: true  },
      { pubkey: userLp,                isSigner: false, isWritable: true  },
      { pubkey: payer.publicKey,       isSigner: true,  isWritable: false },
      { pubkey: poolKeypair.publicKey, isSigner: true,  isWritable: false },
    ],
    data: addLiqData,
  });

  const tx2 = new Transaction().add(addLiqIx);
  const sig2 = await sendAndConfirmTransaction(conn, tx2, [payer, poolKeypair]);
  console.log("ADD_LIQUIDITY:", sig2);

  // ── 8. Save state ─────────────────────────────────────────────────────────
  fs.writeFileSync(
    "pool-keypair.json",
    JSON.stringify(Array.from(poolKeypair.secretKey))
  );

  const state = {
    pool:   poolKeypair.publicKey.toBase58(),
    mintA:  mintA.toBase58(),
    mintB:  mintB.toBase58(),
    vaultA: vaultA.toBase58(),
    vaultB: vaultB.toBase58(),
    lpMint: lpMint.toBase58(),
    userA:  userA.toBase58(),
    userB:  userB.toBase58(),
    userLp: userLp.toBase58(),
  };
  fs.writeFileSync("pool-state.json", JSON.stringify(state, null, 2));

  console.log("\n========== Add to bot/.env ==========");
  console.log(`POOL_PUBKEY=${poolKeypair.publicKey.toBase58()}`);
  console.log("======================================\n");

  console.log("pool-keypair.json  — keep this safe");
  console.log("pool-state.json    — read by add-liquidity.js / swap.js");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
