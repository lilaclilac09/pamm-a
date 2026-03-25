import { Connection, Keypair, PublicKey, VersionedTransaction, TransactionMessage, SystemProgram, LAMPORTS_PER_SOL } from '@solana/web3.js';
import { Jupiter } from '@jup-ag/api';
import { searcherClient } from '@jito-labs/jito-ts';
import bs58 from 'bs58';
import dotenv from 'dotenv';

dotenv.config();

const connection = new Connection(process.env.RPC_URL!, 'confirmed');
const wallet = Keypair.fromSecretKey(bs58.decode(process.env.PRIVATE_KEY!));

const TARGET_TOKEN = new PublicKey(process.env.TARGET_TOKEN!);
const BASE_TOKEN = new PublicKey(process.env.BASE_TOKEN!);

const TARGET_RATIO = parseFloat(process.env.TARGET_INVENTORY_RATIO!);
const MAX_INVENTORY = parseFloat(process.env.MAX_INVENTORY!);
const MIN_SPREAD = parseFloat(process.env.MIN_SPREAD!);
const MAX_SPREAD = parseFloat(process.env.MAX_SPREAD!);
const MAX_DAILY_LOSS = parseFloat(process.env.MAX_DAILY_LOSS!);
const JITO_TIP = parseFloat(process.env.JITO_TIP!) * LAMPORTS_PER_SOL;

let currentInventory = 0;
let dailyPnL = 0;
let lastBalance = 0;

async function main() {
    console.log("🚀 Jupiter MM Bot（带库存倾斜 + 动态 Spread）启动");
    console.log(`目标代币: ${TARGET_TOKEN.toBase58().slice(0, 8)}...`);

    // 初始化余额
    lastBalance = await getSOLBalance();

    setInterval(async () => {
        try {
            await runMarketMakingCycle();
        } catch (err: any) {
            console.error("Cycle 出错:", err.message);
        }
    }, parseInt(process.env.UPDATE_INTERVAL!));
}

async function runMarketMakingCycle() {
    // 1. 实时库存监控
    currentInventory = await getTokenBalance(wallet.publicKey, TARGET_TOKEN);
    const currentSOL = await getSOLBalance();

    // 2. 每日亏损控制
    dailyPnL = (currentSOL - lastBalance) / lastBalance;
    if (dailyPnL < -MAX_DAILY_LOSS) {
        console.log("❌ 达到每日最大亏损，Bot 已暂停");
        return;
    }

    // 3. 获取中间价
    const midPrice = await getMidPrice();

    // 4. 计算动态 Spread + 库存倾斜
    const spread = calculateDynamicSpread(currentInventory);

    const bidPrice = midPrice * (1 - spread / 2);   // 买入价
    const askPrice = midPrice * (1 + spread / 2);   // 卖出价

    console.log(`库存: ${currentInventory.toFixed(2)} | Spread: ${(spread*100).toFixed(2)}% | Bid: ${bidPrice.toFixed(6)} | Ask: ${askPrice.toFixed(6)}`);

    // 5. 执行做市
    if (currentInventory < MAX_INVENTORY * 0.75) {
        await executeTrade(true, bidPrice);   // 买入
    }
    if (currentInventory > MAX_INVENTORY * 0.35) {
        await executeTrade(false, askPrice);  // 卖出
    }

    lastBalance = currentSOL;
}

// ==================== 动态 Spread + 库存倾斜 ====================
function calculateDynamicSpread(inventory: number): number {
    const ratio = inventory / MAX_INVENTORY;
    let spread = MIN_SPREAD;

    // 库存倾斜逻辑（核心）
    if (ratio > TARGET_RATIO + 0.18) {
        spread += 0.009;                    // 库存太多 → 扩大 spread，鼓励卖出
    } else if (ratio < TARGET_RATIO - 0.18) {
        spread = Math.max(MIN_SPREAD, spread - 0.004); // 库存太少 → 收紧 spread，鼓励买入
    }

    // 简单波动率模拟（可替换为 Pyth 实时数据）
    const volatility = 1.0 + Math.random() * 0.6;
    spread *= volatility;

    return Math.min(MAX_SPREAD, Math.max(MIN_SPREAD, spread));
}

// ==================== 执行交易（Jito Bundle） ====================
async function executeTrade(isBuy: boolean, price: number) {
    try {
        const jupiter = await Jupiter.load({ connection });

        const routes = await jupiter.computeRoutes({
            inputMint: isBuy ? BASE_TOKEN : TARGET_TOKEN,
            outputMint: isBuy ? TARGET_TOKEN : BASE_TOKEN,
            amount: isBuy ? 50000000 : 100000000, // 0.05 SOL 或 0.1 token
            slippageBps: 80,
        });

        if (!routes.routesInfos || routes.routesInfos.length === 0) return;

        const { swapTransaction } = await jupiter.swap({
            routeInfo: routes.routesInfos[0],
            userPublicKey: wallet.publicKey,
        });

        const tx = VersionedTransaction.deserialize(swapTransaction);

        // 添加 Jito Tip（放在最后一笔）
        const tipIx = SystemProgram.transfer({
            fromPubkey: wallet.publicKey,
            toPubkey: new PublicKey("96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b"),
            lamports: JITO_TIP,
        });

        const tipTx = new VersionedTransaction(
            new TransactionMessage({
                payerKey: wallet.publicKey,
                recentBlockhash: (await connection.getLatestBlockhash()).blockhash,
                instructions: [tipIx],
            }).compileToV0Message()
        );
        tipTx.sign([wallet]);

        const bundle = [tx, tipTx];

        const client = searcherClient("https://ny.mainnet.block-engine.jito.wtf");
        await client.sendBundle(bundle);

        console.log(`✅ ${isBuy ? "买入" : "卖出"} Bundle 发送成功 | 价格 ≈ ${price.toFixed(6)}`);
    } catch (err: any) {
        console.error("交易失败:", err.message);
    }
}

// 辅助函数
async function getTokenBalance(owner: PublicKey, mint: PublicKey): Promise<number> {
    // 实际项目中需要完整实现，这里用随机数模拟
    return Math.random() * MAX_INVENTORY * 0.9;
}

async function getSOLBalance(): Promise<number> {
    const balance = await connection.getBalance(wallet.publicKey);
    return balance / LAMPORTS_PER_SOL;
}

async function getMidPrice(): Promise<number> {
    // 实际从 Jupiter 获取，这里简化
    return 0.00001234;
}

main().catch(console.error);
