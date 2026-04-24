# Signet + Preprod Setup Guide

How to set up the Cardano Lightning relay for end-to-end testing on Bitcoin Signet and Cardano Preprod.

---

## Prerequisites

- Rust toolchain (stable, with `wasm32-unknown-unknown` target)
- Python 3 with `pycardano` (`pip install pycardano`)
- `expect` (for automated test scripts)
- `curl`, `jq`

---

## 1. Bitcoin Core (Signet)

### Install

Download Bitcoin Core v25.0+ from https://bitcoincore.org/en/download/ and extract.

```bash
tar xzf bitcoin-25.0-x86_64-linux-gnu.tar.gz
export PATH="$PWD/bitcoin-25.0/bin:$PATH"
```

### Start bitcoind on Signet

Pick a datadir for the Signet chain. The default is `~/.bitcoin`; override `BITCOIN_DATADIR` if you want to store the chain on a separate disk.

```bash
export BITCOIN_DATADIR="${BITCOIN_DATADIR:-$HOME/.bitcoin}"
mkdir -p "$BITCOIN_DATADIR"

bitcoind -signet -datadir="$BITCOIN_DATADIR" -daemon
```

This syncs the Signet chain (~2-3 GB, takes a few hours on first run). Data is stored in `$BITCOIN_DATADIR/signet/`.

Verify sync progress:

```bash
bitcoin-cli -signet -datadir="$BITCOIN_DATADIR" getblockchaininfo
```

Wait until `"initialblockdownload": false`.

### Create and fund wallets

You need two wallets: one for the payer node (simulates a user), one for the relay operator (optional, only if not using Esplora+BDK).

```bash
BTC_CLI="bitcoin-cli -signet -datadir=$BITCOIN_DATADIR"

# Create payer wallet
$BTC_CLI createwallet "user"
PAYER_ADDR=$($BTC_CLI -rpcwallet=user getnewaddress)
echo "Fund this address from a Signet faucet: $PAYER_ADDR"

# Create operator wallet (optional — relay uses BDK, not bitcoind wallet)
$BTC_CLI createwallet "relay_operator"
OPERATOR_ADDR=$($BTC_CLI -rpcwallet=relay_operator getnewaddress)
echo "Fund this address too: $OPERATOR_ADDR"
```

### Fund from faucet

Use a Signet faucet to send tBTC to both addresses. Known faucets:

- https://signet.bc-2.jp/
- https://signetfaucet.com/

Request at least 50,000 sats per wallet. Wait for 1 confirmation (~10 min per Signet block).

Verify:

```bash
$BTC_CLI -rpcwallet=user getbalance
$BTC_CLI -rpcwallet=relay_operator getbalance
```

### Important: wallet loading for ldk-sample

`ldk-sample` panics if bitcoind has multiple wallets loaded. Before starting the payer node, unload the relay_operator wallet:

```bash
$BTC_CLI unloadwallet relay_operator
```

---

## 2. Blockfrost API Key (Cardano Preprod)

Sign up at https://blockfrost.io/ and create a project for `Cardano Preprod` (free tier is sufficient).

Save the project ID. You'll need it as `CARDANO_BLOCKFROST_KEY`.

Create a secrets file:

```bash
mkdir -p cardano-lightning-deployment/secrets

cat > cardano-lightning-deployment/secrets/preprod.env <<'EOF'
export BLOCKFROST_PROJECT_ID=preprodYOUR_KEY_HERE
export BLOCKFROST_BASE_URL=https://cardano-preprod.blockfrost.io/api/
export CARDANO_NETWORK=preprod
EOF
```

---

## 3. Cardano Operator Credentials

The relay needs an operator signing key to submit Plutus transactions on Preprod.

### Generate operator key

Using `cardano-cli`:

```bash
cardano-cli address key-gen \
  --signing-key-file cardano-lightning-deployment/secrets/operator.sk \
  --verification-key-file /tmp/operator.vk

cardano-cli address build \
  --payment-verification-key-file /tmp/operator.vk \
  --testnet-magic 1 \
  --out-file /tmp/operator.addr

OPERATOR_ADDR=$(cat /tmp/operator.addr)
echo "Operator address: $OPERATOR_ADDR"
```

Or using `pycardano`:

```python
from pycardano import PaymentSigningKey, PaymentVerificationKey, Address, Network

sk = PaymentSigningKey.generate()
sk.save("cardano-lightning-deployment/secrets/operator.sk")
vk = PaymentVerificationKey.from_signing_key(sk)
addr = Address(vk.hash(), network=Network.TESTNET)
print(f"Operator address: {addr}")
print(f"Operator PKH: {vk.hash().to_primitive().hex()}")
```

### Fund operator on Preprod

Use the Cardano Preprod faucet: https://docs.cardano.org/cardano-testnets/tools/faucet/

Request tADA to the operator address. You need at least 100 tADA for Plutus transaction fees and collateral.

### Ensure ADA-only collateral UTxOs

Plutus transactions require ADA-only collateral inputs (no multi-asset). Create some:

```python
from pycardano import *
import json

with open("cardano-lightning-deployment/secrets/operator.sk") as f:
    sk_data = json.load(f)
raw_key_hex = sk_data["cborHex"][4:]
sk = PaymentSigningKey.from_primitive(bytes.fromhex(raw_key_hex))

ctx = BlockFrostChainContext(
    project_id="YOUR_BLOCKFROST_KEY",
    base_url="https://cardano-preprod.blockfrost.io/api/"
)
addr = Address.from_primitive("YOUR_OPERATOR_ADDRESS")

builder = TransactionBuilder(ctx)
builder.add_input_address(addr)
for _ in range(10):
    builder.add_output(TransactionOutput(addr, Value(coin=10_000_000)))
signed_tx = builder.build_and_sign([sk], change_address=addr)
ctx.submit_tx(signed_tx)
print(f"Created 10 x 10 ADA collateral UTxOs: {signed_tx.id}")
```

---

## 4. Liquidity Manager Contract (Preprod)

The LM contract must be deployed on Preprod with initial cBTC liquidity. If you have an existing deployment, note:

- **Script address** — the Plutus script address holding the pool
- **cBTC policy ID** — the minting policy for test cBTC tokens
- **cBTC asset name** — hex-encoded asset name (e.g. `63425443` for "cBTC")

The `plutus-applied-preprod.json` file (in `lightning-liquidity-manager/`) contains the parameterized compiled script. The relay needs the `compiledCode` field extracted to a hex file:

```bash
python3 -c "
import json
with open('lightning-liquidity-manager/plutus-applied-preprod.json') as f:
    d = json.load(f)
print(d['validators'][0]['compiledCode'])
" > /tmp/script_cbor.hex
```

---

## 5. Build Binaries

### Relay

```bash
cd cardano-lightning-relay
cargo build --release
# Binary: target/release/cardano-lightning-relay
```

### Payer node (ldk-sample)

```bash
cd ldk-sample
cargo build --release
# Binary: target/release/ldk-sample
```

---

## 6. Start the Relay

The relay needs bitcoind for block sync and uses Esplora+BDK for wallet operations.

### Extract bitcoind cookie auth

Signet uses cookie authentication:

```bash
COOKIE_USER="__cookie__"
COOKIE_PASS=$(cat "$BITCOIN_DATADIR/signet/.cookie" | cut -d: -f2)
BTC_RPC_AUTH="${COOKIE_USER}:${COOKIE_PASS}@127.0.0.1:38332"
```

### BDK seed file

On first run, the relay auto-generates a BDK wallet seed file. You can also pre-create one:

```bash
python3 -c "import os; print(os.urandom(32).hex())" > cardano-lightning-relay/signet_wallet_seed.txt
```

### Environment variables reference

| Variable | Required | Description |
|---|---|---|
| `BITCOIN_ESPLORA_URL` | Yes (for Signet) | Esplora API endpoint. Use `https://mempool.space/signet/api` |
| `BITCOIN_WALLET_SEED_PATH` | No | Path to BDK seed file. Defaults to `<ldk_data_dir>/wallet_seed.txt` |
| `LDK_MIN_CHANNEL_CONFIRMATIONS` | No | Min block confirmations for channels. Default 6, set to 1 for Signet testing |
| `CARDANO_ENABLED` | Yes | Set to `1` to enable Cardano swap functionality |
| `CARDANO_BLOCKFROST_URL` | Yes | `https://cardano-preprod.blockfrost.io/api/v0/` |
| `CARDANO_BLOCKFROST_KEY` | Yes | Blockfrost project ID for Preprod |
| `CARDANO_SKEY_PATH` | Yes | Path to operator signing key (`.sk` file) |
| `CARDANO_SCRIPT_ADDRESS` | Yes | Plutus script address on Preprod |
| `CARDANO_SCRIPT_CBOR_PATH` | Yes | Path to extracted script CBOR hex file |
| `CARDANO_CBTC_POLICY_ID` | Yes | cBTC minting policy ID (hex) |
| `CARDANO_CBTC_ASSET_NAME` | Yes | cBTC asset name (hex) |
| `CARDANO_OPERATOR_ADDRESS` | Yes | Operator Cardano address (bech32) |
| `CARDANO_OPERATOR_PKH` | Yes | Operator payment key hash (hex) |
| `CARDANO_API_PORT` | No | REST API port. Default 3000 |
| `CARDANO_API_AUTH_TOKEN` | No | Bearer token for operator endpoints. WARNING logged if unset |
| `CARDANO_API_RATE_LIMIT_MAX` | No | Max API requests per window per IP. Default 100 |
| `CARDANO_API_RATE_LIMIT_WINDOW_SECS` | No | Rate limit window in seconds. Default 60 |
| `CARDANO_SWAP_EXPIRY_SECONDS` | No | Swap/offramp expiry. Default 3600 (1 hour) |
| `CARDANO_MAX_ACTIVE_SWAPS` | No | Max concurrent onramp swaps. Default 50 |
| `CARDANO_MAX_ACTIVE_OFFRAMPS` | No | Max concurrent offramps. Default 50 |

### Start command

```bash
source cardano-lightning-deployment/secrets/preprod.env

BITCOIN_ESPLORA_URL=https://mempool.space/signet/api \
BITCOIN_WALLET_SEED_PATH=./signet_wallet_seed.txt \
LDK_MIN_CHANNEL_CONFIRMATIONS=1 \
CARDANO_ENABLED=1 \
CARDANO_BLOCKFROST_URL="https://cardano-preprod.blockfrost.io/api/v0/" \
CARDANO_BLOCKFROST_KEY="$BLOCKFROST_PROJECT_ID" \
CARDANO_SKEY_PATH="cardano-lightning-deployment/secrets/operator.sk" \
CARDANO_SCRIPT_ADDRESS="<your_script_address>" \
CARDANO_SCRIPT_CBOR_PATH="/tmp/script_cbor.hex" \
CARDANO_CBTC_POLICY_ID="<your_cbtc_policy>" \
CARDANO_CBTC_ASSET_NAME="<your_cbtc_name_hex>" \
CARDANO_OPERATOR_ADDRESS="<your_operator_address>" \
CARDANO_OPERATOR_PKH="<your_operator_pkh>" \
CARDANO_API_PORT=3002 \
target/release/cardano-lightning-relay \
  "$BTC_RPC_AUTH" \
  ./ldk_data_signet 9735 signet
```

Expected output:

```
Esplora connected: https://mempool.space/signet/api (tip height: XXXXXX)
BDK wallet initialized (network: Signet, first address: tb1q...)
Esplora+BDK backend enabled — wallet ops bypass bitcoind
Channel minimum confirmations: 1
Cardano operator agent initialized.
Cardano swap API listening on port 3002
Local Node ID is <hex_pubkey>
```

### Verify API

```bash
curl -s http://localhost:3002/pool/info | python3 -m json.tool
```

---

## 7. Start the Payer Node

The payer simulates a user making Lightning payments. It uses bitcoind's wallet directly.

```bash
# Ensure only the 'user' wallet is loaded
bitcoin-cli -signet -datadir="$BITCOIN_DATADIR" unloadwallet relay_operator 2>/dev/null

ldk-sample/target/release/ldk-sample \
  "$BTC_RPC_AUTH" \
  ldk-sample/ldk_data_signet_payer 9736 signet
```

Note the payer's `Local Node ID` from the output.

---

## 8. Open a Lightning Channel

From the payer node's interactive prompt:

```
connectpeer <relay_node_id>@127.0.0.1:9735
openchannel <relay_node_id>@127.0.0.1:9735 20000
```

- Channel size: 20,000 sats (enough for testing, small enough for faucet-funded wallets)
- Do NOT append `0` or any announce flag to `openchannel` — it causes a crash

### Wait for confirmation

Signet blocks arrive every ~10 minutes. With `LDK_MIN_CHANNEL_CONFIRMATIONS=1`, the channel becomes usable after one block.

Check from the payer:

```
listchannels
```

Wait until `is_channel_ready: true`.

---

## 9. Test a Swap

### Onramp (BTC -> cBTC)

```bash
# Request an onramp swap
curl -s -X POST http://localhost:3002/swap/request \
  -H "Content-Type: application/json" \
  -d '{"amount_cbtc": 50000, "cardano_address": "<recipient_cardano_address>"}'
```

Response includes a `bolt11` invoice. Pay it from the payer node:

```
sendpayment <bolt11_invoice>
```

Poll status:

```bash
curl -s http://localhost:3002/swap/status/<payment_hash>
```

Status progression: `Pending` -> `Fulfilling` -> `Completed`.

### Offramp (cBTC -> BTC)

From the payer node, create a Lightning invoice:

```
getinvoice 50000 3600
```

Then request the offramp:

```bash
curl -s -X POST http://localhost:3002/offramp/request \
  -H "Content-Type: application/json" \
  -d '{"bolt11": "<payer_invoice>", "amount_cbtc": 50000, "cardano_address": "<your_address>"}'
```

The relay submits a CreateOfframp TX on Preprod. Then simulate the user sending cBTC to the operator (in production, the user's wallet does this). Finally notify the relay:

```bash
curl -s -X POST http://localhost:3002/offramp/deposit \
  -H "Content-Type: application/json" \
  -d '{"offramp_id": <id>, "cbtc_tx_hash": "<cbtc_transfer_tx_hash>"}'
```

The relay pays the Lightning invoice and submits FulfillOfframp on Preprod.

---

## 10. Automated E2E Tests

### Single onramp + offramp

```bash
SIGNET_OFFRAMP_RESUME=1 expect flows_e2e_runs/signet_offramp_test.exp
```

Set `SIGNET_OFFRAMP_RESUME=1` to reuse an existing channel (skip channel open + block wait).

### Full MS4 evidence collection (10+10)

```bash
expect flows_e2e_runs/ms4_evidence_collection.exp
```

This runs 10 onramps + 10 offramps, captures all TX hashes, and writes structured evidence to `evidence_ms4/ms4_evidence.json`.

Runtime: ~45-60 minutes (dominated by Blockfrost indexing waits).

---

## Troubleshooting

### bitcoind cookie auth

The relay expects `user:pass@host:port` format. For cookie auth on Signet:

```bash
COOKIE_USER="__cookie__"
COOKIE_PASS=$(cat "$BITCOIN_DATADIR/signet/.cookie" | cut -d: -f2)
```

The cookie file is regenerated each time bitcoind starts, so re-extract after restarts.

### ldk-sample multi-wallet crash

`ldk-sample` crashes with `Wallet file not specified` if bitcoind has multiple wallets loaded. Always unload all wallets except the one the payer uses before starting:

```bash
bitcoin-cli -signet -datadir="$BITCOIN_DATADIR" unloadwallet relay_operator
```

### Channel never becomes ready

- Verify bitcoind is synced: `bitcoin-cli -signet -datadir="$BITCOIN_DATADIR" getblockchaininfo` should show `"initialblockdownload": false`
- Verify the `user` wallet is loaded and funded
- Wait for at least one Signet block (~10 min) after `openchannel`
- Check `listchannels` output for `is_channel_ready: false` vs `true`

### Blockfrost indexing delays

Preprod Blockfrost can lag 10-90 seconds behind chain tip. The relay and test scripts include retry/poll logic. If a swap stays in `Fulfilling` for over 2 minutes, it usually still completes — the FulfillInvoice TX was submitted but Blockfrost hasn't indexed it yet.

### "All inputs are spent" on pycardano self-send

This happens when Blockfrost returns stale UTxO data after a recent TX. The test scripts retry with 30-second delays. For manual testing, wait 30-60 seconds and retry.

### Signet block timing

Signet blocks arrive every ~10 minutes on average, but gaps of 15-20 minutes occur. Set `LDK_MIN_CHANNEL_CONFIRMATIONS=1` to avoid waiting for 6 confirmations (~60 minutes).

---

## Port Summary

| Port | Service | Notes |
|---|---|---|
| 38332 | bitcoind Signet RPC | Default Signet RPC port |
| 9735 | Relay LDK peer | Lightning peer connections |
| 9736 | Payer LDK peer | Lightning peer connections |
| 3002 | Relay REST API | Swap/offramp/pool endpoints |

---

## Directory Layout

```
cardano-lightning-relay/          # Relay source + binary
  target/release/cardano-lightning-relay
  ldk_data_signet/                # LDK state (channels, keys) — auto-created
  signet_wallet_seed.txt          # BDK wallet seed — auto-created on first run

ldk-sample/                       # Payer node source + binary
  target/release/ldk-sample
  ldk_data_signet_payer/          # Payer LDK state — auto-created

lightning-liquidity-manager/      # Cardano contract + deployment tools
  plutus-applied-preprod.json     # Parameterized Plutus script for Preprod

cardano-lightning-deployment/     # Deployment secrets and configs
  secrets/
    preprod.env                   # Blockfrost API key
    operator.sk                   # Cardano operator signing key

cardano-lightning-docs/           # Documentation and test scripts
  flows_e2e_runs/
    signet_offramp_test.exp       # Single onramp+offramp test
    ms4_evidence_collection.exp   # Full 10+10 evidence run
  evidence_ms4/                   # Evidence output from runs
```
