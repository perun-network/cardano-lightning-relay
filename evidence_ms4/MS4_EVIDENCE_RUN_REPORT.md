# MS4 Evidence Run Report

**Date:** 2026-04-21
**Networks:** Bitcoin Signet + Cardano Preprod
**Result:** 10/10 onramps + 10/10 offramps completed, 53/53 checks passed

---

## Setup

### Infrastructure

| Component | Details |
|---|---|
| Bitcoin | Signet (tip ~300,900 at time of run) |
| Cardano | Preprod via Blockfrost API (`https://cardano-preprod.blockfrost.io/api/v0/`) |
| Relay backend | Esplora+BDK (mempool.space Signet API for wallet ops, bitcoind for block sync only) |
| Relay binary | `cardano-lightning-relay` on branch `feat-esplora-backend` |
| Payer binary | `ldk-sample` (standard LDK sample node) |
| Script | `ms4_evidence_collection.exp` (automated expect script) |

### Addresses

| Role | Address |
|---|---|
| Cardano operator | `addr_test1vzcxl4ejs4tqd5w7yh4p9wgpczl7zt4saegu8kcv7ehu6zsfsvfz4` |
| Cardano script (LM contract) | `addr_test1wqf2s47jetde0ne0n0u9vfrm04q363n5ycnzyupen5j20hqple0j8` |
| cBTC policy ID | `c1474e383d24055c747cdfbddb33223276e675bab82ba8969ac13a36` |
| cBTC asset name | `63425443` (hex for "cBTC") |

### Lightning Channel

| Field | Value |
|---|---|
| Channel ID | `0f7f1ae048c43a6cffb8298f1984324089307548b9df2b353d3831eb96d052eb` |
| Capacity | 20,000 sats |
| Payer node ID | `031e6bbad2a0ff4875bd0c5766ffe376e5f7d81fdeca249134d24f8e73e415b392` |
| Relay node ID | `03c634668a0753c8cb0788a252211bdb71ff880bc99cfbd0154420695013b9237b` |
| Status | Pre-existing from earlier test runs; RESUME mode (no new channel open needed) |

The channel was opened in an earlier session (funding TX `eb52d096...` confirmed at Signet block 300,883). The evidence run reused it in RESUME mode, preserving LDK state from disk. Channel became ready within 5 seconds of node startup.

---

## What Happened

### Overview

The `ms4_evidence_collection.exp` script ran 20 sequential swaps through a single Lightning channel:

1. **10 onramp swaps** (BTC via Lightning -> cBTC on Cardano Preprod): 1,000 sats each
2. **10 offramp swaps** (cBTC on Cardano Preprod -> BTC via Lightning): 50 sats each

Each swap was verified against three assertions:
- Lightning payment succeeded (preimage captured)
- Swap/offramp status reached "Completed" on the relay
- Pool liquidity changed by the exact expected amount

### Timeline

- **Start:** 2026-04-21T14:18:17 UTC
- **Onramps 1-10:** ~18 minutes (each onramp takes ~1-2 min: API request, Lightning payment, Cardano CreateInvoice + FulfillInvoice TX, Blockfrost indexing wait)
- **Offramps 1-10:** ~35 minutes (each offramp takes ~3-4 min: payer invoice creation, API request, CreateOfframp TX, Blockfrost indexing, pycardano cBTC self-send, indexing, relay Lightning payment, FulfillOfframp TX)
- **Total runtime:** ~55 minutes

### Pool Liquidity Flow

| Event | Pool (cBTC) | Change |
|---|---|---|
| Initial state | 15,801,000 | - |
| After onramp 1 | 14,801,000 | -1,000,000 |
| After onramp 2 | 13,801,000 | -1,000,000 |
| After onramp 3 | 12,801,000 | -1,000,000 |
| After onramp 4 | 11,801,000 | -1,000,000 |
| After onramp 5 | 10,801,000 | -1,000,000 |
| After onramp 6 | 9,801,000 | -1,000,000 |
| After onramp 7 | 8,801,000 | -1,000,000 |
| After onramp 8 | 7,801,000 | -1,000,000 |
| After onramp 9 | 6,801,000 | -1,000,000 |
| After onramp 10 | 5,801,000 | -1,000,000 |
| After offramp 1 | 5,851,000 | +50,000 |
| After offramp 2 | 5,901,000 | +50,000 |
| After offramp 3 | 5,951,000 | +50,000 |
| After offramp 4 | 6,001,000 | +50,000 |
| After offramp 5 | 6,051,000 | +50,000 |
| After offramp 6 | 6,101,000 | +50,000 |
| After offramp 7 | 6,151,000 | +50,000 |
| After offramp 8 | 6,201,000 | +50,000 |
| After offramp 9 | 6,251,000 | +50,000 |
| After offramp 10 | 6,301,000 | +50,000 |
| **Final** | **6,301,000** | **-9,500,000 net** |

**Expected final:** 15,801,000 - (10 x 1,000,000) + (10 x 50,000) = 6,301,000. **Exact match.**

---

## Onramp Evidence (BTC via Lightning -> cBTC on Cardano)

Each onramp: payer sends Lightning payment -> relay claims payment -> relay submits CreateInvoice TX on Cardano -> relay submits FulfillInvoice TX (transfers cBTC from pool to target address).

| # | Payment Hash | CreateInvoice TX (Cardano) | FulfillInvoice TX (Cardano) | Pool After |
|---|---|---|---|---|
| 1 | `ae3a68f3b7665268baae65e4f4aa81d9f796ef9ca08346867c0bc6a59239a007` | `1a737be2f936b99cd1242fca0eec187f0c1542279c4f38d6b3686cee12155dda` | `77e76bd12147fcfdfee330833b8bbae7b8f22b886ff452bc378822d85183a2e6` | 14,801,000 |
| 2 | `29aabc86419b53a0590369de3a4608a3b3a1335ec2b05d3b30567094feecce4a` | `6a6a6746fea2e9f54a2fdb6f2aa2f5c3bc327d9a41c4e089bf47f3adc27110f1` | `f2bbf5892e4ff85966443fdc7dc845133459db1d526c7548c9463256bb7152e5` | 13,801,000 |
| 3 | `67c280014ce3e2f04078cab4cf57a052e5947cf1c9fd99dd008e636a2c850eca` | `bb39f3d03eff4befd80e9d0f919793914408234ffffccf6a120af06e8ca2c44b` | `dee507ba0058296359d9bfd9aded4597943433697e9b98c6b153dc9923c0cf40` | 12,801,000 |
| 4 | `ff15291fb9d8dae97ae7b36eb7da5e9e5b638b2635aceff32d245f40a11d3d7a` | `a8e2e28cd07c0a3c87b6e4e945891d4dbbf9f3974b6b946695a2cbe9043bb66c` | `bdbf438a00bf4f95e00a633e389d044628c8fce7c737e35d857e6fd5d5578cd9` | 11,801,000 |
| 5 | `9ec2b75c0b5b59da629004ea81cff390ee6958d155dc8682ddf048c136114c29` | `eea3e8c3a4da4ede3ed8010d197891d22d0ce14e2d4c1892009a3a12b4aaf963` | `2275501d94c14b8717170c42b4b1e9dc47f3b1a864405766c2d5777737800e0f` | 10,801,000 |
| 6 | `3ceb3517f7881d36ac0cc58e08c5342ad6f4d61b939cbc3a758fdb7f1441ba65` | `0cfc45d04f5a8a48536edd7dfd7027c09bed1afad86d4ea7d3b6451e862cb400` | `ae050d0816e63a82e4f583fe9ffb47cb64e11285be6b4b5256cda8ab128fe351` | 9,801,000 |
| 7 | `d242b615b67bc93ac827e8d4c4f52a3f2b808f72c0aea2c218daf4f435443ac2` | `6dd9209ad616d30df5fca9381f5f990b26e76f1a86ffe8d6f300df5d678291aa` | `a398205914942b1d67a50105b391439fc02f9cc0659fb6df353e0b38438e2e66` | 8,801,000 |
| 8 | `1c998b06923c62d34321ceabfd7c0f8536a291c6dbeb0d3e8619b82825e18f0c` | `70805f5ad28735506623ab591c311389f9eca652ac243c2b6d8e088d143a1c93` | `045396ff957295ad976642721bc76cc43ae0dde3f9c9c8326e23de7f37501bbe` | 7,801,000 |
| 9 | `6d03321a5ac5652bdce30e4f51c96a422c24ea5b80aa4d5e71d0cd5a8b43864b` | `65a0b3a2e695e8c72ac367ed3e182ab5a3dc8da212821b5f2cb05d50e0028842` | `bf3e7a2304be066f20805c1e9e7fe780f3e3c6fb07c3ecb07d7bd52c10ddb3c9` | 6,801,000 |
| 10 | `22d39fe96a35b543f28964a7673eb9064501cde1f3d0135422d0183288ae7363` | `1482000558f5ab851b753f9e2178418cf29cd29af66abaf703a070b01a6b831b` | `e460bd09783ee503fb1d12a0b2f3012649ad677482bcb6cda87dd0b230b89f87` | 5,801,000 |

All Cardano TXs are on Preprod and verifiable at `https://preprod.cardanoscan.io/transaction/<hash>`.

---

## Offramp Evidence (cBTC on Cardano -> BTC via Lightning)

Each offramp: user creates Lightning invoice on payer -> relay submits CreateOfframp TX -> user sends cBTC to operator -> relay verifies cBTC deposit -> relay pays Lightning invoice -> relay submits FulfillOfframp TX (deposits cBTC to pool).

| # | Payment Hash | cBTC Transfer TX | CreateOfframp TX | FulfillOfframp TX | Lightning Preimage | Pool After |
|---|---|---|---|---|---|---|
| 1 | `b3913f22...570b460` | `cc294d5e...1b3f42` | `62d5bb02...b8a21f` | `767f7057...cb0665` | `1f5ad24e...00a4ff` | 5,851,000 |
| 2 | `693d8121...41bfbd` | `889e0e59...980c77` | `806c0018...b67231` | `f536fe4d...c2e337` | `49102920...9d85b9` | 5,901,000 |
| 3 | `dec8732e...94ea74` | `3b3eb060...191d16` | `ea986c6e...f83448` | `f672b84b...ae4e88` | `7b83f620...8d033e` | 5,951,000 |
| 4 | `864f0fc4...76e5cd` | `b99812ac...76c953` | `61f84fab...dc372c` | `0aaaa20b...9fce5e` | `9a640d78...46448f` | 6,001,000 |
| 5 | `f0647731...f901f0` | `de963b6e...6e5078` | `54d130bc...f0e354` | `31bb7878...232271` | `4a5a5551...1d2f5c` | 6,051,000 |
| 6 | `e4ed0511...b5064c` | `abe11eac...946871` | `89316cbb...b6d5ee` | `1a2dcf2b...1d26e6` | `becc195e...46ee3c` | 6,101,000 |
| 7 | `8bb6fef4...8813ae` | `e4b64102...d287a5` | `c13f28a4...88196d` | `0a388897...3748eb` | `5395361b...b29d9a` | 6,151,000 |
| 8 | `896bd34a...9dd415` | `d8819bf4...823ae9` | `7975dc46...c35942` | `b6b29cc7...27bb4b` | `6fe1278c...bef149` | 6,201,000 |
| 9 | `4a920de7...c57455` | `f9df658d...437a81` | `44cd227a...673d75` | `41f41609...cd8131` | `dadd9279...96441e` | 6,251,000 |
| 10 | `2b5cfe44...42b9cd` | `0eaf5642...f41467` | `96282385...b4e22f` | `56ba749f...719d62` | `20fc54a4...4a6d42` | 6,301,000 |

Full hashes are in `ms4_evidence.json`. All Cardano TXs verifiable on Preprod Cardanoscan.

---

## Onramp Preimages (Lightning proof-of-payment)

| # | Preimage |
|---|---|
| 1 | `c5d01ee54dbc75eb10b4b644477a2605f95be0f54308f93a83f704c074b145ee` |
| 2 | `b068bd8e23015ae5fd2c125ee9469e889d58788c9782eb00b6b8fe68440814cf` |
| 3 | `801c7a742e58ffd58f3f3a043c5991ee88d11563bcc61f50b6fc49d65e0e387d` |
| 4 | `2bca31391762d6e5896c7d9df1c8dfd0d3909a6b1294ca6349bbfa050312486a` |
| 5 | `a836adc0ce077b2327c294fd990de2c2a289e9537fa0a48c069ad453d54042a0` |
| 6 | `239939b0702ad30fb166ea4ede2adc2acbccf09e9e53d2754775d68532bedbc7` |
| 7 | `21c38c9f62c28645e62a0142ed6470da76b1af38890c7af9044a2289126cf9a9` |
| 8 | `5ce238f5e74364063e290892b4be40e9bbcb8152a2ccaec48885caa46de5771f` |
| 9 | `71dc8c79b171324de1d3fd53d83796e127255a054f87155277596537f704326f` |
| 10 | `0d578893e338fc730e444f75377e5fa4c25d4e43a8fbe764df2e00fbbd9bb49c` |

---

## Assertions

All 53 assertions passed:

- **Per-onramp (3 each x 10):** Lightning payment succeeded, swap status = Completed, pool decreased by 1,000,000 cBTC
- **Per-offramp (2 each x 10):** Offramp status = completed, pool increased by 50,000 cBTC
- **Global (3):** >= 10 onramps completed, >= 10 offramps completed, final pool matches expected value

---

## Observations

### Blockfrost Indexing Latency

Preprod Blockfrost indexing varied from ~10s to ~120s per TX. Most onramps completed within 30s (6 status polls at 5s each). One onramp (#2) took longer — the FulfillInvoice TX was submitted but Blockfrost hadn't indexed it within the 120s polling window. It did complete successfully (confirmed by pool accounting), and the TX hashes were recovered from the relay's log output.

The offramp poll-based approach (polling for CreateOfframp TX in operator UTxOs before proceeding with the cBTC self-send) worked reliably — no UTxO contention failures across all 10 offramps.

### Channel Economics

With a 20,000 sat channel:
- 10 onramps at 1,000 sats each pushed 10,000 sats from payer to relay
- 10 offramps at 50 sats each pushed 500 sats from relay back to payer
- Net: relay gained ~9,500 sats of outbound capacity
- Channel remained healthy throughout — no routing failures

### Plutus TX Conservation

Every Cardano TX is a Plutus script execution against the liquidity manager contract. The pool balance tracked perfectly across all 20 TXs with zero drift. The cBTC conservation invariant held: total pool change = -(10 x 1,000,000) + (10 x 50,000) = -9,500,000 cBTC, matching the observed 15,801,000 -> 6,301,000.

---

## Files

| File | Description |
|---|---|
| `ms4_evidence.json` | Structured evidence data (all TX hashes, amounts, pool states) |
| `ms4_evidence_run.log` | Full raw output from the evidence collection script |
| `../flows_e2e_runs/ms4_evidence_collection.exp` | The automated evidence collection script |

---

## How to Verify

### Cardano TXs (Preprod)

Any CreateInvoice, FulfillInvoice, CreateOfframp, FulfillOfframp, or cBTC transfer TX hash from the tables above can be verified at:

```
https://preprod.cardanoscan.io/transaction/<tx_hash>
```

### Pool Contract

The liquidity manager contract is at script address:
```
addr_test1wqf2s47jetde0ne0n0u9vfrm04q363n5ycnzyupen5j20hqple0j8
```

Query its UTxO on Preprod to see the current pool state (should show ~6,301,000 cBTC in the inline datum).

### Lightning

Lightning payments are off-chain and not verifiable on a block explorer. The evidence for each payment is the preimage: knowing the preimage proves the payment was completed, since only the recipient can reveal it (HTLC cryptographic guarantee).
