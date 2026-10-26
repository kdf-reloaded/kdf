# Network Configuration

KDF-Reloaded supports multiple network identifiers (netids). Each netid has its own
fee parameters, discount rules, and (optionally) seed nodes compiled into the binary.
Production networks are always available. Test-only networks are compiled in only when
the `regtest-netid` Cargo feature is enabled.

The `"netid"` field in MM2.json is **required**. If omitted or set to an unsupported
value for the active build, the application will refuse to start and print the list of
supported networks.

## Supported Networks

Both production networks are supported side by side in one binary, and each uses its own
fee rules. Netid 8762 stays usable in KDF Reloaded: the upstream v3.0 releases name it a
retired identifier and refuse to start on it, and KDF Reloaded does not. Netid 6133 is the
GLEEC network, upstream's default since v3.0, with a 2% trading fee (1% with GLEEC on one
side of the pair). See the README for why the project keeps both.

### netid 8762 — AtomicDEX (Komodo)

The original AtomicDEX network.

| Parameter | Value |
|---|---|
| Base DEX fee rate | 1/777 (~0.129%) |
| Discounted tickers | KMD |
| Discounted fee rate | 9/7770 (~0.116%, 10% discount) |
| Minimum fee | Taker coin's minimum transaction amount (no additional network floor) |
| Burn | KMD taker only — three ranges on the dust-floored total: at or below dust, a single fee output for exactly dust; above dust with a 75% share at or above dust, a 75%/25% fee/OP_RETURN split; otherwise a fee output of exactly dust and an OP_RETURN burn of the remainder |
| No-fee waiver key | `0369aa10c061cd9e085f4adb7399375ba001b54136145cb748eb4c48657be13153` — a taker whose taker-coin swap public key equals this value pays no DEX fee, on both swap protocols, independently of the burn setting above |
| Version-two no-fee tickers | `KMD` — a version-two swap with KMD on either side of the pair carries no dex fee |
| Hardcoded seed nodes | None — provide `"seednodes"` in MM2.json |

**Operator note:** a node on netid 8762 has no default seed nodes. Without `"seednodes"` in
MM2.json (or `"i_am_seed": true`) it starts but finds no peers (found in swaplab testing,
2026-10-26). See [Seed Nodes](#seed-nodes).

Swap fee behavior on this network is pinned to the `v2.6.0-beta` compatibility
reference. Non-KMD takers keep the standard single-output fee transaction. For
a KMD taker, the exact split is dust-aware, not a flat 75/25: a 0.01 KMD trade
converts to a 1,000-base-unit fee output and a 158-base-unit OP_RETURN output,
matching a `v2.6.0-beta` node on the wire (issue #11) — not the 868/289 an
earlier revision of this project produced.

The version-two no-fee ticker exemption is now wired into the version-two
swap machinery itself (both roles, before and after negotiation): a
version-two swap with `KMD` on either side carries `NoFee` end to end, and
the taker-payment-spend preimage takes the single-output `NoFee` layout.
Previously the accessor existed but nothing in the version-two path consulted
it.

### netid 6133 — GLEEC DEX

The GLEEC decentralized exchange network.

| Parameter | Value |
|---|---|
| Base DEX fee rate | 2/100 (2%) |
| Discounted tickers | GLEEC |
| Discounted fee rate | 1/100 (1%, 50% discount) |
| Minimum fee | Taker coin's minimum transaction amount (no additional network floor) |
| Burn | Disabled — all takers use a single standard fee output |
| No-fee waiver key | `03a778d9bd346fa704cf3e2508cd074d93a1bbc1e504fbecbb0a8d48e7cccbbf5c` (equal to the fee key) — a taker whose taker-coin swap public key equals this value pays no DEX fee, on both swap protocols, even though burn is disabled |
| Version-two no-fee tickers | None |
| Hardcoded seed nodes | None — provide `"seednodes"` in MM2.json |

Swap fee behavior on this network follows the applicable unreleased v3/dev
compatibility reference. The configured burn key equals the fee key: it does
not enable a burn split (burn stays disabled), but it is still live as the
no-fee waiver key above — this does not depend on the burn setting.

### netid 7777 — Deprecated

Netid 7777 was the original Komodo DEX network. It is **no longer supported** and
the application will reject it at startup. Migrate to netid 8762.

## Test-only Networks

The following netids are used by the test harnesses and are available only when the
binary is built with `--features regtest-netid`:

| Netid | Typical use |
|---|---|
| 9998 | Default fixture for most unit and integration tests |
| 9000 | Docker test harness regtest network |
| 8999 | Tendermint / QRC20-focused test paths |
| 8100 | Targeted startup / bootstrap test scenarios |

These networks are intentionally not part of the production surface and must not be
relied on by release builds unless the regtest feature is enabled explicitly.

## Configuration

### Minimal MM2.json

```json
{
  "gui": "KDF-Reloaded",
  "netid": 8762,
  "rpc_password": "YOUR_RPC_PASSWORD",
  "passphrase": "your seed phrase here"
}
```

### Seed Nodes

Neither production network ships with hardcoded seed nodes. You must provide them via
the `"seednodes"` field in MM2.json, or run the node as a seed itself (`"i_am_seed": true`).

```json
{
  "netid": 8762,
  "seednodes": ["seed1.example.com", "seed2.example.com"],
  ...
}
```

If `"seednodes"` is omitted and `"i_am_seed"` is not set, the node will start
but will not be able to discover peers until it receives incoming connections.

## Adding a New Network

1. Create `mm2src/mm2_net_config/src/netid_XXXX.rs`
2. Implement a unit struct and the `NetConfig` trait
3. Register it in `net_config_for()` in `mm2src/mm2_net_config/src/lib.rs`
4. Add the netid to `SUPPORTED_NETIDS`
5. Update this document
