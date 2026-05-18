# pamm-a

Research and implementation sandbox for prop AMM, market making, and simulation on Solana.

Live risk terminal: **[pamm.aileena.xyz](https://pamm.aileena.xyz)** — fee model trace, reader bot PnL, circuit breaker signals. Sim mode runs without any bot.

---

## What's here

| Path | Description |
|------|-------------|
| [`src/`](src/) | **Competition entry** — EWMA Dynamic Fee v2 (prop-amm-challenge submission) |
| [`pinocchio-prop-amm/`](pinocchio-prop-amm/) | Pinocchio 0.10 PMM — most complete: 16 litesvm tests, Jito bot, health/metrics endpoint |
| [`quasar-prop-amm/`](quasar-prop-amm/) | Quasar AMM + oracle bot |
| [`anchor-prop-amm/`](anchor-prop-amm/) | Anchor AMM + oracle bot (earliest iteration) |
| [`jupiter-mm-bot/`](jupiter-mm-bot/) | TypeScript MM bot routing via Jupiter |
| [`order-book-jupiter-mm-bot/`](order-book-jupiter-mm-bot/) | Order-book variant of the Jupiter MM bot |
| [`zero-slot-bot/`](zero-slot-bot/) | Rust zero-slot execution bot |
| [`visualization/`](visualization/) | **PAMM Terminal** — live risk cockpit (fee model trace, reader bot, signals). Deploy-ready static site. |
| [`docs/`](docs/) | Testing guide across all three AMM implementations |
| [`references/ethereum/`](references/ethereum/) | Solidity port of the strategy, for EVM comparison |

---

## Start here

1. [`src/lib.rs`](src/lib.rs) — the competition strategy: EWMA vol + shock-decay fee model
2. [`pinocchio-prop-amm/README.md`](pinocchio-prop-amm/README.md) — end-to-end reference: on-chain program, Jito bot, admin scripts, 16 tests
3. **[pamm.aileena.xyz](https://pamm.aileena.xyz)** — live risk cockpit: fee model trace, reader bot PnL, signal badges. Sim mode runs without any bot. ([source](visualization/terminal.html))

---

## AMM implementations

Three parallel protocol implementations, each exploring a different Solana framework:

|  | `anchor-prop-amm` | `quasar-prop-amm` | `pinocchio-prop-amm` |
|--|--|--|--|
| Framework | Anchor | Quasar | Pinocchio 0.10 |
| Tests | `anchor test` | `cargo test` | litesvm (16 tests) |
| Bot | Rust oracle bot | Rust oracle bot | Rust + Jito bundles + circuit breaker |
| Status | baseline | high-perf variant | most recent |

---

## Build & test

See [`docs/TESTING_GUIDE.md`](docs/TESTING_GUIDE.md) for build steps across all three implementations.

Competition entry (from repo root):
```sh
cargo build
cargo test
```

---

## Topics

`solana` `amm` `market-making` `mev` `defi` `rust` `simulation` `prop-amm`
