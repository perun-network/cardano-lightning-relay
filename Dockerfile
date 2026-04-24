# syntax=docker/dockerfile:1

# Multi-stage build for cardano-lightning-relay
#
# Build from the PARENT directory containing both repos:
#   docker build -f cardano-lightning-relay/Dockerfile -t cardano-lightning-relay .
#
# Run (connects to external bitcoind for block sync, uses Esplora+BDK for wallet).
# LDK is configured from environment variables by the entrypoint script — no
# positional arguments needed. Positional args still work for backwards
# compatibility (see docker-compose.signet.yml).
#
#   docker run -d --name relay \
#     -p 9735:9735 -p 3002:3002 \
#     -v relay-data:/data \
#     -v ./secrets:/secrets:ro \
#     -e BITCOIN_RPC_URL=<btc_rpc_user>:<btc_rpc_pass>@<bitcoind_host>:38332 \
#     -e BITCOIN_NETWORK=signet \
#     -e BITCOIN_ESPLORA_URL=https://mempool.space/signet/api \
#     -e BITCOIN_WALLET_SEED_PATH=/data/wallet_seed.txt \
#     -e LDK_MIN_CHANNEL_CONFIRMATIONS=1 \
#     -e CARDANO_ENABLED=1 \
#     -e CARDANO_BLOCKFROST_URL=https://cardano-preprod.blockfrost.io/api/v0/ \
#     -e CARDANO_BLOCKFROST_KEY=<your_key> \
#     -e CARDANO_SKEY_PATH=/secrets/operator.sk \
#     -e CARDANO_SCRIPT_ADDRESS=<script_addr> \
#     -e CARDANO_SCRIPT_CBOR_PATH=/secrets/script_cbor.hex \
#     -e CARDANO_CBTC_POLICY_ID=<policy_id> \
#     -e CARDANO_CBTC_ASSET_NAME=<asset_name_hex> \
#     -e CARDANO_OPERATOR_ADDRESS=<operator_addr> \
#     -e CARDANO_OPERATOR_PKH=<operator_pkh> \
#     -e CARDANO_API_PORT=3002 \
#     cardano-lightning-relay

# ----- Builder -----
FROM rust:1.88-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy both crates (build context = parent directory)
COPY cardano-lightning-client/ cardano-lightning-client/
COPY cardano-lightning-relay/ cardano-lightning-relay/

WORKDIR /build/cardano-lightning-relay
RUN cargo build --release

# ----- Runtime -----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        libssl3 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/cardano-lightning-relay/target/release/cardano-lightning-relay \
                    /usr/local/bin/cardano-lightning-relay
COPY cardano-lightning-relay/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

RUN mkdir -p /data /secrets

# Lightning P2P
EXPOSE 9735
# REST API (default 3000, configurable via CARDANO_API_PORT)
EXPOSE 3000 3002

VOLUME ["/data", "/secrets"]

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl -sf http://localhost:${CARDANO_API_PORT:-3000}/pool/info || exit 1

ENTRYPOINT ["docker-entrypoint.sh"]
