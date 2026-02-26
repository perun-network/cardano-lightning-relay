# Cardano Lightning Relay

Lightning-to-Cardano bridge relay node. Combines an LDK Lightning node with a Cardano smart contract connector for cross-chain BTC/cBTC swaps.

Built on [LDK](https://lightningdevkit.org/) and the [Liquidity Manager](https://github.com/perun-network/lightning-liquidity-manager) Plutus V3 contract.

## Prerequisites

- **Rust** (edition 2024)
- **bitcoind** (v25+) running in regtest or testnet mode
- **Cardano devnet** ([yaci-devkit](https://github.com/bloxbean/yaci-devkit)) or Blockfrost API access for preprod/mainnet

## Quick Start (Local Devnet)

### 1. Start bitcoind (regtest)

```bash
bitcoind -regtest -rpcuser=user -rpcpassword=pass -daemon

bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass \
  createwallet testwallet

bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass \
  -rpcwallet=testwallet -generate 101
```

### 2. Deploy the Cardano contract

```bash
cd lightning-liquidity-manager
bash scripts/test_local_devnet.sh --clean
```

This starts yaci-devkit, generates operator credentials, mints test cBTC, and deploys the Liquidity Manager contract with 50M cBTC initial pool. See the [lightning-liquidity-manager](https://github.com/perun-network/lightning-liquidity-manager) repo for details.

### 3. Build and run the relay

```bash
cargo build --release

export CARDANO_ENABLED=true
export CARDANO_SKEY_PATH=path/to/operator.sk
export CARDANO_SCRIPT_ADDRESS=addr_test1w...
export CARDANO_SCRIPT_CBOR_PATH=path/to/script_cbor.hex
export CARDANO_CBTC_POLICY_ID=<policy_hex>
export CARDANO_CBTC_ASSET_NAME=63425443
export CARDANO_OPERATOR_ADDRESS=addr_test1v...
export CARDANO_OPERATOR_PKH=<pkh_hex>
# For local devnet (defaults):
# CARDANO_BLOCKFROST_URL=http://localhost:8080/api/v1/
# CARDANO_BLOCKFROST_KEY=local
# CARDANO_API_PORT=3000

./target/release/cardano-lightning-relay \
  user:pass@127.0.0.1:18443 ./ldk_data 9735 regtest
```

## Preprod

For Cardano Preprod deployment, see the [`feat-deploy-preprod`](https://github.com/perun-network/cardano-lightning-relay/tree/feat-deploy-preprod) branch. The key differences:

- `CARDANO_BLOCKFROST_URL=https://cardano-preprod.blockfrost.io/api/v0`
- `CARDANO_BLOCKFROST_KEY=<your-blockfrost-project-id>`
- Uses built-in `Network::Preprod` cost models (fixes PlutusV3 canonical ordering)
- Fetches protocol params from Blockfrost for correct fee calculation

The [cardano-lightning-client](https://github.com/perun-network/cardano-lightning-client) library (`feat-deploy-preprod` branch) contains the fix.

## REST API

The relay exposes a REST API on port 3000 (configurable via `CARDANO_API_PORT`).

### Pool Management

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/pool/info` | Query pool state (total_liquidity, reserved, available) |
| `POST` | `/pool/deposit` | Deposit cBTC into pool. Body: `{"amount": <i64>}` |
| `POST` | `/pool/withdraw` | Withdraw cBTC from pool. Body: `{"amount": <i64>}` |

### Onramp (Lightning BTC -> Cardano cBTC)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/swap/request` | Request swap. Body: `{"amount_cbtc": <i64>, "cardano_address": "<addr>"}` |
| `GET` | `/swap/status/{hash}` | Query swap status by payment hash |

### Offramp (Cardano cBTC -> Lightning BTC)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/offramp/request` | Request offramp. Body: `{"bolt11": "<invoice>", "amount_cbtc": <i64>, "cardano_address": "<addr>"}` |
| `POST` | `/offramp/deposit` | Notify cBTC deposit. Body: `{"offramp_id": <i64>, "cbtc_tx_hash": "<hash>"}` |
| `GET` | `/offramp/status/{id}` | Query offramp status by ID |

## Docker

Build from the **parent directory** containing both `cardano-lightning-relay/` and `cardano-lightning-client/`:

```bash
docker build -f cardano-lightning-relay/Dockerfile -t cardano-lightning-relay .
docker run --rm cardano-lightning-relay --help
```

## Nix

```bash
nix build   # produces result/bin/cardano-lightning-relay
nix develop  # dev shell with Rust 1.85, pkg-config, openssl
```

Requires the [cardano-lightning-client](https://github.com/perun-network/cardano-lightning-client) repo as a sibling directory.

## E2E Tests

Tests use [Expect](https://core.tcl-lang.org/expect/index) scripts that automate the full flow.

```bash
# Set up devnet (required before each test)
bash test_scripts/test_local_devnet.sh --clean

# Run the channel lifecycle test (from the relay repo root)
expect test_scripts/ms2_channel_lifecycle_test.exp  # 5 channel lifecycles (30 assertions)
```

Test evidence: [evidence_ms2/](evidence_ms2/)

## CLI Commands

The relay has an interactive CLI. Type `help` for all commands. Key Cardano commands:

- `pool-info` — Query pool state
- `cardano-deposit <amount>` — Deposit cBTC
- `cardano-withdraw <amount>` — Withdraw cBTC
- `cancel-expired` — Cancel expired invoices

Standard LDK commands (`openchannel`, `closechannel`, `sendpayment`, `getinvoice`, etc.) are also available.

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE) or http://www.apache.org/licenses/LICENSE-2.0).
