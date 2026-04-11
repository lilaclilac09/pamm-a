/**
 * init-pool.js
 *
 * Creates two devnet token mints, funds user token accounts, creates pool
 * account, calls INIT_POOL, then calls ADD_LIQUIDITY to seed the pool.
 *
 * Usage:
 *   PRIVATE_KEY=<base58> PROGRAM_ID=<pubkey> node init-pool.js
 *
 * Outputs POOL_PUBKEY and VAULT_A / VAULT_B for .env
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
  mintTo,
  getMinimumBalanceForRentExemptAccount,
  TOKEN_PROGRAM_ID,
} = require("@solana/spl-token");
const bs58 = require("bs58");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL =
  process.env.RPC_URL || "https://api.devnet.solana.com";
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
    const sig = await conn.requestAirdrop(payer.publicKey, 2 * LAMPORTS_PER_SOL);
    await conn.confirmTransaction(sig);
    console.log("Airdrop done");
  }

  // 1. Create token mints
  console.log("\nCreating mint A...");
  const mintA = await createMint(conn, payer, payer.publicKey, null, 6);
  console.log("Mint A:", mintA.toBase58());

  console.log("Creating mint B...");
  const mintB = await createMint(conn, payer, payer.publicKey, null, 6);
  console.log("Mint B:", mintB.toBase58());

  // 2. Create pool keypair — this is both the pool account and vault authority
  const poolKeypair = Keypair.generate();
  console.log("\nPool keypair:", poolKeypair.publicKey.toBase58());

  // 3. Create pool account (100 bytes, owned by our program)
  const poolLamports = await conn.getMinimumBalanceForRentExemption(100);
  const createPoolIx = SystemProgram.createAccount({
    fromPubkey: payer.publicKey,
    newAccountPubkey: poolKeypair.publicKey,
    lamports: poolLamports,
    space: 100,
    programId: PROGRAM_ID,
  });

  // 4. INIT_POOL instruction (discriminant 0, no extra data)
  const initPoolIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [{ pubkey: poolKeypair.publicKey, isSigner: false, isWritable: true }],
    data: Buffer.from([0]),
  });

  const tx1 = new Transaction().add(createPoolIx, initPoolIx);
  const sig1 = await sendAndConfirmTransaction(conn, tx1, [payer, poolKeypair]);
  console.log("Pool created + INIT_POOL:", sig1);

  // 5. Create token vaults owned by pool keypair
  console.log("\nCreating vault A...");
  const vaultA = await createAccount(conn, payer, mintA, poolKeypair.publicKey);
  console.log("Vault A:", vaultA.toBase58());

  console.log("Creating vault B...");
  const vaultB = await createAccount(conn, payer, mintB, poolKeypair.publicKey);
  console.log("Vault B:", vaultB.toBase58());

  // 6. Create user token accounts and mint some tokens
  console.log("\nCreating user token accounts...");
  const userA = await createAccount(conn, payer, mintA, payer.publicKey);
  const userB = await createAccount(conn, payer, mintB, payer.publicKey);

  const SEED_AMOUNT = 1_000_000_000n; // 1000 tokens (6 decimals)
  await mintTo(conn, payer, mintA, userA, payer, SEED_AMOUNT);
  await mintTo(conn, payer, mintB, userB, payer, SEED_AMOUNT);
  console.log(`Minted ${SEED_AMOUNT} of each token to user accounts`);

  // 7. ADD_LIQUIDITY — discriminant 3, [amount_a u64le, amount_b u64le]
  const ADD_LIQ = 3;
  const liquidityAmount = 500_000_000n; // 500 tokens each
  const addLiqData = Buffer.allocUnsafe(17);
  addLiqData.writeUInt8(ADD_LIQ, 0);
  addLiqData.writeBigUInt64LE(liquidityAmount, 1);
  addLiqData.writeBigUInt64LE(liquidityAmount, 9);

  const addLiqIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: poolKeypair.publicKey, isSigner: false, isWritable: true },
      { pubkey: userA,                 isSigner: false, isWritable: true },
      { pubkey: vaultA,                isSigner: false, isWritable: true },
      { pubkey: userB,                 isSigner: false, isWritable: true },
      { pubkey: vaultB,                isSigner: false, isWritable: true },
      { pubkey: payer.publicKey,       isSigner: true,  isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID,      isSigner: false, isWritable: false },
    ],
    data: addLiqData,
  });

  const tx2 = new Transaction().add(addLiqIx);
  const sig2 = await sendAndConfirmTransaction(conn, tx2, [payer]);
  console.log("ADD_LIQUIDITY:", sig2);

  // 8. Print .env values
  console.log("\n========== Add to bot/.env ==========");
  console.log(`PROGRAM_ID=${PROGRAM_ID.toBase58()}`);
  console.log(`POOL_PUBKEY=${poolKeypair.publicKey.toBase58()}`);
  console.log(`MINT_A=${mintA.toBase58()}`);
  console.log(`MINT_B=${mintB.toBase58()}`);
  console.log(`VAULT_A=${vaultA.toBase58()}`);
  console.log(`VAULT_B=${vaultB.toBase58()}`);
  console.log(`USER_A=${userA.toBase58()}`);
  console.log(`USER_B=${userB.toBase58()}`);
  console.log("======================================\n");

  // Save pool keypair for remove_liquidity / swap signing
  const fs = require("fs");
  fs.writeFileSync(
    "pool-keypair.json",
    JSON.stringify(Array.from(poolKeypair.secretKey))
  );
  console.log("Pool keypair saved to scripts/pool-keypair.json (keep this safe)");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
