import { Connection, Keypair, PublicKey, VersionedTransaction, TransactionMessage, SystemProgram, LAMPORTS_PER_SOL } from '@solana/web3.js';
import bs58 from 'bs58';
import dotenv from 'dotenv';
import express from 'express';

dotenv.config();

const connection = new Connection(process.env.RPC_URL!, 'confirmed');
const wallet = Keypair.fromSecretKey(bs58.decode(process.env.PRIVATE_KEY!));

const TOKENS = process.env.TOKENS!.split(',').map(m => new PublicKey(m.trim()));
const TARGET_RATIO = parseFloat(process.env.TARGET_INVENTORY_RATIO!);
const MAX_INVENTORY = parseFloat(process.env.MAX_INVENTORY!);
const MIN_SPREAD = parseFloat(process.env.MIN_SPREAD!);
const MAX_SPREAD = parseFloat(process.env.MAX_SPREAD!);
const MAX_SINGLE_TRADE_SOL = parseFloat(process.env.MAX_SINGLE_TRADE_SOL!);
const MAX_DAILY_LOSS = parseFloat(process.env.MAX_DAILY_LOSS!);
const JITO_TIP = parseFloat(process.env.JITO_TIP!) * LAMPORTS_PER_SOL;
const JITO_TIP_ACCOUNT = new PublicKey(
    process.env.JITO_TIP_ACCOUNT ?? '96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b'
);

// Pool account to read real AMM depth (optional — falls back to 0 if unset)
const AMM_POOL_PUBKEY = process.env.AMM_POOL_PUBKEY
    ? new PublicKey(process.env.AMM_POOL_PUBKEY)
    : null;
// Byte offset of reserve_a in the 304-byte pool layout
const OFF_RESERVE_A = 48;

let inventoryMap = new Map<string, number>();
let dailyPnL = 0;
let lastTotalBalance = 0;
let cycleCount = 0;
let consecutiveFailures = 0;
const MAX_CONSECUTIVE_FAILURES = parseInt(process.env.MAX_CONSECUTIVE_FAILURES ?? '5');
let botStartTime = Date.now();
const tradeLog: { ts: number; token: string; side: string; price: number; amount: number }[] = [];

// 轻量 Order Book（受 order_book_server 启发）
const orderBook = new Map<string, { bid: number; ask: number; depth: number }>();

// Pyth Hermes feed IDs (hex), comma-separated, same order as TOKENS
// e.g. PYTH_FEED_IDS=ef0d8b6f...,a19d04ac...
const PYTH_FEED_IDS: string[] = (process.env.PYTH_FEED_IDS ?? '').split(',').map(s => s.trim());

async function main() {
    console.log("🚀 Order Book Style Jupiter MM Bot starting...");

    lastTotalBalance = await getTotalSOLBalance();
    botStartTime = Date.now();

    startMetricsServer();

    setInterval(async () => {
        try {
            cycleCount++;
            await runMarketMakingCycle();
        } catch (err: any) {
            console.error("Cycle error:", err.message);
        }
    }, parseInt(process.env.UPDATE_INTERVAL!));
}

function startMetricsServer() {
    const app = express();
    const port = parseInt(process.env.METRICS_PORT ?? '3000');

    app.get('/health', (_req, res) => {
        res.json({
            status: 'running',
            uptime_s: Math.floor((Date.now() - botStartTime) / 1000),
            cycles: cycleCount,
        });
    });

    app.get('/metrics', (_req, res) => {
        const tokens: Record<string, object> = {};
        for (const [mint, inventory] of inventoryMap) {
            const book = orderBook.get(mint);
            tokens[mint] = {
                inventory,
                bid: book?.bid ?? null,
                ask: book?.ask ?? null,
                depth: book?.depth ?? null,
            };
        }
        res.json({
            wallet: wallet.publicKey.toBase58(),
            sol_balance: lastTotalBalance,
            daily_pnl_pct: (dailyPnL * 100).toFixed(4),
            cycles: cycleCount,
            uptime_s: Math.floor((Date.now() - botStartTime) / 1000),
            tokens,
        });
    });

    // SSE — real-time trade stream
    app.get('/events', (req, res) => {
        res.setHeader('Content-Type', 'text/event-stream');
        res.setHeader('Cache-Control', 'no-cache');
        res.setHeader('Connection', 'keep-alive');
        res.flushHeaders();

        // Send last 20 trades on connect
        for (const t of tradeLog.slice(-20)) {
            res.write(`data: ${JSON.stringify(t)}\n\n`);
        }

        tradeEmitter.on('trade', (t) => res.write(`data: ${JSON.stringify(t)}\n\n`));
        req.on('close', () => tradeEmitter.removeAllListeners('trade'));
    });

    app.listen(port, () => console.log(`Metrics: http://localhost:${port}/metrics`));
}

import { EventEmitter } from 'events';
const tradeEmitter = new EventEmitter();

async function runMarketMakingCycle() {
    const currentSOL = await getTotalSOLBalance();
    dailyPnL = (currentSOL - lastTotalBalance) / lastTotalBalance;

    if (dailyPnL < -MAX_DAILY_LOSS) {
        console.log("🚨 达到每日最大亏损，Bot 已停止");
        return;
    }

    for (const token of TOKENS) {
        await processToken(token);
    }

    lastTotalBalance = currentSOL;
}

async function processToken(tokenMint: PublicKey) {
    const inventory = await getTokenBalance(wallet.publicKey, tokenMint);
    inventoryMap.set(tokenMint.toBase58(), inventory);

    const midPrice = await getMidPrice(tokenMint);
    const volatility = await getPythVolatility(tokenMint);

    // Update lightweight order book — depth from on-chain AMM reserves (SOL units)
    const ammDepth = await getAmmDepth();
    orderBook.set(tokenMint.toBase58(), {
        bid: midPrice * 0.999,
        ask: midPrice * 1.001,
        depth: ammDepth,
    });

    const spread = calculateDynamicSpread(inventory, volatility, tokenMint);

    const bidPrice = midPrice * (1 - spread / 2);
    const askPrice = midPrice * (1 + spread / 2);

    console.log(`[${tokenMint.toBase58().slice(0,6)}] 库存:${inventory.toFixed(2)} | Vol:${(volatility*100).toFixed(1)}% | Spread:${(spread*100).toFixed(2)}%`);

    const maxTrade = Math.min(MAX_SINGLE_TRADE_SOL, MAX_INVENTORY * 0.15);

    if (inventory < MAX_INVENTORY * 0.72) await executeTrade(tokenMint, true, bidPrice, maxTrade);
    if (inventory > MAX_INVENTORY * 0.38) await executeTrade(tokenMint, false, askPrice, maxTrade);
}

// ==================== AMM depth from on-chain reserves ====================
async function getAmmDepth(): Promise<number> {
    if (!AMM_POOL_PUBKEY) return 0;
    try {
        const info = await connection.getAccountInfo(AMM_POOL_PUBKEY);
        if (!info || info.data.length < OFF_RESERVE_A + 8) return 0;
        // reserve_a is at OFF_RESERVE_A (48), stored as u64 little-endian
        const reserveA = Number(info.data.readBigUInt64LE(OFF_RESERVE_A));
        return reserveA / LAMPORTS_PER_SOL; // convert lamports → SOL
    } catch {
        return 0;
    }
}

// ==================== 动态 Spread（结合 Order Book 深度） ====================
function calculateDynamicSpread(inventory: number, volatility: number, tokenMint: PublicKey): number {
    const ratio = inventory / MAX_INVENTORY;
    let spread = MIN_SPREAD;

    // 库存倾斜
    if (ratio > TARGET_RATIO + 0.2) spread += 0.009;
    else if (ratio < TARGET_RATIO - 0.2) spread = Math.max(MIN_SPREAD, spread - 0.005);

    // Pyth 波动率
    spread *= (1 + volatility * 1.8);

    // Order Book 深度影响（深度浅 → spread 扩大）
    const book = orderBook.get(tokenMint.toBase58());
    if (book && book.depth < 50) spread += 0.004;

    return Math.min(MAX_SPREAD, Math.max(MIN_SPREAD, spread));
}

// ==================== 其余函数（可根据需要补全） ====================
async function getPythVolatility(tokenMint: PublicKey): Promise<number> {
    try {
        const idx = TOKENS.findIndex(t => t.equals(tokenMint));
        const feedId = idx >= 0 ? PYTH_FEED_IDS[idx] : undefined;
        if (!feedId) return 0.65;

        const resp = await fetch(
            `https://hermes.pyth.network/v2/updates/price/latest?ids[]=${feedId}&parsed=true`
        );
        if (!resp.ok) return 0.65;
        const data: any = await resp.json();
        const price = data?.parsed?.[0]?.price;
        if (!price) return 0.65;

        const conf = parseFloat(price.conf);
        const mid = Math.abs(parseFloat(price.price));
        if (!mid) return 0.65;

        // confidence/price → relative volatility, scaled to 0..2.5 range
        return Math.min(2.5, (conf / mid) * 100 / 50);
    } catch {
        console.warn("Pyth 获取失败，使用默认波动率");
        return 0.65;
    }
}

async function executeTrade(tokenMint: PublicKey, isBuy: boolean, price: number, maxAmount: number) {
    try {
        const SOL_MINT = new PublicKey('So11111111111111111111111111111111111111112');
        const inputMint = isBuy ? SOL_MINT : tokenMint;
        const outputMint = isBuy ? tokenMint : SOL_MINT;
        const amountLamports = Math.floor(maxAmount * LAMPORTS_PER_SOL);

        // 1. 从 Jupiter v6 获取最优路由
        const quoteResp = await fetch(
            `https://quote-api.jup.ag/v6/quote?inputMint=${inputMint}&outputMint=${outputMint}&amount=${amountLamports}&slippageBps=80`
        );
        if (!quoteResp.ok) throw new Error(`Jupiter quote error: ${quoteResp.status}`);
        const quoteData = await quoteResp.json();

        // 2. 获取 swap 交易体
        const swapResp = await fetch('https://quote-api.jup.ag/v6/swap', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                quoteResponse: quoteData,
                userPublicKey: wallet.publicKey.toBase58(),
                wrapAndUnwrapSol: true,
                dynamicComputeUnitLimit: true,
                prioritizationFeeLamports: 'auto',
            }),
        });
        if (!swapResp.ok) throw new Error(`Jupiter swap error: ${swapResp.status}`);
        const { swapTransaction } = await swapResp.json();

        const swapTx = VersionedTransaction.deserialize(Buffer.from(swapTransaction, 'base64'));

        // 3. Jito tip 交易
        const { blockhash } = await connection.getLatestBlockhash();
        const tipTx = new VersionedTransaction(
            new TransactionMessage({
                payerKey: wallet.publicKey,
                recentBlockhash: blockhash,
                instructions: [
                    SystemProgram.transfer({
                        fromPubkey: wallet.publicKey,
                        toPubkey: JITO_TIP_ACCOUNT,
                        lamports: JITO_TIP,
                    }),
                ],
            }).compileToV0Message()
        );

        swapTx.sign([wallet]);
        tipTx.sign([wallet]);

        // 4. Jito bundle 提交
        // Jito bundle via REST API (no SDK needed)
        const bundlePayload = {
            jsonrpc: '2.0',
            id: 1,
            method: 'sendBundle',
            params: [[swapTx, tipTx].map(tx => Buffer.from(tx.serialize()).toString('base64'))],
        };
        const jitoResp = await fetch('https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(bundlePayload),
        });
        if (!jitoResp.ok) throw new Error(`Jito bundle error: ${jitoResp.status}`);

        const entry = { ts: Date.now(), token: tokenMint.toBase58(), side: isBuy ? 'buy' : 'sell', price, amount: maxAmount };
        tradeLog.push(entry);
        if (tradeLog.length > 500) tradeLog.shift();
        tradeEmitter.emit('trade', entry);
        consecutiveFailures = 0; // reset on success
        console.log(`✅ [${tokenMint.toBase58().slice(0,6)}] ${isBuy ? 'buy' : 'sell'} ${maxAmount.toFixed(4)} SOL @ ${price.toFixed(8)}`);
    } catch (err: any) {
        consecutiveFailures++;
        console.error(`❌ executeTrade 失败 (${consecutiveFailures}/${MAX_CONSECUTIVE_FAILURES}):`, err.message);
        if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) {
            console.error(`🚨 circuit breaker: ${consecutiveFailures} consecutive failures — halting bot`);
            process.exit(1);
        }
    }
}

async function getTokenBalance(owner: PublicKey, mint: PublicKey): Promise<number> {
    try {
        const accounts = await connection.getParsedTokenAccountsByOwner(owner, { mint });
        if (accounts.value.length === 0) return 0;
        const amount = accounts.value[0].account.data.parsed.info.tokenAmount.uiAmount;
        return amount ?? 0;
    } catch {
        return 0;
    }
}

async function getTotalSOLBalance(): Promise<number> {
    const bal = await connection.getBalance(wallet.publicKey);
    return bal / LAMPORTS_PER_SOL;
}

async function getMidPrice(tokenMint: PublicKey): Promise<number> {
    const SOL_MINT = 'So11111111111111111111111111111111111111112';
    const ONE_SOL = 1_000_000_000; // 1 SOL in lamports
    const resp = await fetch(
        `https://quote-api.jup.ag/v6/quote?inputMint=${SOL_MINT}&outputMint=${tokenMint}&amount=${ONE_SOL}&slippageBps=0`
    );
    if (!resp.ok) throw new Error(`getMidPrice quote failed: ${resp.status}`);
    const data = await resp.json();
    // outAmount = how many tokens per 1 SOL
    const tokensPerSol = Number(data.outAmount);
    return tokensPerSol > 0 ? 1 / tokensPerSol : 0;
}

main().catch(console.error);
