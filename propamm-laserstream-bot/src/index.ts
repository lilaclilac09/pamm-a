import 'dotenv/config';
import { Connection, Keypair } from '@solana/web3.js';
import bs58 from 'bs58';
import { subscribe, CommitmentLevel, LaserstreamConfig, SubscribeUpdate } from 'helius-laserstream';
import { MONITORED_PROGRAMS } from './programs.js';
import { extractSignal, Signal } from './signal.js';
import { swap, getSignedSwapTx } from './jupiter.js';
import { createDcaOrder, getRecurringOrders } from './recurring.js';
import { sendViaJito, checkBundleStatus, getRecommendedTip } from './jito.js';
import { printBanner, logTx, logSignal, logTrade, logJito, logDca, logStats, logError } from './ui.js';

// ─── Config ────────────────────────────────────────────────────────────────
const NETWORK          = (process.env.NETWORK ?? 'mainnet') as 'mainnet' | 'devnet';
const HELIUS_API_KEY   = process.env.HELIUS_API_KEY!;
const JUPITER_API_KEY  = process.env.JUPITER_API_KEY!;
const PRIVATE_KEY      = process.env.PRIVATE_KEY!;
const INPUT_MINT       = process.env.INPUT_MINT!;
const OUTPUT_MINT      = process.env.OUTPUT_MINT!;
const TRADE_AMOUNT     = parseInt(process.env.TRADE_AMOUNT_LAMPORTS ?? '50000000');
const SLIPPAGE_BPS     = parseInt(process.env.SLIPPAGE_BPS ?? '50');
const REFERRAL_ACCOUNT = process.env.REFERRAL_ACCOUNT;
const REFERRAL_FEE_BPS = parseInt(process.env.REFERRAL_FEE_BPS ?? '50');
const USE_JITO         = process.env.USE_JITO !== 'false' && NETWORK === 'mainnet';
const JITO_REGION      = (process.env.JITO_REGION ?? 'ny') as any;

// DCA
const DCA_ENABLED       = process.env.DCA_ENABLED === 'true';
const DCA_TOTAL_AMOUNT  = parseInt(process.env.DCA_TOTAL_AMOUNT ?? '0');
const DCA_NUM_ORDERS    = parseInt(process.env.DCA_NUM_ORDERS ?? '10');
const DCA_INTERVAL_SECS = parseInt(process.env.DCA_INTERVAL_SECS ?? '3600');

// LaserStream endpoint by network
const LASERSTREAM_EP = NETWORK === 'devnet'
  ? `https://laserstream-devnet.helius-rpc.com`
  : (process.env.LASERSTREAM_ENDPOINT ?? 'https://laserstream-mainnet-tyo.helius-rpc.com');

// Helius RPC for tx construction
const RPC_URL = NETWORK === 'devnet'
  ? `https://devnet.helius-rpc.com/?api-key=${HELIUS_API_KEY}`
  : `https://mainnet.helius-rpc.com/?api-key=${HELIUS_API_KEY}`;

if (!HELIUS_API_KEY || !JUPITER_API_KEY || !PRIVATE_KEY) {
  console.error('Missing .env vars. See .env.example');
  process.exit(1);
}

const wallet     = Keypair.fromSecretKey(bs58.decode(PRIVATE_KEY));
const connection = new Connection(RPC_URL, 'confirmed');

// ─── State ─────────────────────────────────────────────────────────────────
let lastTrade  = 0;
let txCount    = 0;
let signalCount = 0;
let tradeCount  = 0;
const COOLDOWN_MS = 5_000;

// ─── DCA startup ───────────────────────────────────────────────────────────
async function startDca() {
  if (!DCA_ENABLED || DCA_TOTAL_AMOUNT === 0 || NETWORK === 'devnet') return;

  try {
    const existing = await getRecurringOrders(wallet.publicKey.toBase58(), JUPITER_API_KEY) as any;
    if ((existing?.orders?.length ?? 0) > 0) {
      logDca({ status: 'skipped' });
      return;
    }
  } catch { /* ok */ }

  const amtPerCycle = `${(DCA_TOTAL_AMOUNT / DCA_NUM_ORDERS / 1e6).toFixed(2)} units`;

  try {
    const result = await createDcaOrder({
      walletKeypair: wallet,
      inputMint: INPUT_MINT,
      outputMint: OUTPUT_MINT,
      totalInAmount: DCA_TOTAL_AMOUNT,
      numberOfOrders: DCA_NUM_ORDERS,
      intervalSeconds: DCA_INTERVAL_SECS,
      apiKey: JUPITER_API_KEY,
    });

    logDca({
      status: result.status === 'Success' ? 'created' : 'failed',
      order: result.order ?? undefined,
      signature: result.signature,
      amountPerCycle: amtPerCycle,
      cycles: DCA_NUM_ORDERS,
      error: result.error ?? undefined,
    });
  } catch (err: any) {
    logDca({ status: 'failed', error: err.message });
  }
}

// ─── Reactive trade ────────────────────────────────────────────────────────
async function onSignal(signal: Signal) {
  const now = Date.now();
  if (now - lastTrade < COOLDOWN_MS) return;

  logSignal(signal);

  if (signal.type === 'LARGE_SWAP' && signal.direction === 'SELL') return;

  const slippage = signal.type === 'NEW_POOL' ? 300 : SLIPPAGE_BPS;

  tradeCount++;
  lastTrade = now;
  logTrade({ status: 'pending', tradeCount });

  try {
    if (USE_JITO) {
      // Path A: get signed tx from Jupiter, then land via Jito bundle
      const { tx, order } = await getSignedSwapTx({
        inputMint: INPUT_MINT,
        outputMint: OUTPUT_MINT,
        amountLamports: TRADE_AMOUNT,
        walletKeypair: wallet,
        apiKey: JUPITER_API_KEY,
        slippageBps: slippage,
        referralAccount: REFERRAL_ACCOUNT,
        referralFeeBps: REFERRAL_ACCOUNT ? REFERRAL_FEE_BPS : undefined,
      });

      const tip = await getRecommendedTip('75th');
      const bundle = await sendViaJito({
        swapTx: tx, walletKeypair: wallet,
        region: JITO_REGION, tipLamports: tip, connection,
      });

      logJito({ bundleId: bundle.bundleId, tipLamports: bundle.tipLamports, status: 'sent' });

      // Poll status after 2s
      setTimeout(async () => {
        const status = await checkBundleStatus(bundle.bundleId, JITO_REGION);
        logJito({ bundleId: bundle.bundleId, tipLamports: bundle.tipLamports, status });
      }, 2000);

      logTrade({
        status: 'success',
        signature: bundle.bundleId,
        inputAmount: TRADE_AMOUNT.toString(),
        outputAmount: order.outAmount,
        router: order.router,
        tipLamports: tip,
        tradeCount,
      });

    } else {
      // Path B: devnet / no Jito — use Jupiter /execute directly
      const result = await swap({
        inputMint: INPUT_MINT,
        outputMint: OUTPUT_MINT,
        amountLamports: TRADE_AMOUNT,
        walletKeypair: wallet,
        apiKey: JUPITER_API_KEY,
        slippageBps: slippage,
        referralAccount: REFERRAL_ACCOUNT,
        referralFeeBps: REFERRAL_ACCOUNT ? REFERRAL_FEE_BPS : undefined,
      });

      logTrade({
        status: result.status === 'Success' ? 'success' : 'failed',
        signature: result.signature,
        inputAmount: result.inputAmountResult,
        outputAmount: result.outputAmountResult,
        error: result.error,
        tradeCount,
      });
    }
  } catch (err: any) {
    logTrade({ status: 'failed', error: err.message, tradeCount });
  }
}

// ─── Main ──────────────────────────────────────────────────────────────────
async function main() {
  printBanner(NETWORK);

  console.log(`  Wallet : ${wallet.publicKey.toBase58()}`);
  console.log(`  RPC    : ${RPC_URL.split('?')[0]}`);
  console.log(`  Jito   : ${USE_JITO ? `✓ (${JITO_REGION})` : '✗ (devnet — standard RPC)'}`);
  console.log(`  Referral: ${REFERRAL_ACCOUNT ? `✓ ${REFERRAL_FEE_BPS}bps → ${REFERRAL_ACCOUNT.slice(0, 8)}...` : '✗ not set'}`);
  console.log(`  DCA    : ${DCA_ENABLED ? `✓ ${DCA_TOTAL_AMOUNT / 1e6} USDC over ${DCA_NUM_ORDERS} cycles` : '✗'}\n`);

  await startDca();

  const config: LaserstreamConfig = {
    apiKey: HELIUS_API_KEY,
    endpoint: LASERSTREAM_EP,
    maxReconnectAttempts: 50,
  };

  const request = {
    transactions: {
      'dex-monitor': {
        vote: false,
        failed: false,
        accountInclude: MONITORED_PROGRAMS,
        accountExclude: [],
        accountRequired: [],
      },
    },
    commitment: CommitmentLevel.PROCESSED,
    accounts: {},
    slots: {},
    transactionsStatus: {},
    blocks: {},
    blocksMeta: {},
    entry: {},
    accountsDataSlice: [],
  };

  const stream = await subscribe(
    config,
    request,
    async (update: SubscribeUpdate) => {
      txCount++;
      logTx(txCount, '');  // live rolling counter

      const signal = extractSignal(update);
      if (signal) {
        signalCount++;
        await onSignal(signal);
      }
    },
    (error: Error) => logError(error.message)
  );

  setInterval(() => logStats({
    uptime: process.uptime(),
    txCount, signalCount, tradeCount,
    network: NETWORK,
  }), 60_000);

  process.on('SIGINT', () => {
    logStats({ uptime: process.uptime(), txCount, signalCount, tradeCount, network: NETWORK });
    stream.cancel();
    process.exit(0);
  });
}

main().catch(err => {
  logError(err.message);
  process.exit(1);
});
