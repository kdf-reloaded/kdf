# Network Configuration

KDF-Reloaded supports multiple network identifiers (netids). Each netid has its own
fee parameters, discount rules, and (optionally) seed nodes compiled into the binary.
Production networks are always available. Test-only networks are compiled in only when
the `regtest-netid` Cargo feature is enabled.

The `"netid"` field in MM2.json is **required**. If omitted or set to an unsupported
value for the active build, the application will refuse to start and print the list of
supported networks.

## Supported Networks

### netid 8762 — AtomicDEX (Komodo)

The original AtomicDEX network.

| Parameter | Value |
|---|---|
| Base DEX fee rate | 1/777 (~0.129%) |
| Discounted tickers | KMD |
| Discounted fee rate | 9/7770 (~0.116%, 10% discount) |
| Burn | Disabled |
| Hardcoded seed nodes | None — provide `"seednodes"` in MM2.json |

### netid 6133 — GLEEC DEX

The GLEEC decentralized exchange network.

| Parameter | Value |
|---|---|
| Base DEX fee rate | 2/100 (2%) |
| Discounted tickers | GLEEC |
| Discounted fee rate | 1/100 (1%, 50% discount) |
| Burn | Enabled — 75% to fee address, 25% burned |
| Hardcoded seed nodes | None — provide `"seednodes"` in MM2.json |

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
