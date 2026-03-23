# MS3 Acceptance Test — Local Devnet

**Date:** 2026-03-19
**Network:** Local devnet (Bitcoin regtest + yaci-devkit Conway-era Cardano)
**Test script:** `ms3_e2e_evidence.exp` (31 assertions)
**Result:** ALL 31/31 CHECKS PASSED

---

## Configuration

- **Lightning channel:** 500,000 sats (single channel, reused for all swaps)
- **Onramp amount:** 50,000 cBTC per swap (5 swaps)
- **Offramp amount:** 50,000 cBTC per swap (5 swaps)
- **Initial pool liquidity:** 50,000,000 cBTC
- **Final pool liquidity:** 40,000,000 cBTC (= 50M - 10M mini-onramp - 250K onramps + 250K offramps)

---

## Bitcoin Channel Lifecycle

| Event | TX Hash |
|---|---|
| Channel funding | `b87bcdc999104faaa667f6632e4e3cb3063b1b5fceeb642a3129d805bce9e8ad` |
| Channel close | Cooperative close completed (channel list empty) |

---

## Onramp Exchanges (Lightning BTC → cBTC)

| # | Payment Hash | CreateInvoice TX (Cardano) | FulfillInvoice TX (Cardano) | Pool After |
|---|---|---|---|---|
| 1 | `d99cd8d5...9c94d25b` | `28b900fa...390e9ae6` | `1405a172...31f162f3` | 49,950,000 |
| 2 | `c94c7d8d...6515b0be` | `9df24d5c...00400f30` | `ae7b7431...d196864c` | 49,900,000 |
| 3 | `b27dca40...ad498d6f` | `80e1f63d...bb114dde` | `2a7fbd1f...ca8f2b1a` | 49,850,000 |
| 4 | `ec38c39d...8c890f3b` | `021e836d...437b8ec9` | `eaa11377...2c4cd942` | 49,800,000 |
| 5 | `0bfd2450...af0a3f7f` | `70fd2e5d...8afe1cf9` | `7223bf5b...eb44f6cf` | 49,750,000 |

**Total onramped: 250,000 cBTC (5 x 50,000)**

Each onramp: payer sends Lightning payment → relay claims payment → relay submits FulfillInvoice TX on Cardano → cBTC transferred from pool to target address.

---

## Offramp Exchanges (cBTC → Lightning BTC)

| # | Payment Hash | cBTC Transfer TX (Cardano) | CreateOfframp TX (Cardano) | FulfillOfframp TX (Cardano) | Lightning Preimage | Pool After |
|---|---|---|---|---|---|---|
| 1 | `4ea86eb5...0ffb64d` | `db9a16aa...62c4fb26` | `aa6828c3...c98c9dd5` | `36d626f9...35cef8fb` | `4df5e9df...317cc0b3` | 39,800,000 |
| 2 | `162b58b7...22701385` | `3ae5cc0b...a251be74` | `b10f6037...6df684ed` | `6c1ceafd...7f67482b` | `00813cf2...31946081` | 39,850,000 |
| 3 | `10122d47...a1efd001` | `8c007d70...c755d561` | `c25094ee...bd77c473` | `52c831af...f36504d2` | `bb0d2a5f...ef743b9d` | 39,900,000 |
| 4 | `864516b9...71ee9ad3` | `40f45346...0c33657c` | `ebc5e652...84a4681b` | `f7f98438...224b5653` | `e8abcb79...d3583d1e` | 39,950,000 |
| 5 | `f8c1da37...2082d523` | `ad561012...153a766e` | `b7bcb95c...7aa975d2` | `15a7f588...ece82d35` | `3d63fc93...14c4063b` | 40,000,000 |

**Total offramped: 250,000 cBTC (5 x 50,000)**

Each offramp: user creates Lightning invoice → relay submits CreateOfframp TX → user sends cBTC to operator → relay verifies cBTC → relay pays Lightning invoice → relay submits FulfillOfframp TX (deposits cBTC to pool).

---

## Pool Liquidity Flow

```
Initial:           50,000,000 cBTC
After 5 onramps:   49,750,000 cBTC  (- 250,000)
After mini-onramp: 39,750,000 cBTC  (- 10,000,000 for channel balance shift)
After 5 offramps:  40,000,000 cBTC  (+ 250,000)
Final:             40,000,000 cBTC

Conservation: 50,000,000 - 250,000 (onramps) - 10,000,000 (mini) + 250,000 (offramps) = 40,000,000 ✓
```

---

## Assertions Summary

| Category | Count | Result |
|---|---|---|
| Per-onramp (payment + status + pool) | 15 (3 x 5) | PASS |
| Per-offramp (status + pool) | 10 (2 x 5) | PASS |
| Channel close | 1 | PASS |
| E2E checks (all completed, pool conservation, hashes) | 5 | PASS |
| **Total** | **31** | **ALL PASS** |

---

## MS3 Acceptance Criteria Met

| Criterion | Evidence |
|---|---|
| ≥5 end-to-end exchanges with locked funds | 5 onramp + 5 offramp exchanges (tables above) |
| Integration tests pass | 31/31 assertions passed |
| Logs recorded | `ms3_test_logs.log`, `ms3_evidence.json` |
| TX hashes (Bitcoin + Cardano) | Channel funding TX + 20 Cardano TXs (tables above) |
| Repo public, CI green | Relay CI: [passing](https://github.com/perun-network/cardano-lightning-relay/actions/runs/23300470386). LM CI: [passing](https://github.com/perun-network/lightning-liquidity-manager/actions/runs/23297504161). |

---

## How to Reproduce

Test script: [`test_scripts/ms3_e2e_evidence.exp`](../test_scripts/ms3_e2e_evidence.exp)

```bash
# 1. Build binaries
cd ~/pc-work/cardano-lightning-relay && cargo build --release
cd ~/pc-work/ldk-sample && cargo build --release

# 2. Clean devnet setup
cd ~/pc-work/lightning-liquidity-manager
bash ../cardano-lightning-docs/flows_e2e_runs/test_local_devnet.sh --clean

# 3. Run MS3 evidence test
cd ~/pc-work/cardano-lightning-relay/test_scripts
expect ms3_e2e_evidence.exp

# Output: ms3_evidence.json in the same directory
```
