import {
    Connection,
    Keypair,
    PublicKey,
    VersionedTransaction,
    TransactionMessage,
    SystemProgram,
    LAMPORTS_PER_SOL,
} from '@solana/web3.js';
import bs58 from 'bs58';
import dotenv from 'dotenv';

dotenv.config();

const connection = new Connection(process.env.RPC_URL!, 'confirmed');
const wallet = Keypair.fromSecretKey(bs58.decode(process.env.PRIVATE_KEY!));

const TARGET_TOKEN = new PublicKey(process.env.TARGET_TOKEN!);
const BASE_TOKEN   = new PublicKey(process.env.BASE_TOKEN!);

const TARGET_RATIO    = parseFloat(process.env.TARGET_INVENTORY_RATIO!);
const MAX_INVENTORY   = parseFloat(process.env.MAX_INVENTORY!);
const MIN_SPREAD      = parseFloat(process.env.MIN_SPREAD!);
const MAX_SPREAD      = parseFloat(process.env.MAX_SPREAD!);
const MAX_DAILY_LOSS  = parseFloat(process.env.MAX_DAILY_LOSS!);
const JITO_TIP        = parseFloat(process.env.JITO_TIP!) * LAMPORTS_PER_SOL;
const UPDATE_INTERVAL = parseInt(process.env.UPDATE_INTERVAL!);
const JITO_TIP_ACCOUNT = new PublicKey(
    process.env.JITO_TIP_ACCOUNT ?? '96gYZGLnJYVFmbjzopPSU6QiEV5fG3u3Z4M7o7G1z5b',
);

// Order sizes in lamports (input side). Base is SOL on buy, token on sell.
const BUY_AMOUNT_LAMPORTS  = Math.floor(
    parseFloat(process.env.BUY_SIZE_SOL ?? '0.05') * LAMPORTS_PER_SOL,
);
const SELL_AMOUNT_UNITS    = parseInt(process.env.SELL_SIZE_UNITS ?? '100000000');

let lastSOLBalance = 0;
let dailyPnL = 0;

async function main() {
    console.log('Jupiter MM bot starting');
    console.log(`  target: ${TARGET_TOKEN.toBase58().slice(0, 8)}…`);
    console.log(`  base:   ${BASE_TOKEN.toBase58().slice(0, 8)}…`);

    lastSOLBalance = await getSOLBalance();

    setInterval(async () => {
        try {
            await runMarketMakingCycle();
        } catch (err: any) {
            console.error('cycle error:', err.message);
        }
    }, UPDATE_INTERVAL);
}

async function runMarketMakingCycle() {
    const inventory = await getTokenBalance(wallet.publicKey, TARGET_TOKEN);
    const currentSOL = await getSOLBalance();

    dailyPnL = lastSOLBalance > 0 ? (currentSOL - lastSOLBalance) / lastSOLBalance : 0;
    if (dailyPnL < -MAX_DAILY_LOSS) {
        console.log('daily loss breaker tripped, pausing cycle');
        return;
    }

    const midPrice = await getMidPrice();
    if (!midPrice) {
        console.log('mid price unavailable, skipping');
        return;
    }

    const spread = calculateDynamicSpread(inventory);
    const bidPrice = midPrice * (1 - spread / 2);
    const askPrice = midPrice * (1 + spread / 2);

    console.log(
        `inv=${inventory.toFixed(2)} mid=${midPrice.toFixed(8)} ` +
        `spread=${(spread * 100).toFixed(2)}% bid=${bidPrice.toFixed(8)} ask=${askPrice.toFixed(8)}`,
    );

    if (inventory < MAX_INVENTORY * 0.75) {
        await executeTrade(true, bidPrice);
    }
    if (inventory > MAX_INVENTORY * 0.35) {
        await executeTrade(false, askPrice);
    }

    lastSOLBalance = currentSOL;
}

function calculateDynamicSpread(inventory: number): number {
    const ratio = inventory / MAX_INVENTORY;
    let spread = MIN_SPREAD;

    if (ratio > TARGET_RATIO + 0.18) {
        spread += 0.009;                                       // overweight → widen to push sells
    } else if (ratio < TARGET_RATIO - 0.18) {
        spread = Math.max(MIN_SPREAD, spread - 0.004);          // underweight → tighten to attract buys
    }

    return Math.min(MAX_SPREAD, Math.max(MIN_SPREAD, spread));
}

// ── Jupiter v6 REST ──────────────────────────────────────────────────────────

async function jupiterQuote(
    inputMint: PublicKey,
    outputMint: PublicKey,
    amount: number,
): Promise<any> {
    const url =
        `https://quote-api.jup.ag/v6/quote` +
        `?inputMint=${inputMint.toBase58()}` +
        `&outputMint=${outputMint.toBase58()}` +
        `&amount=${amount}` +
        `&slippageBps=80`;

    const resp = await fetch(url);
    if (!resp.ok) {
        throw new Error(`jupiter quote ${resp.status}: ${await resp.text()}`);
    }
    return resp.json();
}

async function jupiterSwapTx(quote: any): Promise<VersionedTransaction> {
    const resp = await fetch('https://quote-api.jup.ag/v6/swap', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
            quoteResponse: quote,
            userPublicKey: wallet.publicKey.toBase58(),
            wrapAndUnwrapSol: true,
            dynamicComputeUnitLimit: true,
            prioritizationFeeLamports: 'auto',
        }),
    });
    if (!resp.ok) {
        throw new Error(`jupiter swap ${resp.status}: ${await resp.text()}`);
    }
    const { swapTransaction } = (await resp.json()) as { swapTransaction: string };
    return VersionedTransaction.deserialize(Buffer.from(swapTransaction, 'base64'));
}

async function executeTrade(isBuy: boolean, _price: number) {
    try {
        const inputMint  = isBuy ? BASE_TOKEN   : TARGET_TOKEN;
        const outputMint = isBuy ? TARGET_TOKEN : BASE_TOKEN;
        const amount = isBuy ? BUY_AMOUNT_LAMPORTS : SELL_AMOUNT_UNITS;

        const quote  = await jupiterQuote(inputMint, outputMint, amount);
        const swapTx = await jupiterSwapTx(quote);
        swapTx.sign([wallet]);

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
            }).compileToV0Message(),
        );
        tipTx.sign([wallet]);

        const bundle = [swapTx, tipTx].map(tx =>
            Buffer.from(tx.serialize()).toString('base64'),
        );

        const jitoResp = await fetch(
            'https://ny.mainnet.block-engine.jito.wtf/api/v1/bundles',
            {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    jsonrpc: '2.0',
                    id: 1,
                    method: 'sendBundle',
                    params: [bundle],
                }),
            },
        );
        if (!jitoResp.ok) {
            throw new Error(`jito bundle ${jitoResp.status}: ${await jitoResp.text()}`);
        }

        console.log(`${isBuy ? 'buy' : 'sell'} bundle sent, out=${quote.outAmount}`);
    } catch (err: any) {
        console.error('trade failed:', err.message);
    }
}

// ── On-chain reads ───────────────────────────────────────────────────────────

async function getTokenBalance(owner: PublicKey, mint: PublicKey): Promise<number> {
    const accs = await connection.getParsedTokenAccountsByOwner(owner, { mint });
    if (accs.value.length === 0) return 0;
    return accs.value[0].account.data.parsed.info.tokenAmount.uiAmount ?? 0;
}

async function getSOLBalance(): Promise<number> {
    const lamports = await connection.getBalance(wallet.publicKey);
    return lamports / LAMPORTS_PER_SOL;
}

/// Mid price as TARGET-per-BASE, derived from a 1-unit quote through Jupiter.
async function getMidPrice(): Promise<number> {
    const probe = LAMPORTS_PER_SOL;           // 1 SOL (or 10^9 units of BASE) as probe
    const quote = await jupiterQuote(BASE_TOKEN, TARGET_TOKEN, probe);
    const outAmount = Number(quote.outAmount);
    if (!outAmount) return 0;
    return probe / outAmount;                  // TARGET per BASE unit
}

main().catch(console.error);
