# MS2 Acceptance Test — Preprod

**Date:** 2026-02-26
**Network:** Cardano Preprod
**Script Address:** `addr_test1wqf2s47jetde0ne0n0u9vfrm04q363n5ycnzyupen5j20hqple0j8`
**Operator Address:** `addr_test1vzcxl4ejs4tqd5w7yh4p9wgpczl7zt4saegu8kcv7ehu6zsfsvfz4`
**cBTC Policy:** `c1474e383d24055c747cdfbddb33223276e675bab82ba8969ac13a36`
**Initial Pool Liquidity:** 50,000,000 cBTC (TX `860d0240d44475179dc6fc514acaa522b5ee7b51a9cd5a5d7d1854654ca67ab6`)

---

## Phase 1: Python Script Deposits/Withdrawals

These transactions were submitted via Python scripts (`init_contract.py`, direct TX building).

### Deposits (Python)

| # | Amount (cBTC) | TX Hash | Pool After | Explorer |
|---|--------------|---------|------------|----------|
| 1 | 100,000 | `a28daeb4a1287f11f2182d305d6b455f7377b4faff4aacc15ac62495d3c9dd9f` | 50,100,000 | [link](https://preprod.cardanoscan.io/transaction/a28daeb4a1287f11f2182d305d6b455f7377b4faff4aacc15ac62495d3c9dd9f) |
| 2 | 150,000 | `e5f41df15afb8d7e1bbed5f14fe36d7a1b2d7f62effbaa6fdefdf7d9307fec50` | 50,250,000 | [link](https://preprod.cardanoscan.io/transaction/e5f41df15afb8d7e1bbed5f14fe36d7a1b2d7f62effbaa6fdefdf7d9307fec50) |
| 3 | 200,000 | `88283f523c8764070a74c40fb0029cd0b759fbf77b93f62aa80e373834ce0b8b` | 50,450,000 | [link](https://preprod.cardanoscan.io/transaction/88283f523c8764070a74c40fb0029cd0b759fbf77b93f62aa80e373834ce0b8b) |
| 4 | 250,000 | `04ccbbdb59cc375623205c9b2d21356fd3b2dc723e7c09249501b53d3523a78b` | 50,700,000 | [link](https://preprod.cardanoscan.io/transaction/04ccbbdb59cc375623205c9b2d21356fd3b2dc723e7c09249501b53d3523a78b) |
| 5 | 300,000 | `a60c8c8fcd944858a0c68ec64cbef64763cf22c5269e3f684394c288f5899e37` | 51,000,000 | [link](https://preprod.cardanoscan.io/transaction/a60c8c8fcd944858a0c68ec64cbef64763cf22c5269e3f684394c288f5899e37) |

**Total Deposited (Python): 1,000,000 cBTC**

### Withdrawals (Python)

| # | Amount (cBTC) | TX Hash | Pool After | Explorer |
|---|--------------|---------|------------|----------|
| 1 | 80,000 | `d7ba05ca984ec585e9fd2b3ae1974b20902404fd70f83e608c79b55a9d6ccbe6` | 50,920,000 | [link](https://preprod.cardanoscan.io/transaction/d7ba05ca984ec585e9fd2b3ae1974b20902404fd70f83e608c79b55a9d6ccbe6) |
| 2 | 100,000 | `8ab3971dcf7b9f4cab6b61b691149b4c27cd04c2857ceeac3fe24b01ec11147f` | 50,820,000 | [link](https://preprod.cardanoscan.io/transaction/8ab3971dcf7b9f4cab6b61b691149b4c27cd04c2857ceeac3fe24b01ec11147f) |
| 3 | 120,000 | `67bf2183a88db9609f355dfd78d7413061d1b6f1435a2c517ecc905d2eb91a28` | 50,700,000 | [link](https://preprod.cardanoscan.io/transaction/67bf2183a88db9609f355dfd78d7413061d1b6f1435a2c517ecc905d2eb91a28) |
| 4 | 150,000 | `3a477307c5bdbcf65e63bedffa7f24577fbe932914a1b66e669ce1cf3400c906` | 50,550,000 | [link](https://preprod.cardanoscan.io/transaction/3a477307c5bdbcf65e63bedffa7f24577fbe932914a1b66e669ce1cf3400c906) |
| 5 | 180,000 | `df7b0e2d198b3e44fdfb898e5e4525cee8ec4f604388b61a074c153ed068dac0` | 50,370,000 | [link](https://preprod.cardanoscan.io/transaction/df7b0e2d198b3e44fdfb898e5e4525cee8ec4f604388b61a074c153ed068dac0) |

**Total Withdrawn (Python): 630,000 cBTC**

**Pool after Phase 1: 50,370,000 cBTC**

---

## Phase 2: Connector-Driven (Relay REST API) Deposits/Withdrawals

These transactions were submitted via the `cardano-lightning-relay` REST API (`POST /pool/deposit`, `POST /pool/withdraw`), fulfilling MS2 criterion 4: "Connector-driven deposits/withdrawals."

The relay (`cardano-lightning-relay`) was started with Cardano Preprod configuration (Blockfrost API) and its REST API on port 3000.

### Deposits (Relay)

| # | Amount (cBTC) | TX Hash | Pool After | Explorer |
|---|--------------|---------|------------|----------|
| 1 | 100,000 | `9dee1446ca816068a1aa835ac32c3163ff955f4c827d24cd9b65a2dbcfafbdf7` | 50,470,000 | [link](https://preprod.cardanoscan.io/transaction/9dee1446ca816068a1aa835ac32c3163ff955f4c827d24cd9b65a2dbcfafbdf7) |
| 2 | 150,000 | `382b572f1d1cea5764965da2b341555b46c374ef9ba79d8287a24125a39b6d84` | 50,620,000 | [link](https://preprod.cardanoscan.io/transaction/382b572f1d1cea5764965da2b341555b46c374ef9ba79d8287a24125a39b6d84) |
| 3 | 200,000 | `708e3b7bdbdfb9cad4bea16b8f604000cf68434cab0cf352af44f575200c151e` | 50,820,000 | [link](https://preprod.cardanoscan.io/transaction/708e3b7bdbdfb9cad4bea16b8f604000cf68434cab0cf352af44f575200c151e) |
| 4 | 250,000 | `fc63a4b4d3301cf57b6d344be998cfa03ff35d12576eb35a43e75667492b9959` | 51,070,000 | [link](https://preprod.cardanoscan.io/transaction/fc63a4b4d3301cf57b6d344be998cfa03ff35d12576eb35a43e75667492b9959) |
| 5 | 300,000 | `9f17b1e4eccc3e9a11cd4241be050ee1f90db3fe96aa55280bb0290e2ee7f250` | 51,370,000 | [link](https://preprod.cardanoscan.io/transaction/9f17b1e4eccc3e9a11cd4241be050ee1f90db3fe96aa55280bb0290e2ee7f250) |

**Total Deposited (Relay): 1,000,000 cBTC**

### Withdrawals (Relay)

| # | Amount (cBTC) | TX Hash | Pool After | Explorer |
|---|--------------|---------|------------|----------|
| 1 | 80,000 | `8838c3d303fc1b77975d6dff05066d1d10c1a40bde1f6bf9990e9c0566a018cb` | 51,290,000 | [link](https://preprod.cardanoscan.io/transaction/8838c3d303fc1b77975d6dff05066d1d10c1a40bde1f6bf9990e9c0566a018cb) |
| 2 | 100,000 | `0ba76cb86c3ce36a203e80c91e43c182c12c107b58de839110f5165573bdcca4` | 51,190,000 | [link](https://preprod.cardanoscan.io/transaction/0ba76cb86c3ce36a203e80c91e43c182c12c107b58de839110f5165573bdcca4) |
| 3 | 120,000 | `bd8fe22ebf6b30a95209af3e56d5d7748e4acb345df7d800d488e7e2e0ef47cc` | 51,070,000 | [link](https://preprod.cardanoscan.io/transaction/bd8fe22ebf6b30a95209af3e56d5d7748e4acb345df7d800d488e7e2e0ef47cc) |
| 4 | 150,000 | `d5f3085f043ab8ac108e20a23c8a0da520fea2bfc42f76e16efea3603dcbfc0c` | 50,920,000 | [link](https://preprod.cardanoscan.io/transaction/d5f3085f043ab8ac108e20a23c8a0da520fea2bfc42f76e16efea3603dcbfc0c) |
| 5 | 180,000 | `5a6e2191298404b45d7cde175772740b8d688ab7feb7bc2b6d8d8ae9d5b43503` | 50,740,000 | [link](https://preprod.cardanoscan.io/transaction/5a6e2191298404b45d7cde175772740b8d688ab7feb7bc2b6d8d8ae9d5b43503) |

**Total Withdrawn (Relay): 630,000 cBTC**

**Pool after Phase 2: 50,740,000 cBTC**

---

## Summary

### Phase 1 (Python Scripts)
- **Deposits:** 1,000,000 cBTC (5 TXs)
- **Withdrawals:** 630,000 cBTC (5 TXs)
- **Net Change:** +370,000 cBTC
- **Pool:** 50,000,000 → 50,370,000

### Phase 2 (Connector/Relay REST API)
- **Deposits:** 1,000,000 cBTC (5 TXs)
- **Withdrawals:** 630,000 cBTC (5 TXs)
- **Net Change:** +370,000 cBTC
- **Pool:** 50,370,000 → 50,740,000

### Combined
- **Total Deposited:** 2,000,000 cBTC (10 TXs)
- **Total Withdrawn:** 1,260,000 cBTC (10 TXs)
- **Net Change:** +740,000 cBTC
- **Initial Pool:** 50,000,000 cBTC
- **Final Pool:** 50,740,000 cBTC (= 50,000,000 + 2,000,000 - 1,260,000)
- **All checks passed:** Yes

## Full TX Chain

1. Faucet → `818476e9a42f1b6148d89319a66fa6dc4d6c918029342437825d588ab036db8e` (10,000 ADA)
2. Mint cBTC → `213aeec8724d3ca3c713d54d34db0c9b1f81fd9a624320cc9e138da64a16bc31` (100,000,000 cBTC)
3. Init contract → `860d0240d44475179dc6fc514acaa522b5ee7b51a9cd5a5d7d1854654ca67ab6` (pool: 50,000,000)
4. Deposit #1 (Python) → `a28daeb4a1287f11f2182d305d6b455f7377b4faff4aacc15ac62495d3c9dd9f` (pool: 50,100,000)
5. Deposit #2 (Python) → `e5f41df15afb8d7e1bbed5f14fe36d7a1b2d7f62effbaa6fdefdf7d9307fec50` (pool: 50,250,000)
6. Deposit #3 (Python) → `88283f523c8764070a74c40fb0029cd0b759fbf77b93f62aa80e373834ce0b8b` (pool: 50,450,000)
7. Deposit #4 (Python) → `04ccbbdb59cc375623205c9b2d21356fd3b2dc723e7c09249501b53d3523a78b` (pool: 50,700,000)
8. Deposit #5 (Python) → `a60c8c8fcd944858a0c68ec64cbef64763cf22c5269e3f684394c288f5899e37` (pool: 51,000,000)
9. Withdrawal #1 (Python) → `d7ba05ca984ec585e9fd2b3ae1974b20902404fd70f83e608c79b55a9d6ccbe6` (pool: 50,920,000)
10. Withdrawal #2 (Python) → `8ab3971dcf7b9f4cab6b61b691149b4c27cd04c2857ceeac3fe24b01ec11147f` (pool: 50,820,000)
11. Withdrawal #3 (Python) → `67bf2183a88db9609f355dfd78d7413061d1b6f1435a2c517ecc905d2eb91a28` (pool: 50,700,000)
12. Withdrawal #4 (Python) → `3a477307c5bdbcf65e63bedffa7f24577fbe932914a1b66e669ce1cf3400c906` (pool: 50,550,000)
13. Withdrawal #5 (Python) → `df7b0e2d198b3e44fdfb898e5e4525cee8ec4f604388b61a074c153ed068dac0` (pool: 50,370,000)
14. Deposit #1 (Relay) → `9dee1446ca816068a1aa835ac32c3163ff955f4c827d24cd9b65a2dbcfafbdf7` (pool: 50,470,000)
15. Deposit #2 (Relay) → `382b572f1d1cea5764965da2b341555b46c374ef9ba79d8287a24125a39b6d84` (pool: 50,620,000)
16. Deposit #3 (Relay) → `708e3b7bdbdfb9cad4bea16b8f604000cf68434cab0cf352af44f575200c151e` (pool: 50,820,000)
17. Deposit #4 (Relay) → `fc63a4b4d3301cf57b6d344be998cfa03ff35d12576eb35a43e75667492b9959` (pool: 51,070,000)
18. Deposit #5 (Relay) → `9f17b1e4eccc3e9a11cd4241be050ee1f90db3fe96aa55280bb0290e2ee7f250` (pool: 51,370,000)
19. Withdrawal #1 (Relay) → `8838c3d303fc1b77975d6dff05066d1d10c1a40bde1f6bf9990e9c0566a018cb` (pool: 51,290,000)
20. Withdrawal #2 (Relay) → `0ba76cb86c3ce36a203e80c91e43c182c12c107b58de839110f5165573bdcca4` (pool: 51,190,000)
21. Withdrawal #3 (Relay) → `bd8fe22ebf6b30a95209af3e56d5d7748e4acb345df7d800d488e7e2e0ef47cc` (pool: 51,070,000)
22. Withdrawal #4 (Relay) → `d5f3085f043ab8ac108e20a23c8a0da520fea2bfc42f76e16efea3603dcbfc0c` (pool: 50,920,000)
23. Withdrawal #5 (Relay) → `5a6e2191298404b45d7cde175772740b8d688ab7feb7bc2b6d8d8ae9d5b43503` (pool: 50,740,000)

## MS2 Evidence Criteria Met

- **Criterion 4 (Milestone2.md):** "List of Preprod TX hashes for Connector-driven deposits/withdrawals"
  - **Phase 2** provides 10 Connector-driven TX hashes (5 deposits + 5 withdrawals via relay REST API)
  - **Phase 1** provides additional 10 TX hashes via Python scripts for completeness
- **5 deposits executed** via Connector with verifiable on-chain state transitions
- **5 withdrawals executed** via Connector with verifiable on-chain state transitions
