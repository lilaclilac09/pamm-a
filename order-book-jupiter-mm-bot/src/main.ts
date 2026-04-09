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

let inventoryMap = new Map<string, number>();
let dailyPnL = 0;
let lastTotalBalance = 0;

// 轻量 Order Book（受 order_book_server 启发）
const orderBook = new Map<string, { bid: number; ask: number; depth: number }>();

// Pyth Hermes feed IDs (hex), comma-separated, same order as TOKENS
// e.g. PYTH_FEED_IDS=ef0d8b6f...,a19d04ac...
const PYTH_FEED_IDS: string[] = (process.env.PYTH_FEED_IDS ?? '').split(',').map(s => s.trim());

async function main() {
    console.log("🚀 Order Book Style Jupiter MM Bot 已启动（受 lilaclilac09/order_book_server 启发）");

    lastTotalBalance = await getTotalSOLBalance();

    setInterval(async () => {
        try {
            await runMarketMakingCycle();
        } catch (err: any) {
            console.error("Cycle 错误:", err.message);
        }
    }, parseInt(process.env.UPDATE_INTERVAL!));
}

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

    // 更新轻量 Order Book
    orderBook.set(tokenMint.toBase58(), {
        bid: midPrice * 0.999,
        ask: midPrice * 1.001,
        depth: Math.random() * 100 + 20   // 模拟深度，实际可接 Jupiter Depth
    });

    const spread = calculateDynamicSpread(inventory, volatility, tokenMint);

    const bidPrice = midPrice * (1 - spread / 2);
    const askPrice = midPrice * (1 + spread / 2);

    console.log(`[${tokenMint.toBase58().slice(0,6)}] 库存:${inventory.toFixed(2)} | Vol:${(volatility*100).toFixed(1)}% | Spread:${(spread*100).toFixed(2)}%`);

    const maxTrade = Math.min(MAX_SINGLE_TRADE_SOL, MAX_INVENTORY * 0.15);

    if (inventory < MAX_INVENTORY * 0.72) await executeTrade(tokenMint, true, bidPrice, maxTrade);
    if (inventory > MAX_INVENTORY * 0.38) await executeTrade(tokenMint, false, askPrice, maxTrade);
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
        const JITO_TIP_ACCOUNT = new PublicKey('96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b');
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

        console.log(`✅ [${tokenMint.toBase58().slice(0,6)}] ${isBuy ? '买入' : '卖出'} ${maxAmount.toFixed(4)} SOL @ ${price.toFixed(8)}`);
    } catch (err: any) {
        console.error(`❌ executeTrade 失败:`, err.message);
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
