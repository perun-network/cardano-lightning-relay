# Local Testing

End-to-end testing of the relay with devnet, payer node, and frontend.

For the full step-by-step guide with troubleshooting, see `cardano-lightning-docs/LOCAL_FRONTEND_TEST_GUIDE.md`.

## One-command start

```bash
cd ~/pc-work/cardano-lightning-docs/flows_e2e_runs
bash start_local_env.sh
# Open http://localhost:5174
# Pay invoices: echo 'sendpayment <bolt11>' > /tmp/payer_input
# Check balance: bash check_cbtc_balance.sh <address>
# Stop: bash stop_local_env.sh
```

## Manual quick start

```bash
# 1. Build
cargo build --release
cd ~/pc-work/ldk-sample && cargo build --release

# 2. Start devnet (deploys contract with 50M cBTC)
cd ~/pc-work/lightning-liquidity-manager
bash ../cardano-lightning-docs/flows_e2e_runs/test_local_devnet.sh --clean

# 3. Start bitcoind
~/workrepos/bitcoin-25.0/bin/bitcoind -regtest \
  -rpcuser=ic-btc-integration \
  -rpcpassword='QPQiNaph19FqUsCrBRN0FII7lyM26B51fAMeBQzCb-E=' -daemon
sleep 2
~/workrepos/bitcoin-25.0/bin/bitcoin-cli -regtest \
  -rpcuser=ic-btc-integration \
  -rpcpassword='QPQiNaph19FqUsCrBRN0FII7lyM26B51fAMeBQzCb-E=' \
  loadwallet testwallet
~/workrepos/bitcoin-25.0/bin/bitcoin-cli -regtest \
  -rpcuser=ic-btc-integration \
  -rpcpassword='QPQiNaph19FqUsCrBRN0FII7lyM26B51fAMeBQzCb-E=' \
  -rpcwallet=testwallet -generate 101

# 4. Start relay (extract config from deployment.json)
export D="$HOME/pc-work/lightning-liquidity-manager/credentials/deployment.json"
export P="$HOME/pc-work/lightning-liquidity-manager/plutus-applied.json"
python3 -c "import json; print(json.load(open('$P'))['validators'][0]['compiledCode'])" > /tmp/script.hex
rm -rf ldk_data_cardano

CARDANO_ENABLED=1 \
CARDANO_BLOCKFROST_URL="http://localhost:8080/api/v1/" \
CARDANO_BLOCKFROST_KEY="local" \
CARDANO_SKEY_PATH="$(python3 -c "import json; print(json.load(open('$D'))['operator']['sk_file'])")" \
CARDANO_SCRIPT_ADDRESS="$(python3 -c "import json; print(json.load(open('$D'))['validator']['parameterized_script_address'])")" \
CARDANO_SCRIPT_CBOR_PATH="/tmp/script.hex" \
CARDANO_CBTC_POLICY_ID="$(python3 -c "import json; print(json.load(open('$D'))['token']['cbtc_policy'])")" \
CARDANO_CBTC_ASSET_NAME="$(python3 -c "import json; print(json.load(open('$D'))['token']['cbtc_asset_name'])")" \
CARDANO_OPERATOR_ADDRESS="$(python3 -c "import json; print(json.load(open('$D'))['operator']['address'])")" \
CARDANO_OPERATOR_PKH="$(python3 -c "import json; print(json.load(open('$D'))['operator']['pkh'])")" \
CARDANO_API_PORT=3002 \
./target/release/cardano-lightning-relay \
  "ic-btc-integration:QPQiNaph19FqUsCrBRN0FII7lyM26B51fAMeBQzCb-E=@127.0.0.1:18443" \
  ldk_data_cardano 9735 regtest

# 5. Start payer node (new terminal)
rm -rf ~/pc-work/ldk-sample/ldk_data_payer
~/pc-work/ldk-sample/target/release/ldk-sample \
  "ic-btc-integration:QPQiNaph19FqUsCrBRN0FII7lyM26B51fAMeBQzCb-E=@127.0.0.1:18443" \
  ~/pc-work/ldk-sample/ldk_data_payer 9737 regtest

# In payer terminal:
connectpeer <relay_node_id>@127.0.0.1:9735
openchannel <relay_node_id> 500000 0

# Mine to confirm channel
~/workrepos/bitcoin-25.0/bin/bitcoin-cli -regtest \
  -rpcuser=ic-btc-integration \
  -rpcpassword='QPQiNaph19FqUsCrBRN0FII7lyM26B51fAMeBQzCb-E=' \
  -rpcwallet=testwallet -generate 6

# 6. Serve frontend (new terminal)
cd ~/pc-work/cardano-lightning-app/dist
sed -i 's|<head>|<head><script>window.RELAY_URL="http://localhost:3002"</script>|' index.html
python3 -m http.server 5174
# Open http://localhost:5174
```

## Testing the onramp (BTC → cBTC)

1. Browser: select Onramp, enter amount + Cardano address, click Bridge
2. Copy the BOLT11 invoice string
3. Payer terminal: `sendpayment <bolt11>`
4. Mine a block to speed confirmation
5. Browser shows: Pending → Fulfilling → Completed

## Testing the offramp (cBTC → BTC)

1. Payer terminal: `getinvoice 50000 3600` — copy the BOLT11
2. Browser: select Offramp, paste invoice, enter amount + address, click Bridge
3. The full offramp flow requires a CIP-30 wallet (Nami/Eternl) on the devnet — not available locally. Use the E2E scripts instead:
   ```bash
   cd ~/pc-work/cardano-lightning-docs/flows_e2e_runs
   expect cardano_offramp_test.exp
   ```

## Verify via API

```bash
curl -s http://localhost:3002/health | python3 -m json.tool
curl -s http://localhost:3002/pool/info | python3 -m json.tool
curl -s http://localhost:3002/metrics | python3 -m json.tool
```

## E2E test scripts

Run each with a fresh devnet (`test_local_devnet.sh --clean` before each):

| Script | Tests |
|--------|-------|
| `deposit_liq.exp` | Pool deposit |
| `deposit_withdraw_liq.exp` | Deposit + withdraw |
| `cardano_onramp_test.exp` | Full onramp with channel |
| `cardano_offramp_test.exp` | Full offramp with channel |
| `insufficient_liquidity_test.exp` | Rejection when pool empty |
| `cancel_expired_onramp_test.exp` | Expired swap auto-cancel |
| `cancel_expired_offramp_test.exp` | Expired offramp auto-cancel |

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CARDANO_API_PORT` | `3000` | REST API port |
| `CARDANO_SWAP_EXPIRY_SECONDS` | `3600` | Swap/offramp expiry (seconds) |
| `CARDANO_MAX_ACTIVE_SWAPS` | `50` | Max concurrent swaps |
| `CARDANO_MAX_ACTIVE_OFFRAMPS` | `50` | Max concurrent offramps |
| `CARDANO_API_AUTH_TOKEN` | none | Bearer token for operator endpoints |
