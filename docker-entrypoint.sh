#!/bin/sh
set -e

# If positional arguments are supplied, pass them through unchanged so existing
# docker-compose configurations and manual invocations with the full LDK arg
# list keep working.
if [ $# -ge 2 ]; then
    exec cardano-lightning-relay "$@"
fi

# Env-var mode: assemble the LDK positional arguments from environment
# variables. Only BITCOIN_RPC_URL is required; others fall back to sensible
# defaults suitable for the signet + preprod workflow.
if [ -z "$BITCOIN_RPC_URL" ]; then
    cat >&2 <<'USAGE'
ERROR: cardano-lightning-relay requires configuration.

Provide either positional arguments (see the Dockerfile header or
docker-compose.signet.yml) or set these environment variables:

  BITCOIN_RPC_URL   (required)  user:pass@host:port for bitcoind RPC
  LDK_STORAGE_PATH  (optional)  LDK state directory [default: /data/ldk_state]
  LDK_PEER_PORT     (optional)  LDK peer listening port [default: 9735]
  BITCOIN_NETWORK   (optional)  regtest | testnet | signet [default: signet]

Example:
  docker run --rm \
    -e BITCOIN_RPC_URL=user:pass@bitcoind:38332 \
    -e BITCOIN_NETWORK=signet \
    cardano-lightning-relay

For full Signet + Preprod setup with Cardano environment variables, use:
  docker compose -f docker-compose.signet.yml up
USAGE
    exit 64
fi

exec cardano-lightning-relay \
    "$BITCOIN_RPC_URL" \
    "${LDK_STORAGE_PATH:-/data/ldk_state}" \
    "${LDK_PEER_PORT:-9735}" \
    "${BITCOIN_NETWORK:-signet}"
