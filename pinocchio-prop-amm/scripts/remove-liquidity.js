/**
 * remove-liquidity.js
 *
 * Burns LP tokens and withdraws proportional token A + B from the pool.
 * LPs cannot withdraw the admin fee portion — only net reserves.
 *
 * Usage:
 *   node remove-liquidity.js [lp_amount] [min_out_a] [min_out_b]
 *
 * Default: lp_amount=100_000_000, min_out_a=0, min_out_b=0
 */

const {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} = require("@solana/web3.js");
const { getOrCreateAssociatedTokenAccount } = require("@solana/spl-token");
const bs58 = require("bs58");
const fs = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL    = process.env.RPC_URL || "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.PROGRAM_ID);
const POOL_PUBKEY = new PublicKey(process.env.POOL_PUBKEY);

const OFF_RESERVE_A = 48;
const OFF_RESERVE_B = 56;
const OFF_LP_SUPPLY = 80;
const OFF_FEE_A     = 88;
const OFF_FEE_B     = 96;

function readPoolState(data) {
  return {
    ra:  Number(data.readBigUInt64LE(OFF_RESERVE_A)),
    rb:  Number(data.readBigUInt64LE(OFF_RESERVE_B)),
    lp:  Number(data.readBigUInt64LE(OFF_LP_SUPPLY)),
    fa:  Number(data.readBigUInt64LE(OFF_FEE_A)),
    fb:  Number(data.readBigUInt64LE(OFF_FEE_B)),
  };
}

async function main() {
  const lpAmount = BigInt(process.argv[2] ?? "100000000");
  const minOutA  = BigInt(process.argv[3] ?? "0");
  const minOutB  = BigInt(process.argv[4] ?? "0");

  const conn  = new Connection(RPC_URL, "confirmed");
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

  const vaultA     = new PublicKey(state.vaultA);
  const vaultB     = new PublicKey(state.vaultB);
  const lpMint     = new PublicKey(state.lpMint);
  const poolAuthPda = new PublicKey(state.poolAuth);

  const mintA = new PublicKey(state.mintA);
  const mintB = new PublicKey(state.mintB);
  const userAAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintA, payer.publicKey);
  const userBAcc = await getOrCreateAssociatedTokenAccount(conn, payer, mintB, payer.publicKey);
  const userLpAcc = await getOrCreateAssociatedTokenAccount(conn, payer, lpMint, payer.publicKey);
  const userA  = userAAcc.address;
  const userB  = userBAcc.address;
  const userLp = userLpAcc.address;

  // Before
  const infoBefore = await conn.getAccountInfo(POOL_PUBKEY);
  const before = readPoolState(Buffer.from(infoBefore.data));
  console.log(`Pool before: ra=${before.ra} rb=${before.rb} lp=${before.lp} fee_a=${before.fa} fee_b=${before.fb}`);

  // REMOVE_LIQUIDITY (discriminant = 4)
  // Data (25 bytes): [4] + lp_amount(8) + min_out_a(8) + min_out_b(8)
  // Accounts (9): pool, vault_a, user_a, vault_b, user_b, lp_mint, user_lp, pool_auth(PDA), user(signer)
  const data = Buffer.allocUnsafe(25);
  data.writeUInt8(4, 0);
  data.writeBigUInt64LE(lpAmount, 1);
  data.writeBigUInt64LE(minOutA, 9);
  data.writeBigUInt64LE(minOutB, 17);

  const ix = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: POOL_PUBKEY,     isSigner: false, isWritable: true  },
      { pubkey: vaultA,          isSigner: false, isWritable: true  },
      { pubkey: userA,           isSigner: false, isWritable: true  },
      { pubkey: vaultB,          isSigner: false, isWritable: true  },
      { pubkey: userB,           isSigner: false, isWritable: true  },
      { pubkey: lpMint,          isSigner: false, isWritable: true  },
      { pubkey: userLp,          isSigner: false, isWritable: true  },
      { pubkey: poolAuthPda,     isSigner: false, isWritable: false },
      { pubkey: payer.publicKey, isSigner: true,  isWritable: false },
    ],
    data,
  });

  console.log(`\nRemoving ${lpAmount} LP tokens (min_a=${minOutA} min_b=${minOutB})...`);
  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ix), [payer]);
  console.log("REMOVE_LIQUIDITY sig:", sig);

  const infoAfter = await conn.getAccountInfo(POOL_PUBKEY);
  const after = readPoolState(Buffer.from(infoAfter.data));
  console.log(`Pool after:  ra=${after.ra} rb=${after.rb} lp=${after.lp}`);
  console.log(`Withdrawn:   A=${before.ra - after.ra}  B=${before.rb - after.rb}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
