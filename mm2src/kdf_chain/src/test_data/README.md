# Header test fixtures

Hex-encoded header bytes exactly as an Electrum server returned them. The file name is
`<coin>_<first height>_x<count>_<note>.hex` (`lone` for a single header).

## PIVX and LBC fixtures (fetched 2026-10-26)

Fetched without a checkpoint height, with `blockchain.block.headers(start, 11)`, over the public
Electrum protocol (protocol 1.4), from the servers the coin registry lists: PIVX from
`electrum01.chainster.org:50001` (ElectrumX 1.19.0, tip 5642868), LBC from
`lbc.electrum1.cipig.net:10067` (ElectrumX 2.0.0, tip 2209636). Every window returned all 11
headers.

| File | Chain heights | Header versions | Served bytes |
|---|---|---|---|
| `pivx_100000_x11_v3.hex` | 100000..100010 | 3 | 11 x 80 |
| `pivx_863780_x11_v3_to_v4.hex` | 863780..863790 | 3 (7 headers), then 4 (4 headers) | 7 x 80 + 4 x 112 |
| `pivx_1500000_x11_v4.hex` | 1500000..1500010 | 4 | 11 x 112 |
| `pivx_1900000_x11_v5.hex` | 1900000..1900010 | 5 | 11 x 112 |
| `pivx_2153195_x11_v6_to_v7.hex` | 2153195..2153205 | 6 (5 headers), then 7 (6 headers) | 5 x 112 + 6 x 80 |
| `pivx_2400000_x11_v7.hex` | 2400000..2400010 | 7 | 11 x 80 |
| `pivx_2700495_x11_v7_to_v8.hex` | 2700495..2700505 | 7 (5 headers), then 8 (6 headers) | 5 x 80 + 6 x 112 |
| `pivx_3000000_x11_v10_v11.hex` | 3000000..3000010 | 10 and 11, interleaved | 11 x 112 |
| `pivx_5642858_x11_v11_tip.hex` | 5642858..5642868 | 11 | 11 x 112 |
| `lbc_1_x11.hex` | 1..11 | `0x20000000` | 11 x 112 |
| `lbc_500000_x11.hex` | 500000..500010 | `0x20000000` | 11 x 112 |
| `lbc_2000000_x11.hex` | 2000000..2000010 | `0x20000000` | 11 x 112 |
| `lbc_2209626_x11_tip.hex` | 2209626..2209636 | `0x20000000` | 11 x 112 |

The PIVX windows reproduce the version thresholds of CRD ch.37 section 37.5.2 / R37.5.5 on live
data: 80 bytes for versions 3 and below and for 7, 112 bytes for 4 to 6 and for 8 and above.

## Runebase (RUNES) fixtures (fetched 2026-10-26)

Same method, from `electrum2.runebase.io:50002` over TLS with certificate verification
(Runebase ElectrumX 1.16.0, tip 2953207). The coin is a `"QTUM"` family coin (CRD ch.37 R37.5.5),
so its headers are Qtum-shaped: the standard 80 bytes, the state and UTXO roots, the stake prevout and
a length-prefixed block signature.

| File | Chain heights | Served bytes |
|---|---|---|
| `runes_2953197_x11_qtum_tip.hex` | 2953197..2953207 | 11 x 246 (2706 bytes) |
| `runes_500000_x11_qtum.hex` | 500000..500010 | 11 x 251 (2761 bytes) |


## Kerrigan (KRGN) fixtures (fetched 2026-11-18 and 2026-11-22)

Real Kerrigan chain data, fetched on 2026-11-18 from a public Kerrigan Electrum server
(ElectrumX 1.19.0, protocol 1.4, tip 188838) with
`blockchain.block.headers(188818, 11)` and `blockchain.block.header(height)`. The four mining
algorithms use four layouts (CRD ch.37 R37.5.11); the algorithm is bits 8 to 11 of the version.

| File | Chain heights | Algorithm and size |
|---|---|---|
| `krgn_188818_mixed_sizes.hex` | 188818..188828 | 11 headers, 8492 bytes: Equihash(200,9) x4 (1487 each), Equihash(192,7) x4 (543 each), KawPoW x3 (124 each); sizes in order 1487, 1487, 543, 543, 543, 124, 124, 1487, 1487, 543, 124 |
| `krgn_191181_window11.hex` | 191181..191191 | 11 headers, 7085 bytes: X11 x1 (80), KawPoW x3 (124 each), Equihash(192,7) x4 (543 each), Equihash(200,9) x3 (1487 each); sizes in order 124, 124, 1487, 80, 543, 543, 543, 124, 543, 1487, 1487. All four sizes in one `blockchain.block.headers(191181, 11)` answer of a public Kerrigan Electrum server on 2026-11-22 |
| `krgn_188629_lone_x11.hex` | 188629 | X11, 80 bytes |
| `krgn_188823_lone_kawpow.hex` | 188823 | KawPoW, 124 bytes |
| `krgn_188818_lone_equihash_200_9.hex` | 188818 | Equihash(200,9), 1487 bytes |
| `krgn_188820_lone_equihash_192_7.hex` | 188820 | Equihash(192,7), 543 bytes |

## Dash-layout transaction fixtures (fetched 2026-11-22)

One transaction per file, raw bytes as hex, exactly as returned by `blockchain.transaction.get` from a public DASH
Electrum server (DASH rows) or a public Kerrigan Electrum server (Kerrigan row). Each hex hashes (double SHA-256,
reversed) to its txid. They back CRD ch.38 T38.6.10.1 to T38.6.10.4.

| File | Chain, height | txid | Version / type | Inputs / outputs | Payload |
|---|---|---|---|---|---|
| `dash_2576221_type5_cbtx.hex` | DASH 2576221 | `82efdfbb4adb79d9c590cd83c316acb8c40d51c975a519733ff2d420d469e7ed` | 3 / 5 | 1 / 3 | 175 bytes |
| `dash_2576104_type1_proregtx.hex` | DASH 2576104 | `8a334dff3c0c0d7616b809a837798a70294c5fbd0e36604191de75d2670caf4e` | 3 / 1 | 1 / 1 | 274 bytes |
| `dash_2576170_type9_assetunlock.hex` | DASH 2576170 | `0bfefcc1c944d1a0c885a1de834d7587c4069d944b6b28e9c2e16b80a07a3cc4` | 3 / 9 | 0 / 1 | 145 bytes |
| `dash_2576220_v3_type0.hex` | DASH 2576220 | `0d9754c759a993b6ca598a2c6dae7935ffaeeceb77498da1e280437946b5ec8b` | 3 / 0 | 19 / 2 | none |
| `krgn_191125_type10_sapling.hex` | Kerrigan 191125 | `6b99a0b216da1e96b2eacc0ed9db4993301342f45d21c3a3dfeafbfeadd2ad11` | 3 / 10 | 0 / 1 | 2356 bytes |
| `dash_2576219_type6_qc.hex` | DASH 2576219 | `c28c2cf4b6aa304430b0c5ef53763fcd8398451307843e54f5618f4da87f6d07` | 3 / 6 | 0 / 0 | 341 bytes |
