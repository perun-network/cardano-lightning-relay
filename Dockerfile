# syntax=docker/dockerfile:1

# Multi-stage build for cardano-lightning-relay
#
# Build from the PARENT directory containing both repos:
#   docker build -f cardano-lightning-relay/Dockerfile -t cardano-lightning-relay .

# ----- Builder -----
FROM rust:1.85-bookworm AS builder

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
        libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/cardano-lightning-relay/target/release/cardano-lightning-relay \
                    /usr/local/bin/cardano-lightning-relay

# Lightning P2P + REST API
EXPOSE 9735 3000

ENTRYPOINT ["cardano-lightning-relay"]
