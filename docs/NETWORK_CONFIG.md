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

**Redials.** A node that holds fewer relays than it needs dials seeds and known peers every
30 seconds. It dials each address at most once per 5 minutes, as GLEEC KDF does, so a seed that
drops a connection soon after it was made is redialed up to 5 minutes later. It does not dial a
seed it is already connected to, whichever side opened the connection. While nothing is left to
dial, the warning "P2P relays below low watermark" appears at most once per 5 minutes.

### Hostnames from Other Peers

Nodes ask each other for peer addresses every five minutes, and some peers give a seed by its
hostname instead of its IP address. KDF Reloaded looks such a name up through the
[DNS resolver](#dns-resolver) the node uses. It does at most one lookup at a time, looks the same name up at most once per
30 minutes, and does no lookup while it is connected to that peer. It keeps the result only if it
is a public IPv4 address on the network's port (see below for tests), and stores the address,
never the name. Its own answers to other peers carry only such addresses. To turn this off, set:

```json
{
  "p2p_resolve_peer_hostnames": false,
  ...
}
```

The node then skips every hostname a peer gives, without a lookup, and its own answers give each
peer it dialed under the address it dialed, hostnames included.

> **Compatibility:** for behavior matching GLEEC KDF (and the upstream pre-divergence Komodo DeFi
> Framework), set this to `false`. *(GLEEC KDF never looks up a hostname it receives from a
> peer.)*

### Private Addresses (Tests Only)

By default, a node keeps a peer address it learns from another peer only if it is a public IPv4
address on the network's port. For a test network where every node has a private address, set:

```json
{
  "p2p_allow_private_addresses": true,
  ...
}
```

The node then also keeps private, loopback and link-local IPv4 addresses, including those that a
hostname resolves to, and logs a warning at start-up. Do not use this setting in production.

> **Compatibility:** GLEEC KDF has no equivalent. Leave at the default to retain GLEEC-equivalent
> behavior.

### DNS Resolver

Every name the node looks up tells the DNS servers it asks which seeds, coins and services you
use. One setting chooses those servers for every name:

| `dns_resolver` | The node asks |
|---|---|
| absent, `null` or `"google"` (the default) | Google Public DNS: `8.8.8.8`, `8.8.4.4`, `2001:4860:4860::8888`, `2001:4860:4860::8844` |
| `"system"` | your computer's own resolver, exactly as other programs do |
| an array of up to 8 addresses | only those DNS servers, in the order given |

An address in the array is an IPv4 or IPv6 address, with an optional port (53 by default):

```json
{
  "dns_resolver": ["192.0.2.53", "192.0.2.54:5353", "[2001:db8::53]:53"],
  ...
}
```

Any other value (a misspelled keyword, an empty array, a hostname) stops the node at start with
an error naming the key. The node never falls back to the default: a resolver you did not choose
would see every name.

**When the node looks names up.** Not only at start:
- A seed is looked up again each time it is redialed, which includes every reconnect after a
  dropped connection.
- A hostname that another peer hands out is looked up when it arrives, at most once per 30
  minutes.
- An Electrum or lightwalletd server is looked up at each connect and reconnect, including
  when the node moves to another server.
- An EVM, Solana, Tendermint or other HTTP(S) node is looked up whenever a new connection to it
  is opened, for example after an idle connection was closed or when the node rotates to
  another node.

**Cache.** With Google Public DNS or your own servers, answers are kept in memory for as long as
their DNS records allow, but at least 30 seconds and at most an hour. An answer that a name does
not exist is kept between 5 seconds and 5 minutes. Nothing is written to disk, so a restart
starts with an empty cache. With `"system"`, your computer's resolver does its own caching.

**When the resolver can't be reached.** With Google Public DNS or your own servers, the node uses
the last answer it got for a name, for up to 24 hours after that answer expired, and logs a
warning once an hour. It does that only when no server answered, or a server reported its own
failure, never when a server answered that the name does not exist. No other resolver is ever
asked.

**When a name can't be resolved.** Only the connection that needed it fails, and it is retried
as usual. The node keeps running. A warning, at most once per hour per name, says which resolver
was asked and that `dns_resolver` chooses it.

**At start** the node logs one line naming the resolver it uses.

**What the setting does not hide:**
- DNS queries go unencrypted on port 53, so the networks they cross can see and block them.
- The names still appear in the TLS handshake and the HTTP headers sent to each server.
- With the default, Google Public DNS learns every name the node looks up.
- With Google Public DNS or your own servers, the computer's hosts file (`/etc/hosts`) is read
  first, when the node starts. A name it lists is answered from it, and a later change to the
  file applies at the next start.
- Seeds and servers given as IP addresses need no lookup at all.

**Not yet covered.** Sia, the WalletConnect relay, Lightning peer addresses and the statistics
method still use your computer's resolver, whatever `dns_resolver` says.

> **Compatibility:** GLEEC KDF has no such setting. It looks seeds up through Google Public DNS,
> as KDF Reloaded's default does, and every other name through the computer's resolver, as
> `"system"` does. No single value reproduces that mix. See
> [GLEEC compatibility](GLEEC_COMPATIBILITY.md).

## Adding a New Network

1. Create `mm2src/mm2_net_config/src/netid_XXXX.rs`
2. Implement a unit struct and the `NetConfig` trait
3. Register it in `net_config_for()` in `mm2src/mm2_net_config/src/lib.rs`
4. Add the netid to `SUPPORTED_NETIDS`
5. Update this document
