//! Async add_liq that uses our reqwest-based RPC client (works around rustls/LibreSSL issues).
use anyhow::{Context, Result};
use base64::Engine;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    message::Message,
    pubkey::Pubkey,
    signer::Signer,
    transaction::Transaction,
};
use std::str::FromStr;
use tracing::info;

const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const OFF_RESERVE_A: usize = 48;
const OFF_RESERVE_B: usize = 56;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    dotenv::dotenv().ok();

    let key_str = std::env::var("PRIVATE_KEY").context("PRIVATE_KEY")?;
    let key_bytes = bs58::decode(&key_str).into_vec().context("base58")?;
    let wallet = solana_sdk::signature::Keypair::from_bytes(&key_bytes).context("keypair")?;

    let rpc_url = std::env::var("RPC_URL").unwrap_or_else(|_| "https://api.devnet.solana.com".into());

    let p = |name: &str| -> Result<Pubkey> {
        Pubkey::from_str(&std::env::var(name).context(format!("{name} not set"))?)
            .context(format!("invalid {name}"))
    };

    let program_id  = p("PROGRAM_ID")?;
    let pool_pubkey = p("POOL_PUBKEY")?;
    let pool_auth   = p("POOL_AUTH")?;
    let vault_a     = p("VAULT_A")?;
    let vault_b     = p("VAULT_B")?;
    let lp_mint     = p("LP_MINT")?;
    let mint_a      = p("MINT_A")?;
    let mint_b      = p("MINT_B")?;
    let user_a      = p("USER_A")?;
    let user_b      = p("USER_B")?;
    let user_lp     = p("USER_LP")?;
    let dead_lp     = p("DEAD_LP_ACCOUNT")?;
    let token_program = Pubkey::from_str(TOKEN_PROGRAM_ID).unwrap();

    let http = reqwest::Client::builder().danger_accept_invalid_certs(true).build()?;

    // Read pool state
    let pool_data = get_account_data(&http, &rpc_url, &pool_pubkey).await.context("get pool")?;
    let reserve_a = u64::from_le_bytes(pool_data[OFF_RESERVE_A..OFF_RESERVE_A+8].try_into().unwrap());
    let reserve_b = u64::from_le_bytes(pool_data[OFF_RESERVE_B..OFF_RESERVE_B+8].try_into().unwrap());
    info!("current reserves: a={} b={}", reserve_a, reserve_b);

    let mul: u64 = 100;
    let add_a = reserve_a.max(1_000_000).saturating_mul(mul);
    let add_b = reserve_b.max(1_000_000).saturating_mul(mul);
    info!("injecting: add_a={} add_b={}", add_a, add_b);

    let bh = get_blockhash(&http, &rpc_url).await?;

    // Mint A
    let mint_a_ix = mint_to_ix(&token_program, &mint_a, &user_a, &wallet.pubkey(), add_a);
    let tx = Transaction::new_signed_with_payer(&[mint_a_ix], Some(&wallet.pubkey()), &[&wallet], bh);
    let sig = send_tx(&http, &rpc_url, &tx).await.context("mint A")?;
    info!("mint A: {}", sig);

    let bh = get_blockhash(&http, &rpc_url).await?;
    let mint_b_ix = mint_to_ix(&token_program, &mint_b, &user_b, &wallet.pubkey(), add_b);
    let tx = Transaction::new_signed_with_payer(&[mint_b_ix], Some(&wallet.pubkey()), &[&wallet], bh);
    let sig = send_tx(&http, &rpc_url, &tx).await.context("mint B")?;
    info!("mint B: {}", sig);

    let bh = get_blockhash(&http, &rpc_url).await?;
    let mut data = vec![3u8];
    data.extend_from_slice(&add_a.to_le_bytes());
    data.extend_from_slice(&add_b.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());

    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(pool_pubkey, false),
            AccountMeta::new(user_a,      false),
            AccountMeta::new(vault_a,     false),
            AccountMeta::new(user_b,      false),
            AccountMeta::new(vault_b,     false),
            AccountMeta::new(lp_mint,     false),
            AccountMeta::new(user_lp,     false),
            AccountMeta::new(dead_lp,     false),
            AccountMeta::new_readonly(wallet.pubkey(), true),
            AccountMeta::new_readonly(pool_auth, false),
            AccountMeta::new_readonly(token_program, false),
        ],
        data,
    };

    let cu_ix = ComputeBudgetInstruction::set_compute_unit_limit(400_000);
    let msg = Message::new(&[cu_ix, ix], Some(&wallet.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.sign(&[&wallet], bh);
    let sig = send_tx(&http, &rpc_url, &tx).await.context("add_liquidity")?;
    info!("add_liquidity: {}", sig);

    // Verify
    let pool_data2 = get_account_data(&http, &rpc_url, &pool_pubkey).await.context("re-read pool")?;
    let new_a = u64::from_le_bytes(pool_data2[OFF_RESERVE_A..OFF_RESERVE_A+8].try_into().unwrap());
    let new_b = u64::from_le_bytes(pool_data2[OFF_RESERVE_B..OFF_RESERVE_B+8].try_into().unwrap());
    info!("new reserves: a={} b={}", new_a, new_b);

    Ok(())
}

async fn get_account_data(http: &reqwest::Client, rpc_url: &str, pubkey: &Pubkey) -> Result<Vec<u8>> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "getAccountInfo",
        "params": [pubkey.to_string(), {"encoding": "base64"}]
    });
    let resp: serde_json::Value = http.post(rpc_url).json(&body).send().await?.json().await?;
    let b64 = resp["result"]["value"]["data"][0].as_str().context("no data")?;
    Ok(base64::engine::general_purpose::STANDARD.decode(b64)?)
}

async fn get_blockhash(http: &reqwest::Client, rpc_url: &str) -> Result<solana_sdk::hash::Hash> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "getLatestBlockhash",
        "params": [{"commitment": "confirmed"}]
    });
    let resp: serde_json::Value = http.post(rpc_url).json(&body).send().await?.json().await?;
    let hash_str = resp["result"]["value"]["blockhash"].as_str().context("no blockhash")?;
    Ok(hash_str.parse()?)
}

async fn send_tx(http: &reqwest::Client, rpc_url: &str, tx: &Transaction) -> Result<String> {
    let enc = base64::engine::general_purpose::STANDARD;
    let b64 = enc.encode(bincode::serialize(tx)?);
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "sendTransaction",
        "params": [b64, {"encoding": "base64", "skipPreflight": true, "preflightCommitment": "confirmed"}]
    });
    let resp: serde_json::Value = http.post(rpc_url).json(&body).send().await?.json().await?;
    if !resp["error"].is_null() {
        anyhow::bail!("RPC error: {}", resp["error"]);
    }
    resp["result"].as_str().context("no sig").map(|s| s.to_string())
}

fn mint_to_ix(token_program: &Pubkey, mint: &Pubkey, dest: &Pubkey, auth: &Pubkey, amount: u64) -> Instruction {
    let mut data = vec![7u8];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: *token_program,
        accounts: vec![
            AccountMeta::new(*mint, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new_readonly(*auth, true),
        ],
        data,
    }
}
