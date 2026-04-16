# ── Build stage ───────────────────────────────────────────────────────────────
FROM rust:1.78-slim AS builder

WORKDIR /build

# Cache dependencies before copying source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

# Build the real binary
COPY src ./src
RUN cargo build --release

# ── Runtime stage ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/oracle-bot /usr/local/bin/oracle-bot

# Metrics port (override with METRICS_PORT env var)
EXPOSE 3000

# All config via environment variables — see .env.example for full docs.
# Required at minimum:
#   PRIVATE_KEY, PROGRAM_ID, POOL_PUBKEY, POOL_AUTH,
#   VAULT_A, VAULT_B, LP_MINT, MINT_A, MINT_B,
#   USER_A, USER_B, USER_LP, DEAD_LP_ACCOUNT
CMD ["oracle-bot"]
