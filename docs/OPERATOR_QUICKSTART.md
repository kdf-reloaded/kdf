# Operator quickstart

This guide takes a clean Linux host to a running KDF Reloaded node from the signed `v0.5.0-beta.1` release:
download and check the binary, write a minimal configuration, start the node, activate one UTXO coin and one EVM
coin, and place and cancel an order. It names the settings that are new in 0.5.0 and says where the node keeps its
logs and data.

It does not replace the API reference. For the full request and answer shapes of every method, see the
[Komodo DeFi Framework documentation](https://komodoplatform.com/en/docs/komodo-defi-framework/); the differences
are listed in [`RELOADED_VS_GLEEC.md`](../RELOADED_VS_GLEEC.md) and
[`API_REFERENCE_DEVIATIONS.md`](API_REFERENCE_DEVIATIONS.md).

> **Beta software.** This is a beta release. Start with small amounts, and read the
> [breaking changes and upgrade notes](../CHANGELOG.md#breaking-changes-and-upgrade-notes) before you use it with
> an existing wallet.

## 1. Requirements

- 64-bit Linux with glibc 2.31 or newer (Debian 11 or newer, Ubuntu 20.04 or newer, RHEL or Rocky 9). ARM boards
  and other systems: see [`RELEASE_ARTIFACTS.md`](RELEASE_ARTIFACTS.md).
- `curl`, `gpg` and `sha256sum`.
- A clock that is kept right, for example with `systemd-timesyncd` or `chrony`. The node checks the clock at
  start and refuses to start when it is more than 10 seconds off (see [the table in step 3](#3-configuration)).
- Outgoing UDP port 123 for that clock check, and outgoing TCP to the coins' servers and to the network's seed
  nodes.
- A user account of its own for the node. Do not run it as root.

## 2. Download and check the release

All commands run in one directory, for example `~/kdf`.

```sh
mkdir -p ~/kdf && cd ~/kdf
TAG=v0.5.0-beta.1
BASE=https://github.com/kdf-reloaded/kdf/releases/download/$TAG
curl -fLO "$BASE/kdf-$TAG-x86_64-unknown-linux-gnu"
curl -fLO "$BASE/SHA256SUMS"
curl -fLO "$BASE/SHA256SUMS.asc"
curl -fL -o takologi.asc "https://raw.githubusercontent.com/kdf-reloaded/kdf/$TAG/docs/keys/takologi.asc"
```

Check the signature of the checksum list, then the checksum of the binary:

```sh
gpg --import takologi.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS
```

`gpg --verify` must report a good signature from the key with the fingerprint
`FEE1 ACA5 2C65 FF3E BF31 818C B559 5E17 52BC 2A82`. `gpg` may add that the key is not certified with a trusted
signature; compare the fingerprint it prints with the one above. `sha256sum` must print `OK` for the binary. Stop
if either check fails.

```sh
mv "kdf-$TAG-x86_64-unknown-linux-gnu" kdf
chmod +x kdf
```

On a 64-bit ARM host take `kdf-$TAG-aarch64-unknown-linux-gnu` instead; every file name is listed in
[`RELEASE_ARTIFACTS.md`](RELEASE_ARTIFACTS.md).

## 3. Configuration

The node reads two files from the directory it starts in: `MM2.json`, its own settings, and `coins`, the
definitions of the coins it may activate. The environment variables `MM_CONF_PATH` and `MM_COINS_PATH` point to
other paths.

### The coins file

Take the community-maintained coins file:

```sh
curl -fL -o coins https://raw.githubusercontent.com/KomodoPlatform/coins/master/coins
```

The node activates a coin only if `coins` has an entry for its ticker. BNB, one of the examples below, is in the
file; RIN, the other, may not be in your copy yet and then needs an entry first (see
[Adding a coin entry](#adding-a-coin-entry)). Keep the file current: an old file can lack settings that swaps need,
such as gas limits.

### Adding a coin entry

A coin whose ticker is not in `coins` cannot be activated: the activation answers with an error that ends in
"assuming that coin is not supported". Add an entry for it to the array in `coins` before you
start the node; the file is read at start. Check whether your copy has RIN:

```sh
grep -c '"coin": "RIN"' coins
```

If that prints `0`, add this entry to the array (after the opening `[`, followed by a comma):

```json
{
  "coin": "RIN",
  "name": "rincoin",
  "fname": "Rincoin",
  "rpcport": 9556,
  "pubtype": 60,
  "p2shtype": 122,
  "wiftype": 188,
  "txfee": 10000,
  "dust": 5460,
  "segwit": true,
  "bech32_hrp": "rin",
  "sign_message_prefix": "rincoin Signed Message:\n",
  "mm2": 1,
  "required_confirmations": 2,
  "avg_blocktime": 60,
  "protocol": { "type": "UTXO" },
  "derivation_path": "m/44'/9555'"
}
```

| Field | Meaning |
|---|---|
| `coin` | The ticker used in every request. |
| `name`, `fname` | The coin's short and full name. |
| `pubtype`, `p2shtype`, `wiftype` | The version bytes of its addresses, script addresses and private keys. |
| `segwit`, `bech32_hrp` | Whether the coin has SegWit, and the prefix of its SegWit addresses. |
| `txfee` | The fee rate in the smallest unit per kilobyte; absent means 1,000, and `0` asks the servers for an estimate. |
| `dust` | The smallest output the network relays, in the smallest unit. |
| `sign_message_prefix` | The prefix of the coin's signed messages. |
| `mm2` | `1` lets the coin trade. |
| `required_confirmations` | How many confirmations a swap payment of this coin needs by default. |
| `avg_blocktime` | The average time between blocks, in seconds. |
| `protocol` | The coin family; `UTXO` here. |
| `derivation_path` | The start of the coin's HD wallet path. |
| `rpcport` | The RPC port of the coin's full node, used in native mode. |

Take the values from the coin's own project and check each one: a wrong value can produce addresses or
transactions the network does not accept, and a coin's values can change with its network upgrades. The coin is
then activated as in [step 5](#5-activate-coins), with its own Electrum servers on at least two hosts.

### A minimal `MM2.json`

```json
{
  "gui": "kdf-reloaded-quickstart",
  "netid": 8762,
  "passphrase": "<your 12 or 24 word seed phrase>",
  "rpc_password": "<8 to 32 characters>",
  "dbdir": "/home/kdf/kdf-db",
  "seednodes": ["<seed host 1>", "<seed host 2>"]
}
```

- **`passphrase`** is the wallet seed: everything the node controls is derived from it. Use a new seed phrase
  for the node, generated by a wallet or tool you trust, and keep a copy offline. Never reuse a seed phrase that
  appears in examples. Protect the file: `chmod 600 MM2.json`.
- **`rpc_password`** protects the RPC interface. It must be 8 to 32 characters long, contain a digit, a lowercase
  and an uppercase letter and a character that is neither, must not contain the word "password", and must not
  repeat one character three times in a row; the node does not start otherwise.
- **`netid`** selects the trading network: 8762 (AtomicDEX) or 6133 (GLEEC DEX). The fees of each network are in
  [`NETWORK_CONFIG.md`](NETWORK_CONFIG.md#supported-networks).
- **`dbdir`** is where the node keeps its databases (see [step 8](#8-logs-and-data)). The default is `DB` in the
  start directory. Use an absolute path on a disk that is backed up.
- **`seednodes`** lists the seed nodes of the network. Neither network has seed nodes built in; without this list
  the node starts but finds no peers until another node connects to it. The coins repository lists public seed
  nodes in `seed-nodes.json`, each with its `netid`; take hosts of your network only, and check after the start
  that the node reached them ([step 4](#4-start-the-node-and-check-its-version)). See
  [Seed Nodes](NETWORK_CONFIG.md#seed-nodes).

The RPC interface listens on `127.0.0.1:7783` and accepts requests only from this host. Keep it that way, or
put an authenticated proxy in front of it.

### Settings new in 0.5.0

All of them are optional top-level members of `MM2.json`; leave them out to get the default.

| Setting | Default | Change it when |
|---|---|---|
| `swap_evidence_allow_single_host` | `true` | Set `false` to require two hosts, or a trusted node, for every coin. With `true`, a coin whose configured server list names one host trusts that host (see [step 5](#5-activate-coins)). |
| `swap_evidence_node` (activation field, per coin) | not set | You want a swap of that coin to act on one node of its list alone, for example your own server. When that node does not answer, the swap waits. |
| `swap_evidence_probe_attempts` | `3` | The check before a swap starts gives up on a host after this many questions in a row; raise it on a slow link. |
| `swap_evidence_probe_timeout_s` | `5` | The time limit of one such question, in seconds; raise it for servers far away. |
| `swap_evidence_host_log_interval_s` | `3600` | How often the log repeats the line about a silent host while nothing changes, in seconds. |
| `setprice_replace_after_checks` | `false` | Set `true` so that `setprice` with `cancel_previous` cancels your previous orders on the pair only after the new order passed all its checks. A failed re-pricing then leaves the old orders live at their old price. |
| `ntp_servers` | `0.pool.ntp.org` to `3.pool.ntp.org` | Your network blocks the public NTP pool, or you run your own time servers: list up to 8 servers. `[]` turns the clock check off. |
| `ntp_required` | `false` | Set `true` so that the node does not start, and refuses new orders and swaps, while no time server answers. |

The clock check runs at start, every hour, and after the computer slept or its clock was set. A clock more than
10 seconds off stops the start with an error that says by how much it is off; while the node runs, it refuses new
orders and swaps until the clock is right, and running swaps go on.

An invalid value of any of these settings stops the start with an error that names it. Read more:
[Clock Check](NETWORK_CONFIG.md#clock-check), [Swap evidence from one host](NETWORK_CONFIG.md#swap-evidence-from-one-host),
[Swap evidence readiness](NETWORK_CONFIG.md#swap-evidence-readiness).

## 4. Start the node and check its version

```sh
cd ~/kdf
MM_LOG=$HOME/kdf/kdf.log ./kdf
```

The node keeps running in the foreground. With `MM_LOG` set it writes its log to that file and prints nothing; the
first line of the log names the version, and the next lines name the configuration file, the coins file and the log
file in use. For a long-running node, run it under a service manager such as systemd with `WorkingDirectory` set to
`~/kdf`.

In a second terminal, keep the RPC password in a shell variable and ask for the version:

```sh
RPC=http://127.0.0.1:7783
PASS='<your rpc_password>'
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"version\"}"
```

The answer looks like `{"result":"R0.5.0-beta.1-<hash>","datetime":"<build time>"}`: `R` marks KDF Reloaded,
`0.5.0-beta.1` is the version and `<hash>` the source revision it was built from. Mention the whole `result` when
you report a problem.

Check that the node reached the network:

```sh
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"get_directly_connected_peers\"}"
```

The answer lists the connected peers with their addresses. An empty list, together with the log warning "P2P relays
below low watermark", means that no seed node was reached: check `seednodes` and `netid`. Without peers the node
still activates coins, but its orders reach nobody.

To stop the node, send `{"userpass":"...","method":"stop"}` or press Ctrl-C in its terminal.

## 5. Activate coins

### The two-node rule

Before a swap refunds a payment, or uses the swap's secret, the chain reads it acts on must agree between two
nodes of the coin on different hosts, or come from one node you named as trusted with `swap_evidence_node`. Two
ports of one host count as one node. So:

- list servers on at least two hosts for every coin you trade;
- a coin whose list names exactly one host trusts that host while `swap_evidence_allow_single_host` is `true` (the
  default); its activation answer then carries `"swap_evidence_single_host": true`, and the node logs one warning;
- a coin in native mode, served by your own full node, needs nothing more.

When the reads of a running swap do not agree, the swap waits and logs how to fix it; it never refunds or claims on
such a read. Read more: [the breaking change about two nodes](../CHANGELOG.md#breaking-changes-and-upgrade-notes)
and CRD [chapter 51 §51.6G](reloaded-rewrite/51-legacy-v1-swap-state-machine.md).

### A UTXO coin over Electrum (RIN)

```sh
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"electrum\",\"coin\":\"RIN\",
  \"servers\":[{\"url\":\"peer1.rincoin.online:50002\",\"protocol\":\"SSL\"},
               {\"url\":\"peer2.rincoin.online:50002\",\"protocol\":\"SSL\"},
               {\"url\":\"peer3.rincoin.online:50002\",\"protocol\":\"SSL\"}]}"
```

The answer has `"result":"success"`, the address and the balance. List at least three servers: with fewer, the node
cannot always tell a stale server from one that follows another chain, and it warns at activation (see
[Electrum server lists](NETWORK_CONFIG.md#electrum-server-lists)). The published server lists of the coins are in
the `electrums` folder of the coins repository.

### An EVM coin with two hosts (BNB)

```sh
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"mmrpc\":\"2.0\",\"method\":\"enable_eth_with_tokens\",
  \"params\":{\"ticker\":\"BNB\",
    \"nodes\":[{\"url\":\"https://bsc-dataseed3.bnbchain.org\"},{\"url\":\"https://bsc-rpc.publicnode.com\"}],
    \"swap_contract_address\":\"0xeDc5b89Fe1f0382F9E4316069971D90a0951DB31\",
    \"fallback_swap_contract\":\"0xeDc5b89Fe1f0382F9E4316069971D90a0951DB31\",
    \"erc20_tokens_requests\":[]}}"
```

The two nodes are on two hosts, so swap reads of BNB are agreed between them. The swap contract addresses and
further public nodes of each EVM coin are in the `ethereum` folder of the coins repository; take the addresses from
there for your coin. To trust one node alone, add `"swap_evidence_node":"<one url of nodes>"` to `params`.

`get_enabled_coins` lists what is active:

```sh
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"get_enabled_coins\"}"
```

## 6. Place and cancel an order

A maker order offers one coin for another at a price. It needs a balance of the coin you offer:

```sh
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"setprice\",
  \"base\":\"RIN\",\"rel\":\"BNB\",\"price\":\"0.00002\",\"volume\":\"100\"}"
```

The answer carries the order and its `uuid`. By default `setprice` cancels your previous orders on the same pair
(`cancel_previous`); see `setprice_replace_after_checks` in [step 3](#settings-new-in-050) for how that cancel can
wait until the new order has passed its checks.

**A paused order.** When a coin of the pair cannot give the agreed chain reads right now (for example, fewer than
two of its hosts answer), the order is still accepted and saved, but it is not announced to the network. The
`setprice` answer then carries `"paused": true` and `"paused_reason"`, for example
`"RIN: swap evidence not available (fewer than two hosts answering)"`. `my_orders` and `order_status` show
`"paused": true` while the order is paused, and the log has one warning per order that starts paused. The order is
announced again by the next order refresh once the coin's servers answer; nothing needs to be done. When an order
is not paused, these members are absent.

A taker's `buy` or `sell` on such a coin is refused with the `error_type` `SwapEvidenceNotReady` and a text that
names the coin and the reason; try again when its servers answer.

List and cancel your orders:

```sh
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"my_orders\"}"
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"cancel_order\",\"uuid\":\"<uuid>\"}"
curl -s $RPC --data "{\"userpass\":\"$PASS\",\"method\":\"cancel_all_orders\",\"cancel_by\":{\"type\":\"All\"}}"
```

Cancelling an order works at any time, also while new orders are refused.

## 7. While a swap runs

Keep the node running until its swaps have finished: a swap needs the node for its claim or refund.
`my_recent_swaps` lists the swaps and `my_swap_status` shows one. A swap whose chain reads stay undetermined ends
as refund failed after a bounded wait, with a text that names the fix; after fixing the server list, run
`recover_funds_of_swap` for it.

## 8. Logs and data

- **Log.** The node logs to its standard output. With `MM_LOG` set to a path that ends in `.log`, it writes the log
  to that file instead; the start lines say which file is used.
- **Data.** Everything else lives under `dbdir` (default `DB` in the start directory):
  - `<dbdir>/<identity>/`: one folder per wallet identity, named by a hexadecimal identifier. It holds `MM2.db`, the
    swap and order database, the swap files under `SWAPS/` (`SWAPS/MY` for your swaps, `SWAPS/STATS` for
    statistics), the saved orders under `ORDERS/` and the transaction history under `TRANSACTIONS/`;
  - `<dbdir>/` itself holds wallet files that do not belong to one identity.

Back up the whole `dbdir` while the node is stopped. The seed phrase alone restores the funds, but not the record of
running swaps: a swap that was in progress needs its files to claim or refund.

## 9. Upgrade and downgrade

1. Let running swaps finish (check `my_recent_swaps`), or note them; then stop the node.
2. Back up `dbdir` and `MM2.json`, and keep the old binary.
3. Read [Breaking changes and upgrade notes](../CHANGELOG.md#breaking-changes-and-upgrade-notes) of every release
   between your version and the new one. Some changes cannot be undone by going back, for example a database that
   is migrated on the first start, and some need a step from you before the upgrade.
4. Replace the binary, checked as in [step 2](#2-download-and-check-the-release), and start the node.

To go back, stop the node, restore the backed-up `dbdir` if the upgrade notes say that the new version changed it,
and start the old binary. The upgrade notes say for each such change what an older version can still read.
