use litesvm::LiteSVM;
use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer, system_instruction};

#[test]
fn test_prop_amm_basic() {
    let mut svm = LiteSVM::new();

    // 给测试钱包空投一些 SOL
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap(); // 10 SOL

    println!("✅ LiteSVM 测试环境启动成功！");
    println!("你可以在这里测试你的 Prop AMM 程序的 initialize、swap 等功能");
    
    // TODO: 后面我们可以在这里部署你的程序并测试 swap
}
