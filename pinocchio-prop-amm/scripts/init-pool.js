/**
 * init-pool.js
 *
 * Creates two devnet token mints, funds user token accounts, creates pool
 * account (304 bytes), calls INIT_POOL, then calls ADD_LIQUIDITY to seed
 * the pool.
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

const RPC_URL  = process.env.RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);

// Pool state size (must match state.rs POOL_SIZE)
const POOL_SIZE = 304;

async function main() {
  const conn = new Connection(RPC_URL, "confirmed");

  const keyBytes = bs58.default
    ? bs58.default.decode(process.env.PRIVATE_KEY)
    : bs58.decode(process.env.PRIVATE_KEY);
  const payer = Keypair.fromSecretKey(keyBytes);
  console.log("Payer:", payer.publicKey.toBase58());

  // Airdrop if needed
  const bal = await conn.getBalance(payer.publicKey);
  if (bal < 0.5 * LAMPORTS_PER_SOL) {
    console.log("Airdropping 2 SOL...");
    const { blockhash, lastValidBlockHeight } = await conn.getLatestBlockhash();
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

  // ── 2. Pool keypair + PDA derivation ──────────────────────────────────────
  const poolKeypair = Keypair.generate();
  const poolPubkey  = poolKeypair.publicKey;
  console.log("\nPool pubkey:", poolPubkey.toBase58());

  const [poolAuthPda, poolAuthBump] = await PublicKey.findProgramAddress(
    [Buffer.from("pool_auth"), poolPubkey.toBuffer()],
    PROGRAM_ID
  );
  console.log("Pool auth PDA:", poolAuthPda.toBase58(), "bump:", poolAuthBump);

  // ── 3. Create pool account (304 bytes) ────────────────────────────────────
  const poolLamports = await conn.getMinimumBalanceForRentExemption(POOL_SIZE);
  const createPoolIx = SystemProgram.createAccount({
    fromPubkey:      payer.publicKey,
    newAccountPubkey: poolPubkey,
    lamports:        poolLamports,
    space:           POOL_SIZE,
    programId:       PROGRAM_ID,
  });

  // ── 4. INIT_POOL instruction ───────────────────────────────────────────────
  //
  // Data (63 bytes total = 1 discriminant + 62 data bytes):
  //   [0]      discriminant = 0
  //   [1..33]  admin_pubkey  (32 bytes)
  //   [33]     auth_bump
  //   [34]     dead_lp_bump  (0 = unused in current version)
  //   [35..39] fee_bps       u32 le
  //   [39..43] k_param       u32 le
  //   [43..51] target_a      u64 le  (0 = set by oracle bot later)
  //   [51..59] target_b      u64 le
  //   [59..63] max_staleness u32 le
  const initData = Buffer.allocUnsafe(63);
  let off = 0;
  initData.writeUInt8(0, off++);                          // discriminant
  payer.publicKey.toBuffer().copy(initData, off); off += 32; // admin = payer
  initData.writeUInt8(poolAuthBump, off++);               // auth_bump
  initData.writeUInt8(0, off++);                          // dead_lp_bump (unused)
  initData.writeUInt32LE(5, off);    off += 4;            // fee_bps = 5
  initData.writeUInt32LE(1000, off); off += 4;            // k_param = 1000
  initData.writeBigUInt64LE(0n, off); off += 8;           // target_a = 0
  initData.writeBigUInt64LE(0n, off); off += 8;           // target_b = 0
  initData.writeUInt32LE(30, off);   off += 4;            // max_staleness = 30s

  const initPoolIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: poolPubkey,       isSigner: false, isWritable: true  }, // pool
      { pubkey: payer.publicKey,  isSigner: true,  isWritable: false }, // admin
    ],
    data: initData,
  });

  const tx1 = new Transaction().add(createPoolIx, initPoolIx);
  const sig1 = await sendAndConfirmTransaction(conn, tx1, [payer, poolKeypair]);
  console.log("Pool created + INIT_POOL:", sig1);

  // ── 5. Vaults (owned by pool_auth PDA) ────────────────────────────────────
  console.log("\nCreating vault A (owner = pool_auth PDA)...");
  const vaultA = await createAccount(conn, payer, mintA, poolAuthPda);
  console.log("Vault A:", vaultA.toBase58());

  console.log("Creating vault B (owner = pool_auth PDA)...");
  const vaultB = await createAccount(conn, payer, mintB, poolAuthPda);
  console.log("Vault B:", vaultB.toBase58());

  // ── 6. LP mint (pool_auth PDA is mint authority) ──────────────────────────
  console.log("\nCreating LP mint (pool_auth PDA is authority)...");
  const lpMint = await createMint(conn, payer, poolAuthPda, null, 6);
  console.log("LP mint:", lpMint.toBase58());

  // ── 7. Dead LP account (locks MINIMUM_LIQUIDITY=1000 on first deposit) ────
  // ATA of pool_auth PDA for LP mint — effectively burned (PDA has no key)
  console.log("Creating dead LP account (ATA of pool_auth for LP mint)...");
  const deadLpAcc = await getOrCreateAssociatedTokenAccount(
    conn, payer, lpMint, poolAuthPda,
    true /* allowOwnerOffCurve — pool_auth is a PDA */
  );
  const deadLpAccount = deadLpAcc.address;
  console.log("Dead LP account:", deadLpAccount.toBase58());

  // ── 8. User token accounts + initial balance ──────────────────────────────
  console.log("\nCreating user token accounts...");
  const userAAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintA, payer.publicKey);
  const userBAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintB, payer.publicKey);
  const userA = userAAcc.address;
  const userB = userBAcc.address;

  const SEED_AMOUNT = 1_000_000_000n; // 1000 tokens @ 6 decimals
  await mintTo(conn, payer, mintA, userA, payer, SEED_AMOUNT);
  await mintTo(conn, payer, mintB, userB, payer, SEED_AMOUNT);
  console.log(`Minted ${SEED_AMOUNT} of each token to user accounts`);

  // ── 9. User LP account ────────────────────────────────────────────────────
  const userLpAcc = await getOrCreateAssociatedTokenAccount(conn, payer, lpMint, payer.publicKey);
  const userLp = userLpAcc.address;
  console.log("User LP account:", userLp.toBase58());

  // ── 10. ADD_LIQUIDITY ─────────────────────────────────────────────────────
  //
  // Accounts (10):
  //   0  pool            writable
  //   1  user_a          writable
  //   2  vault_a         writable
  //   3  user_b          writable
  //   4  vault_b         writable
  //   5  lp_mint         writable
  //   6  user_lp         writable
  //   7  dead_lp_account writable
  //   8  user            signer
  //   9  pool_auth       (PDA, not a signer in the tx — signs via CPI)
  //
  // Data (25 bytes):
  //   [0]      discriminant = 3
  //   [1..9]   amount_a   u64
  //   [9..17]  amount_b   u64
  //   [17..25] min_lp_out u64 (0 = no slippage guard)
  const SEED_LIQ = 500_000_000n; // 500 tokens each
  const addLiqData = Buffer.allocUnsafe(25);
  addLiqData.writeUInt8(3, 0);
  addLiqData.writeBigUInt64LE(SEED_LIQ, 1);
  addLiqData.writeBigUInt64LE(SEED_LIQ, 9);
  addLiqData.writeBigUInt64LE(0n, 17); // min_lp_out = 0

  const addLiqIx = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: poolPubkey,       isSigner: false, isWritable: true  },
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
    data: addLiqData,
  });

  const tx2 = new Transaction().add(addLiqIx);
  const sig2 = await sendAndConfirmTransaction(conn, tx2, [payer]);
  console.log("ADD_LIQUIDITY:", sig2);

  // ── 11. Save state ────────────────────────────────────────────────────────
  fs.writeFileSync(
    "pool-keypair.json",
    JSON.stringify(Array.from(poolKeypair.secretKey))
  );

  const state = {
    pool:         poolPubkey.toBase58(),
    poolAuth:     poolAuthPda.toBase58(),
    poolAuthBump: poolAuthBump,
    mintA:        mintA.toBase58(),
    mintB:        mintB.toBase58(),
    vaultA:       vaultA.toBase58(),
    vaultB:       vaultB.toBase58(),
    lpMint:       lpMint.toBase58(),
    deadLpAccount: deadLpAccount.toBase58(),
    userA:        userA.toBase58(),
    userB:        userB.toBase58(),
    userLp:       userLp.toBase58(),
  };
  fs.writeFileSync("pool-state.json", JSON.stringify(state, null, 2));

  console.log("\n========== Add to bot/.env ==========");
  console.log(`POOL_PUBKEY=${poolPubkey.toBase58()}`);
  console.log("======================================\n");
  console.log("pool-keypair.json  — keep safe");
  console.log("pool-state.json    — read by all other scripts");
}

main().catch((e) => { console.error(e); process.exit(1); });
