/**
 * collect-fees.js
 *
 * Admin withdraws accumulated swap fees from the pool vaults.
 * Only the admin keypair (stored in pool state) can call this.
 *
 * Usage:
 *   node collect-fees.js
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

const OFF_FEE_A = 88;
const OFF_FEE_B = 96;

async function main() {
  const conn  = new Connection(RPC_URL, "confirmed");
  const keyBytes = bs58.default
    ? bs58.default.decode(process.env.PRIVATE_KEY)
    : bs58.decode(process.env.PRIVATE_KEY);
  const admin = Keypair.fromSecretKey(keyBytes);
  console.log("Admin:", admin.publicKey.toBase58());

  const statePath = __dirname + "/pool-state.json";
  if (!fs.existsSync(statePath)) {
    console.error("pool-state.json not found — run init-pool.js first");
    process.exit(1);
  }
  const state = JSON.parse(fs.readFileSync(statePath));

  const vaultA     = new PublicKey(state.vaultA);
  const vaultB     = new PublicKey(state.vaultB);
  const poolAuthPda = new PublicKey(state.poolAuth);
  const mintA      = new PublicKey(state.mintA);
  const mintB      = new PublicKey(state.mintB);

  // Admin's token accounts (receive the fees)
  const adminAAcc = await getOrCreateAssociatedTokenAccount(conn, admin, mintA, admin.publicKey);
  const adminBAcc = await getOrCreateAssociatedTokenAccount(conn, admin, mintB, admin.publicKey);
  const adminA = adminAAcc.address;
  const adminB = adminBAcc.address;

  // Show pending fees
  const info = await conn.getAccountInfo(POOL_PUBKEY);
  if (!info) throw new Error("Pool account not found");
  const d = Buffer.from(info.data);
  const feeA = Number(d.readBigUInt64LE(OFF_FEE_A));
  const feeB = Number(d.readBigUInt64LE(OFF_FEE_B));
  console.log(`Pending fees: A=${feeA}  B=${feeB}`);
  if (feeA === 0 && feeB === 0) {
    console.log("No fees to collect.");
    return;
  }

  // COLLECT_FEES (discriminant = 5)
  // Data (1 byte): [5]
  // Accounts (7): pool, vault_a, admin_a, vault_b, admin_b, admin(signer), pool_auth(PDA)
  const ix = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: POOL_PUBKEY,       isSigner: false, isWritable: true  },
      { pubkey: vaultA,            isSigner: false, isWritable: true  },
      { pubkey: adminA,            isSigner: false, isWritable: true  },
      { pubkey: vaultB,            isSigner: false, isWritable: true  },
      { pubkey: adminB,            isSigner: false, isWritable: true  },
      { pubkey: admin.publicKey,   isSigner: true,  isWritable: false },
      { pubkey: poolAuthPda,       isSigner: false, isWritable: false },
    ],
    data: Buffer.from([5]),
  });

  const sig = await sendAndConfirmTransaction(conn, new Transaction().add(ix), [admin]);
  console.log("COLLECT_FEES sig:", sig);
  console.log(`Collected: A=${feeA}  B=${feeB} → admin token accounts`);
}

main().catch((e) => { console.error(e); process.exit(1); });
