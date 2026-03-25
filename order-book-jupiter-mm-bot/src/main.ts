import { Connection, Keypair, PublicKey, VersionedTransaction, TransactionMessage, SystemProgram, LAMPORTS_PER_SOL } from '@solana/web3.js';
import { Jupiter } from '@jup-ag/api';
import { searcherClient } from '@jito-labs/jito-ts';
import { PythSolanaReceiver } from '@pythnetwork/pyth-solana-receiver';
import bs58 from 'bs58';
import dotenv from 'dotenv';

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

const pythReceiver = new PythSolanaReceiver({ connection, wallet });

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
        const priceFeed = await pythReceiver.getPriceFeed(tokenMint);
        if (!priceFeed) return 0.6;
        const price = priceFeed.getPriceUnchecked();
        const confidence = price.confidence || 0;
        const volatility = confidence / (price.price || 1) * 100;
        return Math.min(2.5, volatility / 50);
    } catch (e) {
        console.warn("Pyth 获取失败，使用默认波动率");
        return 0.65;
    }
}

async function executeTrade(tokenMint: PublicKey, isBuy: boolean, price: number, maxAmount: number) {
    // 这里可补全 Jupiter/Jito 交易逻辑
    // ...
}

async function getTokenBalance(owner: PublicKey, mint: PublicKey): Promise<number> {
    // 实际项目中需要完整实现，这里用随机数模拟
    return Math.random() * MAX_INVENTORY * 0.9;
}

async function getTotalSOLBalance(): Promise<number> {
    const bal = await connection.getBalance(wallet.publicKey);
    return bal / LAMPORTS_PER_SOL;
}

async function getMidPrice(tokenMint: PublicKey): Promise<number> {
    // 实际应从 Jupiter 获取，这里简化
    return 0.00001234;
}

main().catch(console.error);
