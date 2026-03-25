# Project Structure and Design Overview

See also: docs/structure.md for a detailed breakdown.

## Directory Structure

```
prop-amm-full/
├── programs/
│   └── prop-amm/
│       ├── Cargo.toml
│       ├── Quasar.toml
│       └── src/
│           ├── lib.rs
│           ├── state.rs
│           └── instructions/
│               ├── swap.rs
│               ├── update_oracle.rs
│               ├── add_liquidity.rs
│               └── remove_liquidity.rs
├── bot/
│   ├── Cargo.toml
│   ├── .env.example
│   └── src/
│       └── main.rs
├── scripts/
│   └── deploy.sh
├── README.md
```

## Key Features
- Dynamic spread and inventory skew logic
- LP token support (add/remove liquidity)
- Off-chain Oracle Bot with Jito/Harmonic bundle, tip optimization, real-time monitoring, volatility
- Clear structure for easy extension

## For more details, see docs/structure.md
# Quasar Prop AMM

High-performance Solana AMM contract + off-chain Oracle Bot (Jito Bundle/Tip/inventory monitoring/volatility/LP Token/simulation/multi-relay push)

## Directory Structure

```
quasar-prop-amm/
├── programs/
│   └── prop-amm/
│       ├── Cargo.toml
│       ├── Quasar.toml
│       └── src/
│           ├── lib.rs
│           ├── state.rs
│           └── instructions/
│               ├── swap.rs
│               ├── add_liquidity.rs
│               └── remove_liquidity.rs
├── bot/
│   ├── Cargo.toml
│   ├── .env.example
│   └── src/
│       └── main.rs
```

## 功能亮点
- 动态 Spread + 库存倾斜
- LP Token 支持（流动性增减）
- 链下 Oracle Bot 实时库存监控、动态波动率、Jito Bundle+Tip 优化
- 代码结构清晰，便于二次开发

## 快速开始

### 1. 安装依赖

```bash
cd programs/prop-amm
cargo build

cd ../../bot
cargo build
```

### 2. 配置 .env

复制 bot/.env.example 为 .env，填写 RPC、钱包私钥、Pool Pubkey、Jito Relay 等参数。

### 3. 运行链下 Oracle Bot

```bash
cargo run
```

### 4. 部署合约

参考 Quasar 官方文档，或用 scripts/deploy.sh

---

如需详细用法、集成测试或部署脚本，请补充需求！
