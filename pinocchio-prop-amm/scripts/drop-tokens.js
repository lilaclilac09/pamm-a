/**
 * drop-tokens.js
 *
 * Mints tokens into the user's A and B accounts, then prints a full
 * on-chain balance table for the wallet, pool vaults, and LP.
 *
 * Usage:
 *   node drop-tokens.js               # mint 1000 of each token (default)
 *   node drop-tokens.js --amount 5000 # mint 5000 of each token
 */

const {
  Connection,
  Keypair,
  PublicKey,
} = require("@solana/web3.js");
const {
  mintTo,
  getAccount,
} = require("@solana/spl-token");
const bs58 = require("bs58");
const fs   = require("fs");
require("dotenv").config({ path: "../bot/.env" });

const RPC_URL = process.env.RPC_URL || "https://api.devnet.solana.com";
const STATE   = JSON.parse(fs.readFileSync("pool-state.json", "utf8"));

const DECIMALS = 6;

async function main() {
  const args   = process.argv.slice(2);
  const amtIdx = args.indexOf("--amount");
  const mintAmt = amtIdx !== -1
    ? BigInt(Math.round(parseFloat(args[amtIdx + 1]) * 10 ** DECIMALS))
    : BigInt(1000 * 10 ** DECIMALS);

  const conn = new Connection(RPC_URL, "confirmed");

  const decode = bs58.default ? bs58.default.decode : bs58.decode;
  const payer  = Keypair.fromSecretKey(decode(process.env.PRIVATE_KEY));

  const mintA        = new PublicKey(STATE.mintA);
  const mintB        = new PublicKey(STATE.mintB);
  const userA        = new PublicKey(STATE.userA);
  const userB        = new PublicKey(STATE.userB);
  const vaultA       = new PublicKey(STATE.vaultA);
  const vaultB       = new PublicKey(STATE.vaultB);
  const userLp       = new PublicKey(STATE.userLp);
  const deadLp       = new PublicKey(STATE.deadLpAccount);

  const humanAmt = Number(mintAmt) / 10 ** DECIMALS;
  console.log(`\nMinting ${humanAmt} token A → ${userA.toBase58()}`);
  const sigA = await mintTo(conn, payer, mintA, userA, payer, mintAmt);
  console.log(`  sig: ${sigA}`);

  console.log(`Minting ${humanAmt} token B → ${userB.toBase58()}`);
  const sigB = await mintTo(conn, payer, mintB, userB, payer, mintAmt);
  console.log(`  sig: ${sigB}`);

  // ── Fetch on-chain state ────────────────────────────────────────────────────
  const [accUA, accUB, accVA, accVB, accLP, accDead] = await Promise.all([
    getAccount(conn, userA),
    getAccount(conn, userB),
    getAccount(conn, vaultA),
    getAccount(conn, vaultB),
    getAccount(conn, userLp),
    getAccount(conn, deadLp),
  ]);

  const fmt = (raw) => (Number(raw) / 10 ** DECIMALS).toFixed(6);

  console.log(`
╔══════════════════════════════════════════════════════════════════╗
║  On-chain token balances (devnet)                                ║
╠══════════════════════════════════════════════════════════════════╣
║  Wallet: ${payer.publicKey.toBase58()}  ║
╠══════════╦═══════════════════════════════════════╦══════════════╣
║  Account ║  Address                              ║  Balance     ║
╠══════════╬═══════════════════════════════════════╬══════════════╣
║  user_A  ║  ${userA.toBase58()}  ║  ${fmt(accUA.amount).padStart(12)} ║
║  user_B  ║  ${userB.toBase58()}  ║  ${fmt(accUB.amount).padStart(12)} ║
╠══════════╬═══════════════════════════════════════╬══════════════╣
║  vault_A ║  ${vaultA.toBase58()}  ║  ${fmt(accVA.amount).padStart(12)} ║
║  vault_B ║  ${vaultB.toBase58()}  ║  ${fmt(accVB.amount).padStart(12)} ║
╠══════════╬═══════════════════════════════════════╬══════════════╣
║  user_LP ║  ${userLp.toBase58()}  ║  ${fmt(accLP.amount).padStart(12)} ║
║  dead_LP ║  ${deadLp.toBase58()}  ║  ${fmt(accDead.amount).padStart(12)} ║
╚══════════╩═══════════════════════════════════════╩══════════════╝

  mint_A:  ${STATE.mintA}
  mint_B:  ${STATE.mintB}
  lp_mint: ${STATE.lpMint}
  pool:    ${STATE.pool}
`);
}

main().catch((e) => { console.error(e); process.exit(1); });
