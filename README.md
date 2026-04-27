# Cardano Lightning Relay

Lightning-to-Cardano bridge relay node. Combines an LDK Lightning node with a Cardano smart contract connector for cross-chain BTC/cBTC swaps.

Built on [LDK](https://lightningdevkit.org/) and the [Liquidity Manager](https://github.com/perun-network/lightning-liquidity-manager) Plutus V3 contract.

## Prerequisites

- **Rust** (edition 2024) — for native builds. Skip if you only run via Docker (see [Docker](#docker)).
- **bitcoind** (v25+) running in regtest or testnet mode — provided automatically by `docker-compose.signet.yml`.
- **Cardano devnet** ([yaci-devkit](https://github.com/bloxbean/yaci-devkit)) or Blockfrost API access for preprod/mainnet
- Only if you deploy a **fresh** Liquidity Manager yourself (see [Deploying a fresh Liquidity Manager](#deploying-a-fresh-liquidity-manager)):
  - [`aiken`](https://aiken-lang.org/installation-instructions) — to build and apply parameters to the validator
  - [`uv`](https://docs.astral.sh/uv/) — to run the Liquidity Manager Python deploy scripts
  - [`jq`](https://jqlang.org) — to extract values from JSON

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

The recommended way to run on Preprod is via Docker Compose — see [Option A](#option-a--docker-compose-signet--preprod-recommended) below. It bundles Signet bitcoind alongside the relay and pre-wires the RPC endpoint.

Key Preprod-specific bits the Compose setup handles for you:

- `CARDANO_BLOCKFROST_URL=https://cardano-preprod.blockfrost.io/api/v0/`
- Uses built-in `Network::Preprod` cost models (correct PlutusV3 canonical ordering)
- Fetches protocol params from Blockfrost for correct fee calculation

Working branches: [`feat-cardano`](https://github.com/perun-network/cardano-lightning-relay/tree/feat-cardano) (this repo) and [`feat-deploy-preprod`](https://github.com/perun-network/cardano-lightning-client/tree/feat-deploy-preprod) (the [cardano-lightning-client](https://github.com/perun-network/cardano-lightning-client) crate).

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

Two ways to run the relay in Docker.

### Option A — Docker Compose (Signet + Preprod, recommended)

`docker-compose.signet.yml` brings up `bitcoind` (Signet) and the relay in one go. The Bitcoin RPC endpoint is already wired (`relay:relay@bitcoind:38332`); you don't have to configure it yourself.

**1. Clone both repos as siblings in the same parent directory.** The build context is `..` because the relay has a path-dependency on the local `cardano-lightning-client` crate. **Both repos must be on their working feature branches** — `main` does not contain the Cardano + Docker work yet:

```bash
mkdir cardano-lightning && cd cardano-lightning
git clone -b feat-deploy-preprod https://github.com/perun-network/cardano-lightning-client
git clone -b feat-cardano        https://github.com/perun-network/cardano-lightning-relay
cd cardano-lightning-relay
```

**2. Create the secrets file.** Compose loads `secrets/preprod.env` via `env_file:`, so it must exist or Compose refuses to start:

```bash
cp secrets/preprod.env.example secrets/preprod.env
```

Edit `secrets/preprod.env` and fill in:

- `CARDANO_BLOCKFROST_KEY` — Preprod project ID from [blockfrost.io](https://blockfrost.io) (free tier is fine)
- `CARDANO_OPERATOR_ADDRESS` (`addr_test1v…`), `CARDANO_OPERATOR_PKH` (hex), `CARDANO_SCRIPT_ADDRESS` (`addr_test1w…`), `CARDANO_CBTC_POLICY_ID` (hex) — all four come from the operator/team in step 3

**3. Get the Cardano-side credentials from the operator/team.** The Liquidity Manager validator is parameterized with the operator's PKH, so the script address + CBOR are bound to a single operator key. Every relay running against the shared pool also runs as that same operator. Coordinate with the operator to receive (over a private channel — Slack, encrypted mail, etc.):

- `operator.sk` — Cardano signing key (sensitive)
- `script_cbor.hex` — parameterized validator CBOR (~10 kB hex)
- Four hex values for `secrets/preprod.env`:
  - `CARDANO_OPERATOR_ADDRESS`, `CARDANO_OPERATOR_PKH`
  - `CARDANO_SCRIPT_ADDRESS`
  - `CARDANO_CBTC_POLICY_ID` (asset name is the literal `63425443`, already in `preprod.env.example`)

Plus get your own free [Blockfrost](https://blockfrost.io) project ID for `CARDANO_BLOCKFROST_KEY`.

> **Important:** Only one relay should be live against the same pool at a time — the script's state UTxO is single-spend, so concurrent transactions from two relays will fail. Coordinate handover with the operator.

(If instead you want to spin up a **fresh, independent** LM deployment for solo development, see [Deploying a fresh Liquidity Manager](#deploying-a-fresh-liquidity-manager) below.)

**4. Drop the artefacts into `secrets/`:**

```bash
cp /path/to/operator.sk    secrets/operator.sk
cp /path/to/script_cbor.hex secrets/script_cbor.hex
chmod 600 secrets/operator.sk
```

Then edit `secrets/preprod.env` and fill in the four hex values plus the Blockfrost key.

**5. Build the image:**

```bash
docker compose -f docker-compose.signet.yml build
```

**6. Start bitcoind first and wait for it to sync Signet** (no pruning is configured; an initial sync typically takes one to several hours depending on the link):

```bash
docker compose -f docker-compose.signet.yml up -d bitcoind
# poll until "initialblockdownload": false:
docker exec bitcoind bitcoin-cli -signet \
  -rpcuser=relay -rpcpassword=relay getblockchaininfo
```

**7. Start the relay:**

```bash
docker compose -f docker-compose.signet.yml up -d relay
docker logs -f relay
```

REST API on `http://localhost:3002`, Lightning P2P on `9735`. Smoke check:

```bash
curl http://localhost:3002/pool/info
```

**Stop everything:**

```bash
docker compose -f docker-compose.signet.yml down
```

State persists in two named Docker volumes: `bitcoin-data` (chain state) and `relay-data` (LDK state + wallet seed at `/data/wallet_seed.txt`, auto-generated on first start if missing).

#### Deploying a fresh Liquidity Manager

Use this only if you want a **separate**, independent LM deployment (e.g. for solo development) — it produces a new operator key, a new script address, and a new cBTC token, **not** linked to any existing pool. To run against an existing shared pool, use the credential handover described in step 3 above instead.

```bash
cd ..
git clone -b fix/audit-2026-04-10 https://github.com/perun-network/lightning-liquidity-manager
cd lightning-liquidity-manager
uv sync
export CARDANO_NETWORK=preprod
export CARDANO_BLOCKFROST_KEY=<your_blockfrost_preprod_project_id>

# Generate operator key + write credentials/deployment.json skeleton
uv run scripts/config.py
# → prints the operator address. Fund it with at least 1000 tADA from
#   https://docs.cardano.org/cardano-testnets/tools/faucet, then continue:

aiken build
uv run scripts/init_contract.py 1000000   # mints + seeds pool with 1M cBTC
```

After this, `credentials/deployment.json` and `plutus-applied.json` contain everything you need to fill `secrets/preprod.env` + `secrets/script_cbor.hex` in the relay repo. Mapping:

| `secrets/preprod.env` variable | source field in `credentials/deployment.json`     |
| ------------------------------ | -------------------------------------------------- |
| `CARDANO_OPERATOR_ADDRESS`     | `.operator.address`                                |
| `CARDANO_OPERATOR_PKH`         | `.operator.pkh`                                    |
| `CARDANO_SCRIPT_ADDRESS`       | `.validator.parameterized_script_address`          |
| `CARDANO_CBTC_POLICY_ID`       | `.token.cbtc_policy`                               |
| `CARDANO_CBTC_ASSET_NAME`      | `.token.cbtc_asset_name`                           |

```bash
# Operator signing key
cp credentials/operator.sk ../cardano-lightning-relay/secrets/operator.sk

# Parameterized validator CBOR
jq -r '.validators[] | select(.title == "liquidity_manager.liquidity_manager.spend") | .compiledCode' \
  plutus-applied.json \
  > ../cardano-lightning-relay/secrets/script_cbor.hex
```

### Option B — Plain Docker (bring your own bitcoind)

Build from the **parent directory** containing both `cardano-lightning-relay/` and `cardano-lightning-client/`:

```bash
docker build -f cardano-lightning-relay/Dockerfile -t cardano-lightning-relay .
docker run --rm cardano-lightning-relay --help
```

Run against an external bitcoind. The entrypoint requires either positional arguments or `BITCOIN_RPC_URL` (full env-var list at the top of the [`Dockerfile`](Dockerfile)):

```bash
docker run -d --name relay \
  -p 9735:9735 -p 3002:3002 \
  -v relay-data:/data \
  -v "$(pwd)/secrets":/secrets:ro \
  --env-file secrets/preprod.env \
  -e BITCOIN_RPC_URL=user:pass@<bitcoind_host>:38332 \
  -e BITCOIN_NETWORK=signet \
  -e BITCOIN_ESPLORA_URL=https://mempool.space/signet/api \
  -e BITCOIN_WALLET_SEED_PATH=/data/wallet_seed.txt \
  -e LDK_MIN_CHANNEL_CONFIRMATIONS=1 \
  -e CARDANO_ENABLED=1 \
  -e CARDANO_API_PORT=3002 \
  cardano-lightning-relay
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
