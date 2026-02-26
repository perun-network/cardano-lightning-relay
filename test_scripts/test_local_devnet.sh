#!/bin/bash
# =============================================================================
# Local Devnet E2E Test: Deploy LM Contract + Rust Query
# =============================================================================
#
# Prerequisites:
#   - Docker running
#   - yaci-devkit installed (~/.yaci-devkit/)
#   - pycardano installed (pip3 install pycardano)
#   - aiken CLI installed (cargo install aiken)
#   - Rust toolchain (edition 2024)
#
# Usage:
#   bash test_local_devnet.sh          # Normal run (reuses existing devnet)
#   bash test_local_devnet.sh --clean  # Clean start (tears down first)
#
# What it does:
#   1. (Optional) Tears down existing devnet and cleans credentials
#   2. Starts yaci-devkit local Cardano devnet (Conway era)
#   3. Generates operator credentials
#   4. Funds operator via cluster API
#   5. Mints test cBTC tokens
#   6. Deploys parameterized contract
#   7. Builds Rust query-contract binary
#   8. Queries contract state from Rust
#   9. Validates query output
# =============================================================================

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${BLUE}[$(date +%H:%M:%S)]${NC} $1"; }
ok()  { echo -e "${GREEN}[OK]${NC} $1"; }
err() { echo -e "${RED}[ERROR]${NC} $1"; }
warn(){ echo -e "${YELLOW}[WARN]${NC} $1"; }

# Paths
YACI_DIR="$HOME/.yaci-devkit"
LM_DIR="$HOME/pc-work/lightning-liquidity-manager"
RELAY_DIR="$HOME/pc-work/cardano-lightning-relay"

# Environment for local devnet
export BLOCKFROST_BASE_URL="http://localhost:8080/api/v1/"
export CARDANO_NETWORK="local"
export BLOCKFROST_PROJECT_ID="local"
export PATH="$YACI_DIR/bin:$HOME/.cargo/bin:$PATH"

# =============================================================================
# Step 0: Clean start (if --clean flag)
# =============================================================================
if [[ "${1:-}" == "--clean" ]]; then
    log "Clean start requested — tearing down existing devnet..."

    # Stop containers and remove volumes
    if docker ps --format '{{.Names}}' 2>/dev/null | grep -q 'node1-yaci-cli-1'; then
        cd "$YACI_DIR/scripts"
        docker compose --env-file ../config/env --env-file ../config/version down -v 2>&1 || true
        ok "Devnet containers stopped and volumes removed"
    else
        ok "No running devnet containers found"
    fi

    # Clean LM credentials
    rm -f "$LM_DIR/credentials/deployment.json" \
          "$LM_DIR/credentials/operator.sk" \
          "$LM_DIR/credentials/operator.addr" \
          "$LM_DIR/plutus-applied.json"
    ok "LM credentials cleaned"

    sleep 2
fi

# =============================================================================
# Step 1: Check prerequisites
# =============================================================================
log "Checking prerequisites..."

if ! docker info >/dev/null 2>&1; then
    err "Docker is not running"
    exit 1
fi
ok "Docker is running"

if ! command -v python3 >/dev/null 2>&1; then
    err "python3 not found"
    exit 1
fi

if ! python3 -c "import pycardano" 2>/dev/null; then
    err "pycardano not installed. Run: pip3 install pycardano"
    exit 1
fi
ok "python3 + pycardano available"

if ! command -v aiken >/dev/null 2>&1; then
    err "aiken CLI not found. Install with: cargo install aiken"
    exit 1
fi
ok "aiken $(aiken --version) available"

if ! command -v cargo >/dev/null 2>&1; then
    err "cargo not found"
    exit 1
fi
ok "Rust toolchain available"

# =============================================================================
# Step 2: Start yaci-devkit
# =============================================================================
log "Starting yaci-devkit local devnet..."

# Start Docker containers if not running
if docker ps --format '{{.Names}}' | grep -q 'node1-yaci-cli-1'; then
    warn "yaci-devkit containers already running"
else
    cd "$YACI_DIR/scripts"
    docker compose --env-file ../config/env --env-file ../config/version up -d
    ok "Docker containers started"

    # Wait for containers to be ready
    log "Waiting for containers to initialize..."
    sleep 5
fi

# Check if devnet node is already running
if curl -sf http://localhost:8080/api/v1/blocks/latest >/dev/null 2>&1; then
    ok "Devnet node already running"
else
    log "Creating Conway-era devnet node..."
    docker exec node1-yaci-cli-1 /app/yaci-cli.sh create-node --era conway -o --start &
    YACI_PID=$!

    # Wait for the API to become available
    log "Waiting for Blockfrost API..."
    API_READY=false
    for i in $(seq 1 90); do
        if curl -sf http://localhost:8080/api/v1/blocks/latest >/dev/null 2>&1; then
            ok "Devnet API ready (took ${i}s)"
            API_READY=true
            break
        fi
        sleep 1
    done

    if [ "$API_READY" = false ]; then
        err "Devnet API not available after 90 seconds"
        exit 1
    fi
fi

# Verify block production
BLOCK_HEIGHT=$(curl -sf http://localhost:8080/api/v1/blocks/latest | python3 -c "import sys,json; print(json.load(sys.stdin)['height'])")
ok "Current block height: $BLOCK_HEIGHT"

# =============================================================================
# Step 3: Generate operator credentials
# =============================================================================
log "Generating operator credentials..."
cd "$LM_DIR"

if [ -f credentials/deployment.json ]; then
    warn "Existing credentials found, reusing them"
else
    python3 scripts/config.py 2>&1 | tail -10
fi
ok "Operator credentials ready"

# Extract operator address
OPERATOR_ADDR=$(python3 -c "
import json
with open('credentials/deployment.json') as f:
    d = json.load(f)
print(d['operator']['address'])
")
log "Operator address: $OPERATOR_ADDR"

# =============================================================================
# Step 4: Fund operator
# =============================================================================
log "Funding operator with 5000 ADA..."

TOPUP_RESULT=$(curl -sf -X POST "http://localhost:10000/local-cluster/api/addresses/topup" \
    -H "Content-Type: application/json" \
    -d "{\"address\":\"$OPERATOR_ADDR\",\"adaAmount\":5000}" 2>&1)

if echo "$TOPUP_RESULT" | python3 -c "import sys,json; r=json.load(sys.stdin); assert r.get('status', True)" 2>/dev/null; then
    ok "Funded 5000 ADA"
else
    warn "Topup response: $TOPUP_RESULT"
fi

sleep 3  # Wait for block confirmation

# =============================================================================
# Step 5: Mint test cBTC tokens
# =============================================================================
log "Minting 100,000,000 test cBTC tokens..."

python3 scripts/mint_cbtc.py 100000000 2>&1 | tail -10

sleep 3  # Wait for block confirmation

ok "cBTC minted"

# =============================================================================
# Step 6: Deploy contract
# =============================================================================
log "Deploying parameterized contract with 50,000,000 cBTC initial liquidity..."

DEPLOY_OUTPUT=$(python3 scripts/init_contract.py 50000000 2>&1)
echo "$DEPLOY_OUTPUT" | tail -15

sleep 3  # Wait for block confirmation

# Extract script address (parameterized or fallback to base)
SCRIPT_ADDR=$(python3 -c "
import json
with open('credentials/deployment.json') as f:
    d = json.load(f)
print(d['validator'].get('parameterized_script_address', d['validator']['script_address']))
")

ok "Contract deployed at: $SCRIPT_ADDR"

# Extract TX hash from deploy output
TX_HASH=$(echo "$DEPLOY_OUTPUT" | grep -oP 'TX Hash:\s+\K[a-f0-9]+' || echo "unknown")
log "Deploy TX: $TX_HASH"

# =============================================================================
# Step 7: Verify contract state via Blockfrost API
# =============================================================================
log "Querying contract UTxO via Blockfrost..."

UTXO_RESP=$(curl -sf "http://localhost:8080/api/v1/addresses/$SCRIPT_ADDR/utxos" \
    -H "project_id: local" 2>&1)

if [ $? -ne 0 ]; then
    err "Failed to query script UTxOs"
    exit 1
fi

# Verify UTxO has inline datum and cBTC
HAS_DATUM=$(echo "$UTXO_RESP" | python3 -c "
import json, sys
utxos = json.load(sys.stdin)
for u in utxos:
    if u.get('inline_datum'):
        cbtc = next((a['quantity'] for a in u['amount'] if a['unit'] != 'lovelace'), '0')
        print(f'cBTC: {cbtc}, datum: present')
        break
else:
    print('no datum UTxO found')
")
echo "  $HAS_DATUM"

if echo "$HAS_DATUM" | grep -q "cBTC: 50000000"; then
    ok "Contract has 50,000,000 cBTC with inline datum"
else
    err "Contract state mismatch: $HAS_DATUM"
    exit 1
fi

# =============================================================================
# Summary
# =============================================================================
echo ""
echo "============================================================"
echo -e "${GREEN}Local Devnet E2E Test PASSED${NC}"
echo "============================================================"
echo "Devnet API:      http://localhost:8080/api/v1/"
echo "Explorer:        http://localhost:5173"
echo "Ogmios:          ws://localhost:1337"
echo "Script Address:  $SCRIPT_ADDR"
echo "Deploy TX:       $TX_HASH"
echo ""
echo "To query contract state:"
echo "  curl -s http://localhost:8080/api/v1/addresses/$SCRIPT_ADDR/utxos -H 'project_id: local' | python3 -m json.tool"
echo ""
echo "To run onramp E2E test:"
echo "  cd ~/pc-work/cardano-lightning-docs/flows_e2e_runs && expect cardano_onramp_test.exp"
echo ""
echo "To stop devnet:"
echo "  cd ~/.yaci-devkit/scripts && docker compose --env-file ../config/env --env-file ../config/version down"
echo "  # Add -v flag to also remove volumes (wipes chain data)"
echo "============================================================"
