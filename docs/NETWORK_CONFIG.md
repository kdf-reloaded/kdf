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

### Electrum Servers over TLS

An Electrum server entry with `"protocol": "SSL"` is reached over TLS. The entry sits in the
`servers` list of the coin's activation request, or in the legacy `electrum` request. A server
given as an IP address cannot use `SSL`: it needs a host name, which the node also sends as the
TLS server name.

**By default** the node checks the server's certificate fully. It must chain to a root in the
node's bundled list of public roots, be valid today, not be a CA certificate, allow server
authentication and carry the server's host name. A self-signed certificate therefore fails.

**`disable_cert_verification: true`** skips all of those checks. The connection is still
encrypted, and the server must still prove that it holds the key in the certificate it presents.
That certificate must be a version 3 certificate with an accepted key (an RSA key of 2048 to
8192 bits, or a P-256, P-384 or Ed25519 key). A version 1 certificate or a 1024-bit RSA key is
refused even with the flag set. The server is not authenticated: anyone who can redirect your
traffic can present a certificate of their own. At activation the node logs one warning for each
such server, by host and port, once per activation of the coin.

**`spki_sha256`** pins a server to its key. Put it in the same server entry:

```json
{ "url": "electrum.example.org:50002", "protocol": "SSL",
  "spki_sha256": "9f2c...64 hexadecimal digits...e1" }
```

The value is the SHA-256 digest of the server's public key, as 64 hexadecimal digits in upper or
lower case, with colons between byte pairs allowed. It can also be an array of 1 to 4 such values,
so that you can pin the next key before a server changes it. It works for `SSL` servers only, and
not in a browser build, which does its own TLS and cannot pin.

With a pin, the node checks the key and nothing else: the certificate may be self-signed, expired
or issued for another name, as long as it carries a pinned key and the server proves it holds that
key. The pin is on the key, not the certificate, so it survives a renewal that keeps the key. If
`disable_cert_verification` is also set, the pin decides and the flag adds nothing; the warning is
not logged then, and one info line names the server as pinned. A pin never accepts more than the
pinned key does. What it gives up is the expiry check: a leaked key stays accepted until you
remove its pin.

To get the value, on the server, from its key file:

```
openssl pkey -in server.key -pubout -outform der | sha256sum
```

The first 64 hexadecimal digits are the value. Or, from anywhere, from the running server:

```
openssl s_client -connect <host>:<port> -servername <host> </dev/null 2>/dev/null | openssl x509 -pubkey -noout | openssl pkey -pubin -outform der | sha256sum
```

The second command is trust on first use done by hand: the value is only as good as the network
path it was read over. Compare it with the value the server's operator publishes, for example next
to the server's address, before you rely on it. Before the operator rotates a key, pin the new key
alongside the current one, so that the switch does not lock you out.

**When it goes wrong.** If the server's key matches none of the pins, the connection fails like any
other failed handshake, with the text `the server's key does not match spki_sha256`. The server is
suspended and tried again later, never removed, and every refusal is logged as a warning with the
server's host and port, because it means either that the key changed or that someone is in the
way. A pin that is not 64 hexadecimal digits (or an array that is empty, longer than 4, or holds a
non-string) removes that server from the coin's list at activation, with the text `spki_sha256 is
not a 64-digit hexadecimal SHA-256 digest`; the configured value is not repeated. A pin on a `TCP`
server removes it too, with `spki_sha256 needs protocol SSL`, so that a pin that is not enforced
never looks enforced. The coin's other servers are used.

> **Compatibility:** Komodo DeFi Framework and GLEEC KDF ignore `spki_sha256`, so an entry that
> carries it still works there, without the pin. See [GLEEC compatibility](GLEEC_COMPATIBILITY.md).

Read more: CRD chapter 38, R38.6.4m (what is checked, with and without the flag) and R38.6.4n (the
pin).

## Adding a New Network

1. Create `mm2src/mm2_net_config/src/netid_XXXX.rs`
2. Implement a unit struct and the `NetConfig` trait
3. Register it in `net_config_for()` in `mm2src/mm2_net_config/src/lib.rs`
4. Add the netid to `SUPPORTED_NETIDS`
5. Update this document
