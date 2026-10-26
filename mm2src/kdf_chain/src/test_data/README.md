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

