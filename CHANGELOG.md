# Changelog

All notable changes to KDF Reloaded are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 releases use the `0.MAJOR.MINOR-PRERELEASE.N` convention; expect breaking changes between alpha and beta.

## [Unreleased]

<!-- How to add an entry: put it under the matching ### group and #### area (add the area heading if it is missing), as a bold one-sentence summary in plain words, then the detail, ending with "Detail: PR #N; CRD ch.X RY". A change that an older version can no longer read or undo, or that needs a step from the user before or after upgrading, also gets an emphasized plain-words item under "### Breaking changes and upgrade notes" at the top of the section. -->
<!-- The same PR updates every affected document: the CRD chapter, the plan body and its "Plans touched" row, RELOADED_VS_GLEEC.md, docs/GLEEC_COMPATIBILITY.md and "Known limitations" below; a plan's Status line is left to the maintainer. -->

### Breaking changes and upgrade notes

Read this before you upgrade. It lists every change in this release that an older version can no longer read
or undo, and every change that needs a step from you.

- **A new swap needs two nodes of the coin that agree, or one node you name as trusted.** Before a swap refunds, or
  uses the swap's secret, the chain reads that step rests on (the other side's payment, whether a payment was
  claimed, refunded or not spent, and the chain time of the refund) must agree between two nodes of the coin. Two
  ports of one host are one node; two hosts of one operator are two nodes. A coin in native mode is served by your
  own daemon and needs nothing, and by default a coin whose node list names only one host trusts that host (see
  *Single-host coins* below).
  - *What changes:* a coin with two or more distinct hosts in its server or node list swaps only on reads two of them
    agree on. With `swap_evidence_allow_single_host` set to `false`, a coin with fewer than two distinct hosts and no
    `swap_evidence_node` also starts no new swap: `setprice`, `buy`, `sell` and the swap start refuse it with a
    reason that names `swap_evidence_node`. A swap that is already running when you upgrade keeps running, but a step
    whose reads do not agree, or that has too few answers, waits and logs how to fix it; it never refunds or claims
    on such a read. A refund step waits like this until an hour after the refund first became possible in the current
    run, and at least until its payment's lock time plus 3700 seconds; a swap still waiting then ends as refund
    failed, with a text that names the kind of failure and, for reads that did not agree, the fix; after fixing the
    node list, or once the coin answers again, you run `recover_funds_of_swap`.
  - *What to do:* list a server on a second host in the activation request, or set the new activation field
    `swap_evidence_node` to one entry of the coin's own list to trust that node alone. When the trusted node does
    not answer, the step waits; no other node is asked in its place.
  - *Single-host coins:* the new top-level `MM2.json` boolean `swap_evidence_allow_single_host`, default `true`,
    keeps a coin whose configured node list names exactly one host swapping as before. That host is trusted as if
    `swap_evidence_node` named it, the coin's activation answer carries `"swap_evidence_single_host": true`, and the
    node logs one warning naming the coin. The same holds for the bchd servers of an SLP token's platform: a BCH
    platform whose `bchd_urls` name exactly one host trusts it for the token's reads, and its activation answer
    carries `"swap_evidence_single_bchd_host": true`. The list as configured counts, not the hosts that answer: a
    coin listed with two or more hosts stays under the two-node rule even while one of them does not answer. Set the
    switch to `false` to require two hosts or `swap_evidence_node` for every coin. Detail: PR #536; CRD ch.51 status
    note of 2026-11-24 before §51.7.
  - *Coin families covered* (the rule is switched on family by family as each family's chain reads move onto it):
    - TRON (TRX, TRC20). TRON swaps stay off in this release; when they are switched on, their chain reads already
      follow the rule. Detail: PR #509; CRD ch.21 §21.11D.
    - **UTXO coins over Electrum, BCH and QTUM.** Affected: every such coin whose Electrum server list names two or
      more hosts (two ports of one host count as one): its swap steps wait until two servers agree. A coin whose list
      names fewer than two hosts, for example a coin activated with a single server, trusts that server by default;
      with `swap_evidence_allow_single_host` set to `false` it no longer starts a swap: `setprice`, `buy`, `sell` and
      the swap start refuse it, and its running swaps wait at a refund or claim step until two servers agree. To fix
      it, add a server on a second host to the activation request, or set `swap_evidence_node` to one of the coin's
      servers to trust that server alone. A coin in native mode reads your own daemon and needs nothing. Detail: PR
      #522; CRD ch.38 status note of 2026-11-23 before §38.6.4Q.
    - **EVM coins and ERC20 tokens.** Affected: every such coin whose configured node list names two or more hosts:
      its swap steps wait until two of its nodes that passed the activation probe agree, and while fewer than two
      passed it starts no swap: `setprice`, `buy`, `sell` and the swap start refuse it. A coin configured with one
      host, for example a coin activated with a single node URL, trusts that node by default; with
      `swap_evidence_allow_single_host` set to `false` it is refused in the same way. A legacy ERC20 `enable` counts
      its own node list, and a token enabled with its platform counts the platform's. To fix it, add a node on a
      second host to the activation request, or set `swap_evidence_node` to one of the coin's nodes to trust that
      node alone. Detail: PR #512; CRD ch.51 §51.6G R100.
    - **Sia (SC).** A Sia coin reads one walletd node, so there is no second node to add: by default
      (`swap_evidence_allow_single_host`) the walletd node is trusted as if `swap_evidence_node` named it. With that
      switch set to `false`, set `swap_evidence_node` to the walletd `server_url`; without the field, SC orders are
      refused and a running SC swap waits at its next chain read and logs this fix. Detail: PR #514; CRD ch.20 §20.7.
    - **ZCoin and ARRR (Light mode).** The coin's nodes are its `light_wallet_d_servers`, counted by host: list
      servers on two hosts, or set `swap_evidence_node` to one of them; a coin configured with one host trusts it by
      default (`swap_evidence_allow_single_host`). A claim that reveals the swap's secret is still taken from one
      server. Detail: PR #515; CRD ch.39 (the status note at the end of 39.11.4).
    - QRC20 tokens over Electrum. The contract state of a payment, its receipt, the claim or refund found in the
      contract-event history and the chain time of the refund are read from two servers on distinct hosts. A token
      also needs two hosts that serve the contract-event history (`blockchain.contract.event.get_history`), or, with
      `swap_evidence_node` or a QTUM platform configured with one host (trusted by default through
      `swap_evidence_allow_single_host`), needs that server to serve it: each host is asked once after activation,
      and until enough have answered, the token is refused for swaps with a reason that names the hosts. When checked
      on 2026-11-23, both QTUM servers of the published coins file answered that request with an error, so with them
      alone a QRC20 token starts no swap: list servers on two distinct hosts that serve it, or set
      `swap_evidence_node` to a listed server that serves it. Detail: PR #524; CRD ch.51 §51.6G, ch.38.
    - **SLP tokens.** Affected: every SLP token whose BCH activation request lists two or more bchd servers on
      different hosts: the check of the other side's token payment waits until two bchd hosts agree, and the swap's
      other chain reads follow BCH. A token whose request lists exactly one bchd host trusts that host by default
      (`swap_evidence_allow_single_host`); with the switch set to `false`, a token with fewer than two bchd hosts no
      longer starts a swap, and its running swaps wait at a refund or claim step. To fix it, add a bchd server on a
      second host; `swap_evidence_node` does not replace the second bchd host. Detail: PR #526; CRD ch.51 §51.6G
      R100.
  - Detail: PR #508; CRD ch.51 §51.6G R100.
- **A swap database written by a newer version is refused at start-up.** When the migration state recorded in
  `MM2.db` is above the highest state this build knows (15), start-up stops with an error that names both numbers,
  instead of running on a database layout it does not know. No released version of KDF Reloaded or GLEEC KDF records
  a state above 15, so this matters only when a node goes back from a later version, or from an unreleased
  development build, that migrated the database further.
  - *What to do:* start the version that migrated the database, or move `MM2.db` aside (keep it) to start with a new
    one. The legacy swaps and their statistics are imported again from their saved files; data kept only in the
    database, namely swaps of the newer swap protocol, the order history and the node statistics, stays in the file
    you moved aside.
  - Detail: PR #486; CRD ch.44 R44.2.3 and §44.9A R44.9A.5.
- **The shielded (ARRR, Pirate) wallet database is migrated irreversibly on the first start of this version, and an
  older build cannot open the migrated database.** The Zcash libraries were upgraded (see "Changed"), and the new
  wallet library changes the layout of the wallet database. On the first activation of a shielded coin the node
  writes a consistent, checked backup named `<TICKER>_RELOADED_WALLET.db.pre-upgrade.<ms>.bak` next to the wallet
  database, migrates a copy of it, and replaces the original only when the result checks out. If the migration
  fails, the original is set aside as `.upgrade-failed.<ms>.bak` and the wallet is scanned again from the chain.
  - *Upgrade risk:* after the migration an older KDF Reloaded build cannot read the wallet database. The node
    needs free disk space for two copies of the database plus 16 MiB and refuses to activate the coin, without
    changing anything, when it is short of it.
  - *Going back to an older build:* stop the node, copy the `.pre-upgrade.<ms>.bak` file over
    `<TICKER>_RELOADED_WALLET.db`, delete `<TICKER>_RELOADED_WALLET.db-wal` and `<TICKER>_RELOADED_WALLET.db-shm` if
    they exist, and start the older build. Anything the wallet recorded after the upgrade is not in the backup; the
    older build scans it again. A wallet of another key (`<TICKER>_RELOADED_WALLET.<fingerprint>.db`, made by a
    development build) has its own backup named after that file and is restored the same way. Keep the backup until
    you are sure you stay on this version
    (`shielded_db_upgrade_backups` in `MM2.json` sets how many backups the node keeps; the default is 1).
  - *Before you upgrade:* make your own copy of the wallet database file and of the seed or passphrase as for any
    upgrade; the steps under "Coins that are no longer visible after an upgrade" below apply if a coin is no longer
    shown afterwards.
  - Detail: PR #411; CRD ch.39 R39.8.0ba, R39.8.0bc and the status note of 2026-11-18; RELOADED_VS_GLEEC.md.
- **Under an HD login, `priv_key_policy: IguanaPrivKey` now selects the node's single internal key for Tendermint
  coins, EVM coins and their tokens, BCH with its SLP tokens, Sia, Lightning and shielded coins such as ARRR.** Until
  now these families ignored the value: Tendermint and EVM coins used their HD key at `path_to_address`, Sia its fixed
  single-address HD key, BCH with tokens and Lightning the key derived from the passphrase, and shielded coins the key
  the HD login derives (at `z_derivation_path` and `account`). A request that asks for `IguanaPrivKey` now gets a
  different address or Lightning identity.
  - *Upgrade risk:* if your activation requests already contain `IguanaPrivKey`, the address or Lightning identity of
    those coins changes after this upgrade, although you changed nothing. Follow the steps under "Coins that are no
    longer visible after an upgrade" below.
  - *Unchanged:* an absent value, `ContextPrivKey`, an iguana login, and the UTXO family, which already worked this way.
  - *Lightning:* under an HD login, `IguanaPrivKey` starts a separate node identity with its own data. The channels of
    the other identity are not shown. Activation is refused while the other identity still has open channels: the
    error names that identity; close its channels first under the policy it names. Channels opened before this upgrade
    belong to the default identity (no `priv_key_policy` or `ContextPrivKey`), even if your requests already said
    `IguanaPrivKey`.
  - *Sia and Lightning:* they now read `priv_key_policy`. A hardware value or an unknown value fails the request,
    where it used to be ignored.
  - *Shielded coins (ARRR):* under an HD login, `IguanaPrivKey` now activates the coin with the internal key, for the
    shielded address and the transparent key, and with a wallet database of its own; the HD wallet stays in its own
    file with its scan state, and the two never mix. If your ARRR activation requests already contain `IguanaPrivKey`,
    the address changes after this upgrade and the balance of the HD wallet is not shown. To keep the HD wallet, leave
    `priv_key_policy` out or send `ContextPrivKey`. If coins are not visible after the upgrade, read "Coins that are
    no longer visible after an upgrade" below and start the previous version again before you move or spend
    anything. A hardware value fails the request, where it used to be ignored.
  - Detail: PRs #400, #414; CRD ch.05 R46 and its status notes of 2026-11-17 and 2026-11-18, ch.39 R39.8.0bd.
- **ZEC sends need the coins entry with `"txversion": 5` and `"consensus_branch_id": "77190ad9"`** (the branch id is
  accepted with or without `0x`). Since the Zcash network upgrade NU7 the network refuses version-4 transactions. This
  release builds version-5 ZEC transactions when the ZEC entry says `txversion` 5; with the old entry (`txversion` 4) every
  ZEC send is still refused by the network. With either entry, a lab
  test of 0.4.0-beta.1 and of the development build before this change activated ZEC and showed its balance, and the
  network refused the ZEC withdrawal it sent (version-4 entry: "not supported by the network upgrade Nu7"; version-5
  entry: an invalid consensus branch id).
  - *Activation:* a Zcash-family (`overwintered`) entry whose `txversion` is not 4 or 5, and a version-5 entry without
    `consensus_branch_id`, are now refused with an error that names the field.
  - Detail: PR #404; CRD ch.39 R39.11.15 and the §39.11.4V status notes.
- **WalletConnect sessions paired with an earlier version must be paired again.** The node now keeps one relay
  identity across reconnects and restarts. Sessions made by an earlier version were made under a different identity,
  so the relay refuses requests on them after the upgrade: remove them in the wallet and pair again.
  - *Browser (WASM) builds:* the WalletConnect store in the browser is upgraded to a new format at the first start.
    An earlier version can no longer open it. To go back to an earlier version, clear this site's WalletConnect data
    in the browser first, then pair again.
  - Detail: PR #393; CRD ch.22 §22.5.2.
- **`disable_coin` of a platform coin that has active tokens no longer removes it.** The legacy `disable_coin`
  now makes such a platform coin passive and answers `passivized: true`: its tokens stay active, and every
  method that needs an active coin answers as if the platform were not activated. Before, the call removed the
  platform and left its tokens registered. A script or wallet that disabled a platform to remove it, then
  activated it again, now finds it still registered, and its `enable` or `electrum` is refused as already
  activated.
  - *What to do:* to remove the platform and its tokens, send `"force_disable": true`.
  - *Versioned form:* the `mmrpc: "2.0"` `disable_coin` does not change. It neither makes a platform passive nor
    reads `force_disable`: it removes the platform, and its tokens stay registered and active, but the platform no
    longer collects their history; it does so again once the platform is activated again with `tx_history` and the
    token named in the request.
  - *Activating it again:* `enable_eth_with_tokens` and the other platform-with-tokens methods activate a passive
    platform again (see "Added" below); the legacy `enable` and `electrum` still refuse it as already activated.
    Disabling a coin now ends its background work (see "Changed" below).
  - Detail: PR #424; CRD ch.43 §43.1E and §43.1F.
- **A platform-with-tokens activation that names a token registered for another platform is now refused.**
  Before, `enable_eth_with_tokens`, `enable_bch_with_tokens`, `enable_tendermint_with_assets`,
  `experimental::enable_solana_with_assets` and the task forms replaced a registered token of the same ticker by a new
  one built from the request. Now a token that is registered for this same platform is kept as it is and joins the
  platform again, for example after the versioned `disable_coin` of the platform or after a token was activated by
  the legacy `enable`; the request's settings for it are not applied. A token registered for another platform is
  refused with `TokenIsAlreadyActivated` (HTTP 400) before anything is built.
  - *What to do:* disable the token first, leave it out of the request, or set `reactivation_registered_token` to
    `"keep"` in `MM2.json` (see "Added" below).
  - Detail: PR #432; CRD ch.43 §43.1F R43.1F.4.
- **RPC answers no longer have `<`, `>` and `&` replaced by `&lt;`, `&gt;` and `&amp;`.** Answers are sent as the
  handler produced them. A client that decoded those entities itself to get the original text must stop doing so,
  or it will now change text that really contains such a sequence.
  - *What you see:* URLs with query strings, memos, error texts and labels in answers arrive with their plain
    characters.
  - Detail: PR #421; CRD ch.55 status note of 2026-11-18.
- **Transaction history now drops transactions that are no longer on the chain, and shows a moved transaction at its
  new height.** This applies to UTXO coins, Qtum, QRC20 tokens, BCH and SLP. After the upgrade, a stored history row
  whose transaction the coin's servers no longer list disappears, once two server listings at least one block apart
  confirm its absence. This can happen to a transaction of a chain branch that was later abandoned. A transaction the
  servers now report at another height shows that height and its block's time.
  - *What you see:* an existing history can lose rows or show changed heights. Nothing needs to be done.
  - Detail: PR #408; CRD ch.38 §38.5C, D38.5C.1 and D38.5C.2.
- **A Kerrigan (KRGN) coin entry needs `"chain_variant": "KRGN"` in its `protocol` object.** Without it, the
  coin's Electrum headers cannot be read, so its median block time is almost always unknown, and claims and refunds
  of a KRGN swap fail, as they did before this release. With the value, the headers are read and claims and refunds
  follow the same lock-time rule as those of any UTXO coin. A KRGN entry with
  `"chain_variant": "KRGN"` and `enable_spv_proof` or `spv_conf` is refused at activation; remove those keys.
  - *What you see:* a KRGN entry without the value still activates, but its claims and refunds keep failing.
  - Detail: PR #468; CRD ch.37 status note of 2026-11-18.
- **`account_balance` now answers `page_balance` as an object keyed by ticker, for every HD coin, and the activation
  report of an EVM platform lists `{}` for the addresses its scan passed over.** Until now `page_balance` was one
  balance, `{"spendable": "...", "unspendable": "..."}`. It is now an object with one balance per ticker, the same
  type as the `balance` of one address: `{"RIN": {"spendable": "...", "unspendable": "..."}}`. Each ticker is summed
  over the addresses of the returned page only, not of the whole account.
  - *What you see:* a UTXO, QTUM or Sia coin answers its own ticker. An EVM platform ticker answers the platform and
    every ERC-20 token activated on it, a token with a zero balance included; a TRX ticker answers TRX and its
    enabled TRC20 tokens; an ERC-20 or TRC20 ticker answers that token only. A page that lists no address (a page
    past the known range) answers a map with the requested ticker and a zero balance. GLEEC KDF answers `{}` there;
    the maintainer decided on 2026-11-22 to keep the entry for the requested ticker, as the `total_balance` of an
    account without a known address already does.
  - *What to do:* read `page_balance` as an object and take a ticker's balance from `page_balance["<TICKER>"]`; the
    members `spendable` and `unspendable` no longer sit directly under `page_balance`.
  - *Activation report of an EVM platform:* the unused addresses that the scan at activation passes over on its way
    to a used address (not on a TRON platform) carry an empty balance map, `{}`, where they used to carry a zero
    entry for the platform ticker. A client that reads the platform ticker's balance of every listed address must
    accept an empty map. A later `account_balance` lists those addresses with the full map and zero values.
  - Detail: PR #478; CRD ch.49 R49.35, R49.36.
- **ARRR pays a fixed 0.0001 ARRR per transaction and trades at least 0.001 ARRR; saved ARRR orders follow at
  start-up.**
  - *What changes:* with no `txfee` in its coin entry, ARRR pays 0.0001 ARRR (10,000 base units) for every
    transaction, and its minimum trading volume is 0.001 ARRR in place of 0.0001 ARRR; `setprice`, `buy` and `sell`
    below it fail with `VolumeTooLow`. Once both coins of a saved order are active after start-up, and when a
    good-till-cancelled taker order becomes a maker order, an order with ARRR on either side whose minimum volume is
    below 0.001 ARRR keeps trading with its minimum raised to 0.001 ARRR; one whose whole volume is below it is
    cancelled. Each such order is named in one log line. An ARRR coin entry with `txfee: 0` or the ZIP 317 fee class
    is refused at activation.
  - *What to do:* nothing for the published ARRR entry. Raise any ARRR volume or minimum below 0.001 ARRR in your
    scripts (saved orders are adjusted at start-up as above), and give an ARRR coin entry with `txfee: 0` or the ZIP
    317 class a positive `txfee`, or remove it. A `txfee` of 1000 gives the earlier fee and a minimum of 0.0001 ARRR
    back.
  - Detail: PR #544; CRD ch.39 R39.6.5f (status note of 2026-11-26); the item under "Changed" / "Shielded coins".

#### Coins that are no longer visible after an upgrade

**If coins or Lightning channels you held are no longer visible after an upgrade, follow these steps
before you move or spend anything.** For some coin families a release can change the rule
that picks a coin's key or address, for example which key is used for a given `priv_key_policy` under
an HD login. The node then shows a different address for that coin, and the funds stay at the address
the previous version used. The safe way:
1. Note the address the new version shows for that coin.
2. Start the previous version again, with the same passphrase, `MM2.json` and activation requests.
3. Move the coins to the address you noted, or close the Lightning channels.
4. Upgrade again.

### Added

#### Start-up and configuration

- **The node checks the computer's clock against time servers.** At start, and again every hour
  and after the computer slept or its clock was set, the node asks NTP time servers for the time
  and takes the median of their answers. Swaps rely on the clock for their lock times and
  deadlines. A clock more than 10 seconds off stops the node at start with an error that says by
  how much the clock is off; swaps that were in progress then wait until the node starts with a
  correct clock, so setting the clock right is urgent. While the node runs, it refuses new orders
  and new swaps until the clock is right again; running swaps and withdrawals go on. By default
  the node asks `0.pool.ntp.org` to `3.pool.ntp.org`; `ntp_servers` in `MM2.json` lists other
  servers, and `[]` turns the check off. When no server answers, the node logs a warning and goes
  on, unless `ntp_required` is `true`. Browser builds cannot send the queries and skip the check.
  Detail: PR #348; CRD ch.45 §45.6C.
- **A setting that keeps a registered token named in a platform-with-tokens activation.** The `MM2.json` member
  `reactivation_registered_token` is `"refuse"` (default, also when absent or `null`) or `"keep"`. With `"keep"`, a token that is already
  registered for another platform and is named in `enable_eth_with_tokens` or another platform-with-tokens method is
  kept silently and joins the platform, as in GLEEC KDF; a token whose coin configuration names another platform is
  still refused with `PlatformCoinMismatch`. With `"refuse"` it is refused with `TokenIsAlreadyActivated`. Any other
  value stops the node at start with an error that names the member. Detail: PR #435; CRD ch.43 §43.1F D43.1F.1.

#### RPC interface

- **`task::get_new_address::init`, `::status`, `::user_action` and `::cancel` make the next address of an HD account
  as a task, by the coin's key policy.** The request is `coin`, `account_id`, an optional `chain` (default
  `External`) and an optional `gap_limit`. It is served for standard UTXO coins, QTUM, EVM coins and ERC-20 tokens,
  TRX and TRC20 tokens, and Siacoin; init answers only `NoSuchCoin` and `Internal`, and every other condition (also a
  coin kind it does not serve, `CoinIsActivatedNotWithHDWallet`) shows in the task's `Error` status. Before it
  derives an address the task runs the gap check: when the last `gap_limit` known addresses of the chain are all
  unused it ends with `EmptyAddressesLimitReached` and the `gap_limit` (the wallet's gap limit when the request names
  none). The direct `get_new_address` is unchanged and runs no gap check.
  - *Software keys:* the task derives, records and queries without any device state, also when a hardware wallet is
    connected beside a software login; its states are `Preparing` and `RequestingAccountBalance`, and it ends with
    the same `{"new_address": ...}` answer as `get_new_address`. A TRX or TRC20 ticker completes this way, and so
    does a Siacoin.
  - *Hardware wallet (Trezor) keys:* the task needs the connected device (`HwContextNotInitialized` when none is
    connected, `FoundUnexpectedDevice` for another device than the coin was activated with), asks the device to show
    the address, and reports `{"ConfirmAddress": {"expected_address": "<address>"}}` while the device waits for the
    button. The address is recorded only after the button was pressed. A refusal on the device ends the task with
    `HwError` `UserCancelled`. If the device shows another address than the one the node derived, the task ends with
    `DeviceAddressMismatch` and `{expected_address, device_address}` and records nothing; the comparison you make on
    the device's screen is the check that matters.
  - *Differences from GLEEC KDF:* GLEEC KDF runs this task on the device path only, so with software keys it ends
    with `HwContextNotInitialized`; and it ends a device mismatch with `Internal`, accepts the `Internal` chain for
    EVM coins, and has a scripthash-subscription step that has no counterpart here. The `Internal` chain is refused
    for EVM and TRON coins with `InvalidBip44Chain`; UTXO and QTUM accept it.
  - *No rollback:* an address whose balance query fails stays known, as with `get_new_address`; cancelling the task
    during the balance query does the same, and cancelling it before the device confirmed records nothing.
  Detail: PR #491; CRD ch.49 §49.2.3 (the status note "the task form of `get_new_address` (as built)"), ch.05 R54;
  the maintainer decided on 2026-11-22 on the divergences.
- **New optional activation field `swap_evidence_node`.** It names one node of the coin's own list as the node whose
  answer alone a swap acts on (see the breaking change about two nodes above). Every coin family that can swap
  accepts it: UTXO, BCH and SLP, QTUM and QRC20, EVM and ERC20, ZHTLC, Sia and Tendermint, in the legacy and in the
  task-based activation methods. A value that names no entry of the coin's list is refused with the `error_type`
  `SwapEvidenceNodeNotInList`, whose data and message name the ticker and the node as host and port only. Detail: PR
  #508; CRD ch.51 §51.6G R100 item 2.
- **The legacy `disable_coin` makes a platform coin with active tokens passive, answers `passivized`, and takes
  `force_disable`.** A platform coin (an EVM, BCH, Tendermint, Solana or standard UTXO coin) that has at least one
  active token or layer-two coin, activated by a token activation method or as part of a platform-with-tokens
  activation, is not removed by `disable_coin`. The call cancels the platform's own orders, keeps the platform
  registered but passive, and answers `{"coin", "cancelled_orders", "passivized": true}`. A passive platform is
  listed by `get_enabled_coins`, its tokens stay active (a TRC20 or ERC-20 token's `withdraw` and HD methods
  included, and a TRC20 token's GasFree address), and its history collection goes on, TRON's included; every other
  method answers as if it were not activated, and a second `disable_coin` for it is refused like one for an unknown
  coin.
  With `"force_disable": true` the call removes the platform and all its tokens and answers `passivized: false`;
  unlike in GLEEC KDF it first checks the platform and every token, and refuses the whole call if any of them has a
  running swap or an order that is being matched. Every other successful call now also answers `passivized: false`.
  Only the JSON value `true` forces. A node restart makes every coin inactive, as before. Detail: PR #424; CRD ch.43 §43.1E
  and §43.1F.
- **A passive platform coin is activated again by the platform-with-tokens methods.** `enable_eth_with_tokens`,
  `enable_bch_with_tokens`, `enable_tendermint_with_assets`, `experimental::enable_solana_with_assets` and the task
  forms `task::enable_eth::init` and `task::enable_tendermint::init`, given the ticker of a passive platform of their
  own family, make it active again with the instance it already has. The request's nodes, signing policy, swap-contract
  settings, required confirmations and `tx_history` are not applied, and no history collection is started (a
  platform first activated without `tx_history` gets none until it is removed and activated again);
  `get_balances` and, for an HD wallet, the address-scan settings and `path_to_address` shape the answer. The requested
  tokens are activated, and the answer lists every token the platform was activated with and not disabled since, plus
  the request's tokens. An active platform, and a passive platform of another family, still answer
  `PlatformIsAlreadyActivated`. A refused request leaves the platform passive and its token list as it was. Detail: PR
  #432; CRD ch.43 §43.1F R43.1F.2 to R43.1F.5.
- **An EVM platform's NFT chain counts as its dependent.** An NFT chain activated by `enable_nft`, or inline by
  `nft_req`, now keeps its platform from being removed by an unforced `disable_coin`: a platform whose only dependent
  is its NFT chain becomes passive, and a forced `disable_coin` deactivates the NFT chain with the platform's tokens. A
  re-activation without `nft_req` keeps the NFT chain. Detail: PR #432; CRD ch.43 R43.1F.8.
- **The order-management methods `setprice`, `update_maker_order`, `cancel_order`, `cancel_all_orders`,
  `my_orders` and `order_status` accept `mmrpc: "2.0"` requests.** A versioned request carries the legacy
  members unchanged inside `params`, gets the legacy result under `result` with the request's `id` echoed,
  and a typed `error_type` with an HTTP status instead of the legacy error string: a request the client
  got wrong (unknown coin, unknown order, a volume or price below the minimum, not enough balance) answers
  400, a failure of the node 500, and a refusal while the computer's clock is off 503. An unknown member
  inside `params` is ignored. The legacy forms of the six methods and their answers are unchanged, except
  that a legacy error text no longer carries the inner `file:line]` trace tokens. This is a KDF Reloaded extension; the reference answers
  `NoSuchMethod` for these names under `mmrpc: "2.0"`. The remaining legacy-only methods follow in later
  releases (plan `docs/plans/mmrpc-v2-completion.md`). Detail: PR #359; CRD ch.43 §43.1R, D43.1A.1 status note.
- **The trading methods `buy`, `sell`, `orderbook_depth` and `orders_history_by_filter` accept `mmrpc: "2.0"`
  requests**, in the same way as the order-management methods above: legacy members inside `params`, legacy
  result under `result` with `id` echoed, typed `error_type` with an HTTP status (400 for a request the client
  got wrong, 500 for a node failure, 503 while the computer's clock is off), unknown `params` members ignored,
  legacy forms and success bodies unchanged, legacy error texts as for the order-management methods (no inner
  trace tokens). `get_trade_fee` stays legacy-only: the reference documentation marks it
  deprecated in favor of the versioned `trade_preimage`. Detail: PR #372; CRD ch.43 §43.1R.
- **The wallet and coin methods `my_balance`, `disable_coin`, `send_raw_transaction`, `validateaddress`,
  `convertaddress`, `convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`,
  `set_required_confirmations` and `set_requires_notarization` accept `mmrpc: "2.0"` requests**, in the same
  way as the order and trading methods above. Their typed errors follow the versioned `withdraw` as the reference
  answers it: an unknown coin answers `NoSuchCoin` with HTTP 404, a node or network failure `Transport` with 502, a request the client
  got wrong 400. `disable_coin` names the swaps or orders that block the deactivation inside `error_data`.
  Legacy forms and success bodies unchanged, legacy error texts as above (no inner trace tokens). Detail:
  PR #379; CRD ch.43 §43.1R.
- **The remaining legacy-only methods accept `mmrpc: "2.0"` requests**: `all_swaps_uuids_by_filter`, `import_swaps`,
  `version`, `stop`, `metrics`, `get_my_peer_id`, `get_directly_connected_peers`, `get_gossip_mesh`,
  `get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh`, `ban_pubkey`, `list_banned_pubkeys` and
  `unban_pubkeys`, in the same way as the methods above. With this, every method that only the legacy surface knew
  has a versioned form, except `recover_funds_of_swap` (waits for the swap architecture decisions),
  `stats_swap_status`, `get_trade_fee`, `electrum`, `enable`, `help` and `show_priv_key`, which stay legacy-only on
  purpose, and the test-only `sim_panic`. Detail: PR #387; CRD ch.43 §43.1R.

- **`buy` and `sell` that name only orders the node cannot match fail with a typed error.** From
  0.4.1 the order book leaves out an order whose maker lacks a capability the node requires (ARRR
  version 6, BC2 suffix signing), and a `buy` or `sell` whose `match_by` names only such orders is
  refused at once (see the 0.4.1-beta.1 entry below). On the versioned surface the refusal now has
  the error type `OrderNotMatchableMissingCapability`, HTTP 400, with the members `coin`,
  `capability` and `orders`; the legacy surface answers the same text as in 0.4.1. Detail: PR
  #448; CRD ch.59 R59.4C.6.

#### Shielded coins

- **ARRR and the other shielded coins accept `priv_key_policy`, and each key gets a wallet of its own.**
  Under an HD login, `IguanaPrivKey` activates the shielded coin with the node's single internal key; leaving
  the member out keeps the HD key. The shielded balance and history are per key and per `account`: switching
  the policy or the `account` opens the other wallet instead of failing, and switching back reopens the first
  one with its scan state, without a rescan. The existing wallet file keeps the name it has and keeps the key
  it already holds; another key's wallet is stored in files whose names carry a short fingerprint of that key.
  Amounts a swap step pays or receives appear in the account that was active when that step ran; activate that
  account to see them. The coin is refused with a message that names the swaps and says what to do, in two
  cases: when swaps of that coin that are not finished were started with another key (activate with the key they
  were started with, let them finish or refund, then switch), and when a wallet database of the coin cannot be
  read while a swap still has to send its payment or fee. Changing `account` alone is allowed.
  Detail: PR #414; CRD ch.39 R39.8.0bd, T39.8.0k, ch.05 status note of 2026-11-18.

#### Solana coins

- **Solana is back, as a wallet-only coin family: SOL and SPL tokens can be activated and used as a
  wallet, and cannot be traded.** Earlier releases refused every Solana activation. Now a Solana
  platform and its SPL tokens activate through `enable_solana_with_tokens` and `enable_spl`, and
  through the methods GLEEC KDF uses, `experimental::enable_solana_with_assets` and
  `experimental::enable_solana_token`, in native and browser (WASM) builds, from coins-file entries in
  either form. A Solana wallet covers balances, an HD wallet under a global-HD login, withdrawals of
  SOL and SPL tokens, message signing and verification, `show_priv_key` and `disable_coin`; the items
  below describe each. A Token-2022 token is still refused at activation. The legacy `enable` and
  `electrum` methods do not activate Solana: they answer that the coin needs
  `enable_solana_with_tokens` or `enable_spl`, as GLEEC KDF also refuses Solana there. *Orders and
  swaps stay off:* `setprice`, `buy`, `sell` and `trade_preimage` refuse a pair with a Solana coin or
  SPL token with "Trading is not supported for coin …", and every swap operation of such a coin
  answers an error that names it. The transaction history is the next item. Detail: PR #475; CRD
  ch.40 D40.4.

- **Solana activation reads a swap program and an evidence node.** A `SOLANA` coins-file entry may name
  the cluster's swap program in `protocol.protocol_data.swap_program_id` (base58). The activation reads that
  program once: it must exist, be executable and have no upgrade authority, or the coin stays wallet-only.
  A value that is not a base58 public key makes the entry unparsable (`CoinProtocolParseError`).
  `enable_solana_with_tokens` and `experimental::enable_solana_with_assets` take an optional
  `swap_evidence_node`, one URL of the coin's node list, which a swap uses as its one trusted node for the
  reads before a refund or a secret step; a URL that is not in the list is refused. Solana and SPL tokens
  still cannot be traded: the SOL side of the swap is built, and SPL and the switch-on follow after 0.5.0. Detail: PR #503;
  CRD ch.40 §40.6D.

- **Solana and SPL tokens have a transaction history.** With `tx_history: true` at the platform's
  activation (`enable_solana_with_tokens` or `experimental::enable_solana_with_assets`), the node
  reads the whole history of the wallet's address and of its token accounts from the coin's Solana
  nodes, at most four requests per second, then checks for new transactions every minute. Both
  forms of `my_tx_history` serve it, for the platform and for each SPL token it serves, in native
  and browser builds (the versioned form is not served in browser builds, as for every coin).
  Only finalized transactions are listed. A SOL row's `my_balance_change` is the exact change of
  the address's balance, fee and rent deposits included; the rent is in `fee_details.rent_amount`.
  A failed transaction whose fee was charged is a row with that fee only. Without `tx_history`
  nothing changes: `my_tx_history` answers as before. A platform made passive by the legacy
  `disable_coin` keeps the history setting it had when it is activated again; to turn the history
  on, remove it with `"force_disable": true` and activate it again. GLEEC KDF has no Solana
  history. Detail: PR #494; CRD ch.40 §40.6C.

- **Solana and SPL tokens in browser builds.** Browser (WASM) builds now read Solana coins-file
  entries in both forms and route `enable_solana_with_tokens`, `enable_spl` and the two
  `experimental::` methods as native builds do, with the same node pool. A Solana coin
  cannot be traded in browser builds either. Detail: PR #430; CRD ch.40 R40.1.1, T40.1.

- **The Solana activation methods of GLEEC KDF are routed.** `experimental::enable_solana_with_assets`
  activates a Solana platform with its tokens and `experimental::enable_solana_token` a single SPL
  token, with the requests, results and errors GLEEC KDF uses, in native builds. They share the
  activation of `enable_solana_with_tokens` and `enable_spl`: a platform activated through either
  method takes tokens through either token method, and `disable_coin` treats both the same. A token
  can also be activated from a `protocol` given in the request; such a token is wallet-only. A coin
  activated through these methods sends no commitment level, so the node's default applies.
  `enable_solana_with_tokens` now also refuses an inline token whose platform is another coin
  (`PlatformCoinMismatch`). Detail: PR #426; CRD ch.40 §40.2, §40.3, R40.3A.3.

- **Solana activates under an HD login, at a path the request chooses.** Under an HD login the
  Solana key is now derived from the seed phrase at `m/44'/501'/N'/0'` by default, or at
  `m/44'/501'/N'` when the activation request sends `"hd_path_layout": "bip44"`; N comes from
  `path_to_address.account_id`. Nothing is stored: a wallet that used `"bip44"` must send it on
  every activation, or it gets the default address. One seed phrase therefore gives three Solana
  addresses: as a single-key passphrase, with the default layout, and with `"bip44"`. A request
  that asks for a hardware wallet is refused and the WalletConnect form is an invalid request; a
  request that asks for `IguanaPrivKey` under an HD login gets the wallet's single internal key and
  its address, not the address of a single-key login with the same words. Detail: PR #392; CRD
  ch.40 §40.1A.

- **Solana entries of a coins file in the form GLEEC KDF uses can be read.** Besides its own
  forms, KDF Reloaded now reads, in native builds, a Solana platform entry with
  `"protocol_data": {}` and an SPL token entry of type `SOLANATOKEN` with `platform`, `decimals`, `mint_address` and `program_id`. A
  `mint_address` or `program_id` that is not a valid public key makes the entry unreadable, and the
  activation reports a protocol parse error. Detail: PR #361; CRD ch.40 R40.1.2, R40.1.3.

#### TRON coins

- **TRON is back, as a wallet-only coin family: TRX and TRC20 tokens can be activated and used as a wallet, and
  cannot be traded.** 0.4.0 refused every TRON activation. From this version a TRX coin and its TRC20 tokens
  activate through `enable_eth_with_tokens`, the `task::enable_eth::*` methods, `enable_erc20` and the
  `task::enable_erc20::*` methods, the way GLEEC KDF serves TRON, and through the legacy `enable` and `electrum`
  methods, which is a Reloaded extension. A TRON wallet covers balances and addresses, an HD wallet under a
  global-HD login, withdrawals with their real fee (TRC20 amounts of any size; a transfer the token contract
  refuses is not signed), and a transaction history when a TronGrid API key is set; the items below describe each.
  *Swaps stay off.* Reloaded has no TRON swap, so every TRX and TRC20 coin counts as wallet-only, whatever its
  coins-file entry says: `setprice`, `buy`, `sell` and `start_swap` refuse it with the usual "… is wallet only"
  answer, as GLEEC KDF does for a coin configured `"wallet_only": true` (every published TRON entry is), a TRON coin
  declines to match, an order with a TRON coin saved earlier is kept at startup but not announced or matched, and an
  unfinished saved swap that names a TRON coin is not resumed at startup (its record is kept). For a TRON entry that
  does not say `"wallet_only": true`, GLEEC KDF refuses the order at a later check; the answer has the same form, so
  no setting exists for this. Gasless TRC20 transfers through GasFree work through the task form of `withdraw`: a
  GasFree provider can be configured, the GasFree address is shown, the task form submits a gasless transfer to the
  provider and follows it until it settles, and the direct `withdraw` returns a signed authorization (items below).
  Detail: PR #451; CRD
  ch.21 D6, R-D1, R21.10B.9, R21.B3 and their status notes of 2026-11-20; ch.55 D55.5.
- **The TRON node client gives each node a time limit and fails over when a node answers badly.** A node
  that does not answer within 10 seconds is skipped, and so is a node that answers with an error status
  (a rate limit included), with something that is not the expected JSON, or with the node's own error form;
  such an answer is never read as an empty account. The node that answers is asked first by the next
  request, and several requests of one coin can run at the same time instead of waiting for each other.
  A node that answers "busy" or "not in sync" (`SERVER_BUSY`, `NO_CONNECTION`, `NOT_ENOUGH_EFFECTIVE_CONNECTION`,
  `BLOCK_UNSOLIDIFIED`) is skipped too, and the node list is shuffled once at activation and accepts only `http`
  and `https` URLs. A signed transaction that a node rejects with any other code ends the request with that code. An entry of the
  TRON `urls` list can now be an object `{"url": "...", "headers": {...}}` whose headers go to that node
  only, and the new `MM2.json` key `tron_api_key` is sent as the `TRON-PRO-API-KEY` header to TronGrid
  hosts over HTTPS only. Header values and the key are never logged or shown. Detail: PR #394; CRD ch.21 §21.9 R-N3
  to R-N9.
- **TRON has an activation route on the standard platform-with-tokens methods.** `enable_eth_with_tokens`,
  `task::enable_eth::init` and its status, user-action and cancel calls accept a TRX platform with its TRC20
  tokens, and `enable_erc20` and `task::enable_erc20::init` accept a TRC20 token, the way GLEEC KDF serves TRON.
  The node list is read from `nodes` (a `url`, an optional `komodo_proxy` that is read but does not sign
  requests, and an optional `headers` object); an empty list is refused with `AtLeastOneNodeRequired` and an
  unparsable URL or a scheme other than `http` and `https` with `InvalidPayload`, before any node is asked.
  The result lists the balances at the base58check address, with the head block from the TRON node. A TRC20 token
  whose config has no `decimals` reads them from the contract, on both routes. The Trezor, WalletConnect and MetaMask
  policies are refused at activation.
  The legacy `enable` and `electrum` methods keep activating TRON as an extension. A key in the query string of a
  TRON node URL now works: `https://h/x?key=1` is asked as `https://h/x/wallet/getaccount?key=1`.
  `task::withdraw` takes TRX and TRC20 coins. Detail: PR #405; CRD ch.21 §21.4, R21.10B.2, R21.10B.4, R21.10B.5;
  ch.35 R35.1.1; ch.55 D55.5; ch.49 R49.9.
- **TRON has an HD wallet under a global-HD login.** `enable_eth_with_tokens` and `task::enable_eth::init` derive the
  TRX address under the `derivation_path` of the coin config (coin type 195, `m/44'/195'`) with `path_to_address`,
  `gap_limit`, `scan_policy` and `min_addresses_number`, the way every EVM platform does, for the default policy and for
  `ContextPrivKey`; a config without `derivation_path` is refused with `InvalidPayload`. `IguanaPrivKey` keeps the node's
  single internal key. The result lists the accounts with base58check addresses, their derivation paths and the balances
  of TRX and the enabled TRC20 tokens. `get_new_address`, `account_balance`, `task::scan_for_new_addresses::init` and
  `task::account_balance::init` work for TRX and TRC20 tickers. Asked for TRX,
  each balance map covers TRX and the enabled TRC20 tokens; asked for a TRC20 ticker, that token only, as for an ERC-20
  ticker. For either ticker an address counts as used when the chain has created its account or an enabled TRC20 token
  has a balance there. For a TRC20 ticker GLEEC KDF looks at the account only; receiving a TRC20 token does not create
  the account, so Reloaded also looks at the token balances and finds an address that holds only a token.
  `task::create_new_account::init` fails with `Internal` for every TRON coin. `withdraw` and `task::withdraw` take an HD
  `from` for TRX and TRC20 and sign with the key of that path, and `show_priv_key` answers the key of the activated
  address. A TRC20 withdraw may name any activated address of the platform's HD wallet in `from`, as an EVM ERC-20
  withdraw also does since PR #438. A TRON coin on a single key now refuses `from` with `UnexpectedFromAddress`, as an EVM coin
  does; it was ignored before. The legacy `enable` method keeps the node's internal key under a global-HD login, as the
  EVM and UTXO legacy routes do in Reloaded, so its TRON address differs from the one the mmrpc 2.0 methods give for the
  same seed.
  Detail: PR #428; CRD ch.21 R-A2, R21.10B.4, R21.10B.5, R21.10B.8, T21.10B.8.
- **`convertaddress` for a TRON coin converts between the base58check and the `41...` hex form.** Its
  `to_address_format` is `{"format": "base58"}` or `{"format": "hex"}`, `from` may also be `0x41...`, and an EVM `0x`
  address or an EVM format value is refused; GLEEC KDF answers the EVM `0x` form here. `validateaddress` and
  `get_my_address` already answered as the CRD states. Detail: PR #412; CRD ch.21 R21.10B.8, D21.10B.2.
- **A TRON withdrawal works out its real fee, takes it into account in the amount, and fails instead of guessing.**
  The fee of a TRX or TRC20 withdrawal is now estimated from the real size of the transaction, the sender's free and
  staked bandwidth and energy, and the chain's current prices. Bandwidth follows the TRON node's own rule: a
  transaction is paid from the staked bandwidth as a whole, or from the free bandwidth as a whole, or all of it is
  paid in TRX; the two are never added up, so the estimate is never below what the node charges. Sending TRX to an
  address the chain has not seen adds the account-creation fee; a TRC20 transfer never does. `max: true` for TRX now
  sends the balance less the fee (it used to send the whole balance, which the network refused), a zero balance
  answers `ZeroBalanceToWithdrawMax`, a balance that does not exceed the fee answers `AmountTooLow`, and a TRX amount
  plus fee above the balance answers `NotSufficientBalance` with the sum. A TRC20 withdrawal whose sender does not
  hold enough TRX for the fee answers `NotSufficientPlatformBalanceForFee`. If the node cannot be asked for the
  resources, the prices or the energy of a token transfer, the withdrawal fails with `Transport`; it no longer shows
  made-up figures or leaves the fee out.
  The fee limit of a TRC20 transfer is the cost of the energy the transfer needs plus 20%, rounded up, and at most
  100 TRX; it was a fixed 100 TRX. The fee limit is the most the token transfer's contract call can cost. A transfer
  whose energy alone would cost more than 100 TRX is refused before anything is signed, with both figures in the
  error.
  The members of the TRON fee details changed to the documented ones: `coin`, `bandwidth_used`, `energy_used`,
  `bandwidth_fee`, `energy_fee`, `total_fee` and, when an account is created, `account_creation_fee`, with the fees as
  decimal amounts in TRX, as GLEEC KDF reports them. The earlier members in SUN (`total_fee_sun`, `bandwidth_fee_sun`,
  `energy_fee_sun`, `activation_fee_sun`) are gone. `total_fee` is the expected charge. `spent_by_me` includes the fee
  for TRX. A `memo` is refused with `UnsupportedError`, and the new optional request member `expiration_seconds` sets
  how long the transaction stays valid (60 seconds by default); other coins ignore it. An HD `from` is honored
  throughout: every balance and resource is read for that address.
  Detail: PR #437; CRD ch.21 R21.10B.7, D21.10B.1 and their status notes of 2026-11-19; RELOADED_VS_GLEEC.md.
- **A TRC20 withdrawal can send any amount the token balance holds.** The amount of a TRC20 transfer is now encoded
  as a 256-bit value, so a token's whole balance can be sent whatever its decimals; a token of 18 decimals could send
  at most about 18.4 whole tokens per withdrawal, and a larger amount answered `InternalError`. Transfers of smaller
  amounts are built exactly as before. Detail: PR #442; CRD ch.21, the status note of 2026-11-20 after R21.10B.7.
- **A TRC20 withdrawal the token contract refuses is no longer signed.** Before a TRC20 transfer is built, the TRON
  node runs it as a trial to learn its energy. When that trial shows that the token contract would refuse the
  transfer (a frozen or blacklisted address, a paused token, a balance that changed in the meantime), the withdrawal
  now fails with `UnsupportedError` and the text "the token contract refused this transfer (its dry run reverted)",
  followed by the node's reason. It used to return a signed transaction with a fee estimate, and broadcasting it
  would have burned the fee without moving any token. Nothing is signed, and no other node is asked, because the
  refusal is the contract's and not the node's. GLEEC KDF returns the signed transaction in this case.
  The fee estimate of a TRC20 withdrawal counts all of the transfer's energy as the sender's. Some token contracts
  let their deployer pay part of the energy; for such a token the estimate, and the TRX the withdrawal asks the
  sender to hold, can be higher than what the chain then charges. Detail: PR #450; CRD ch.21 R21.10B.10 to R21.10B.13
  and the status note of 2026-11-20 after T21.10B.18.
- **TRON coins have a transaction history when a TronGrid API key is set.** With `tx_history: true` in the
  activation request and a TronGrid node that has an API key (the node's own `TRON-PRO-API-KEY` header, or the
  `MM2.json` key `tron_api_key`), TRX and TRC20 coins list their transactions through `my_tx_history`, in both the
  legacy and the `mmrpc: "2.0"` form. The rows come from TronGrid's account listings, and those requests go to keyed
  TronGrid hosts over HTTPS only. Addresses are shown in the form that starts with `T`. Under an HD wallet the history
  covers every known address, and a transfer between two own addresses is one row. A TRX row carries the amount and
  the fee; a TRC20 row carries the token amount, with the block and the TRX fee read once from a node; an own token
  transfer also shows as a fee-only row of TRX, so nothing is counted twice. The fee details have the shape of a
  withdraw result. Only confirmed transactions are listed and a chain reorganization is not followed; a TRC20 row has
  an empty `tx_hex`; amounts moved by staking, TRC10 assets and internal transactions are not shown. Without a key
  nothing changes: the legacy method answers an empty list with `NotEnabled` and the versioned one `NotSupportedFor`,
  as GLEEC KDF does. GLEEC KDF has no TRON history and ignores `tx_history` for TRON. The key is never logged or
  shown. In a browser build only the legacy method answers, and the keyed history has not been checked in a browser.
  Detail: PR #440; CRD ch.21 §21.10C.
- **Support code for gasless TRC20 transfers through GasFree was added.** The
  code derives a user's GasFree custody address and checks it against the GasFree contract, prepares and signs a
  transfer authorization, and reads account and fee data from a GasFree provider. The two items below use it: the
  address part in the reports, and the rest in the gasless withdraw. Detail: PR #433; CRD ch.63 §63.4 to
  §63.7, R29.
- **A GasFree provider can be configured when TRON is activated, and the balance answers show the GasFree deposit
  address.** The platform activation takes a `tron_gasless_provider` object (the provider's URL as scheme and host,
  its API key and secret, and the provider's address), and a TRC20 token activation takes a `gasless` block with an
  optional fee cap. A request with these members on a coin that is not a TRON coin, a provider URL with a path, or a
  fee cap the token cannot express is refused. With a provider configured, `my_balance`, the activation results and
  the HD address methods show `gasfree_address` next to each address: the address to send a token to before it can be
  moved without TRX. The address is checked against the GasFree contract first and is left out if the contract
  disagrees. The API key and secret are never logged or shown. Both can be used now that TRON is switched on: the
  provider is configured and the GasFree address is reported. A token that sits on the GasFree address is moved with
  a gasless withdraw in its task form (the next item). The provider object's `status_poll_interval_ms` (3000 by
  default) is the time between two questions to the provider about a submitted transfer. Detail: PR #441; CRD
  ch.63 §63.3.1 to §63.3.3, R18, R29.
- **A TRC20 token can be withdrawn without TRX through a GasFree provider: the task form of `withdraw` submits the
  transfer and follows it until it settles; the direct `withdraw` only signs it.** The withdraw request takes
  `fee_method` (`native`, `gasless` or `auto`) and a `gasless` object (`max_fee`, `deadline_seconds`,
  `fallback_to_native`). With `gasless`, the fee is paid in the token from the GasFree address.
  *The task form* (`task::withdraw::init`, then `task::withdraw::status`) signs the authorization, hands it to the
  provider and asks the provider about the transfer every `status_poll_interval_ms`, and at most once a second
  whatever that setting says, until it is settled. So KDF can now move a token that sits on the GasFree address.
  While the task runs, its status shows
  `SubmittingGaslessAuthorization`, then `WaitingForGaslessProvider` (an object with the provider's `trace_id` and
  `provider_state`) and `WaitingForGaslessSettlement` (an object with `trace_id` and `tx_hash`, which is `null`
  until the provider names its transaction). The final answer holds the signed authorization, the fee in the token
  with the provider's `trace_id`, and `tx_hash`, the hash of the provider's transaction; it has no `tx_hex`. The
  submission is sent once and never repeated. If the submission gets no usable answer (a timeout, a broken
  connection, a provider error, or an answer that does not match what was signed), the provider may still have
  accepted the transfer: the error text then says so and asks you to check the GasFree address balance and its
  history before trying again, because a second withdraw would be a second transfer, and a warning in the log names
  the coin, the addresses, the amount and the request id. The task ends with an error when the authorization has
  expired before it could be sent (`QuoteExpired`; nothing is sent), when the provider's answer does not match what
  was signed (`InvalidProviderResponse`), when the provider reports the transfer as failed (`ProviderRejected`) or
  does not know its trace id (`TraceNotFound`), and when the transfer has not settled 120 seconds after the
  authorization's deadline (`Transport`; the outcome is then unknown). Every error after the submission names the
  trace id, so the transfer can be looked up at the provider. `task::withdraw::cancel` before the submission stops
  it; after the submission it only stops the waiting, and the transfer may still complete. A cancel, or a stop of the
  node, while the status is `SubmittingGaslessAuthorization` leaves the outcome unknown; a log line written when the
  submission is sent carries its request id, which identifies it.
  *The direct `withdraw`* only signs: its answer holds the signed authorization and the fee in the token instead of
  a transaction, with no `tx_hex`, no `tx_hash` and a `trace_id` of `null`, and nothing is sent to the provider.
  With `auto`, the ordinary TRX-paid transfer is tried first and the gasless one only when it cannot be funded. Other
  coins refuse `gasless` and `auto`. On every coin, a request that carries a `gasless` object without `fee_method`
  `gasless` or `auto` is now refused (`InvalidFeePolicy`, HTTP 400); until now an unknown `gasless` member was
  ignored. The new `MM2.json` setting `tron_gasfree_guard_allows_fallback` (default `false`) decides whether a
  withdraw with `fallback_to_native` may use the TRX-paid transfer when the GasFree contract check refuses. This can
  be used now that TRON is switched on. The way to move tokens back from the GasFree address is a gasless withdraw to
  your own address, through the task form. Detail: PRs #445, #495; CRD ch.63 §63.3.4 to §63.3.6, §63.8, §63.8A,
  §63.8B (D63.9 and its status note), R29; RELOADED_VS_GLEEC.md.
- **TRON swap groundwork: a swap with a TRX or TRC20 coin on either side will use the 32-byte secret hash.** The
  legacy swap's choice of the secret-hash width selects SHA-256 over 32 bytes for such a pair, as the TRON swap
  contract requires; ETH and ERC20 pairs keep the 20-byte hash. TRON swaps stay off, so nothing a user can run
  changes yet. Detail: PR #499; CRD ch.51 R72 status note, ch.21 R21.B6.
- **The warning about `swap_contract_address` at a legacy TRON activation has a new text.** A TRON coin is still
  wallet-only and still ignores the swap contracts of an `enable` or `electrum` request. The log line now reads
  "swap_contract_address is ignored, the coin is wallet-only". Nothing else changes for a user: the groundwork for
  TRON swaps added here (the contract interface, the transaction form and the contract binding at activation) is not
  used by any order or swap while TRON swaps are off. Detail: PR #506; CRD ch.21 §21.11C, R21.B2.
- **A TRON platform accepts `swap_evidence_node` at activation.** The field names one node of the request's own
  node list to trust for swap evidence instead of the two-node agreement (see "Breaking changes and upgrade
  notes"); a value that is not in the list refuses the activation with `SwapEvidenceNodeNotInList`. TRON swaps stay
  off: the part of a TRON swap that reads the chain (checking the other side's payment and the DEX fee, this node's
  claim and refund, the refund time, the search for a claim and its secret) is built and waits for the switch-on.
  Detail: PR #509; CRD ch.21 §21.11D.

#### Tendermint (Cosmos) coins

- **A Tendermint coin now has a transaction history.** `tx_history: true` on a Tendermint platform
  coin starts reading the account's transactions from a node in the background, and `my_tx_history`
  (the mmrpc-2.0 method) lists them for the coin and for its tokens: plain sends, token sends,
  IBC transfers, HTLC creations and claims, delegations, undelegations and reward claims, each with
  its amounts, fee and memo, and the fee of a token send as a row of the platform coin. While the
  first read of the chain runs, the history state is `InProgress` with the number of transactions
  left; it is `Finished` afterwards. A fee-only transaction counts its fee once, a reward claim
  counts the fee it cost, a transaction touching several validators gets one row per validator, and
  `tx_hex` is the signed transaction. The history of a refunded HTLC or a completed undelegation has
  no row, because the chain records no transaction for them. The history of a custom token that is
  not in the coins file is absent in 0.5.0. A history store written by this node is listed by the other
  implementation too (checked against a reference release build on 2026-11-15); in this change only
  Tendermint coins write the index that makes this work. A history database is per coin and shared by
  every account activated on it, so rows of an earlier account stay and are listed under their own
  addresses (a known property of the shared database, the same as at GLEEC KDF). A transaction whose
  block time no node can give is read again by the next pass. Native only in 0.5.0: the
  browser build has no versioned history store for any coin family. Detail: PR #363;
  CRD ch.36 §36.14.1; §36.14.7.
- **A Tendermint coin can be activated from a public key (watch-only).** `activation_params` of
  `enable_tendermint_with_assets` (and the task form) now accepts `{"type": "WithPubkey", "params":
  {"pubkey": {"type": "secp256k1" | "ed25519", "value": [bytes]}, "is_ledger_connection": bool}}`. The
  coin's address is the account of that key (an `ed25519` key by the public Cosmos SDK rule for that
  key type), and nothing is asked of a node or a wallet at activation. Such a coin holds no private
  key: balances, the address and the history work as for any coin; `withdraw` and the delegation,
  undelegation and reward-claim requests do not sign or send anything but answer an unsigned
  document for the caller's own wallet to sign (no `tx_hex` and no `tx_hash`; a `sign_doc` with
  `body_bytes`, `auth_info_bytes`, `chain_id` and `account_number`). With `is_ledger_connection: true`
  a `withdraw` answers the Amino form instead (`legacy_amino_json` and `original_tx_type_url`) with
  three times the computed fee (for the coin, not for a token), and the staking requests fail. A `memo` in the `withdraw` request is carried in the transaction: `memo` is now a typed member of every withdraw request: a `memo` that is not a string is refused as a malformed request for every coin (it was ignored before); only Tendermint coins and tokens read it. Placing an order with the coin or one
  of its tokens, message signing, private-key display, and every swap payment, claim and refund are
  refused (`PrivKeyNotAllowed::WatchOnlyCoin`). A malformed `activation_params` now fails request
  parsing with `InvalidRequest`. Detail: PR #382; CRD ch.36
  §36.14.3.
- **A Tendermint coin can be activated with a WalletConnect wallet, and the wallet signs its
  withdrawals.** `activation_params` of `enable_tendermint_with_assets` (and the task form) now accepts
  `{"type": "WalletConnect", "params": {"session_topic": "..."}}` and the form the public docs show,
  `{"priv_key_policy": {"type": "WalletConnect", "data": "..."}}`; both do the same. The node reads the
  account and its public key for the coin's chain from the paired session: from the key record the wallet
  delivered with the session when it has one for that chain, otherwise by asking the wallet once for its
  accounts. When the key does not give the account the session granted for that chain, the activation
  fails (`PlatformCoinCreationError`) instead of activating another account. The coin holds no private
  key. `withdraw` and the delegation, undelegation and reward-claim requests are signed in the wallet (the
  Ledger-backed account of a wallet signs the Amino form, which covers a plain send only, and a Ledger
  `withdraw` costs three times the computed fee, for the coin and not for a token). A `withdraw` answers
  the signed transaction (`tx_hex`, `tx_hash`) and broadcasts nothing; the staking requests broadcast the
  signed transaction. The wallet's answer must carry the transaction body that was asked to be signed;
  an echo of another body fails the request. A request the wallet refuses, does not answer within five
  minutes, or cannot receive because the session is gone fails with the new error `SigningError` (HTTP
  500) for `withdraw` and for the staking requests, where GLEEC KDF answers a general internal error.
  Placing an order with such a coin or one of its tokens is refused, as for a watch-only coin, and swaps
  do not sign through the wallet. The top-level `priv_key_policy` member of the request still refuses the
  WalletConnect spelling. The fee and the gas limit of such a request come from the node's gas simulation
  (gas limit = simulated gas x 1.5, fee = limit x gas price; an explicit gas limit is kept), because the fixed
  limit of 125,000 ran out of gas for a real send and lost the fee; a failed simulation fails the request.
  A test with a real wallet is still to be run. Detail: PR #393; CRD ch.36
  R36.14.13--R36.14.15.

### Changed

#### RPC interface

- **Disabling a coin now ends its background work.** When `disable_coin` removes a coin (a coin, a token, a platform
  with its tokens by `force_disable`, or the clean-up of a failed or cancelled activation), the transaction-history
  collection of that coin and the block-header sync of a UTXO coin stop, and the balance, fee-estimate and
  transaction-history streams of the removed ticker end for every client: a client that was subscribed sees the stream
  stop, as after its own unsubscribe, and subscribes again after the coin is enabled again. Before, history collection
  of BCH, EVM and Tendermint platforms ran until the node stopped, and a removed coin stayed alive in its streams. A
  passive platform keeps its work. A removed Lightning coin keeps its background work (graph persistence, peer
  listener, reconnect and announcement loops) until the node stops; that is a known limitation of this release. Detail:
  PR #439; CRD ch.43 R43.1E.5 item 4.
- **A token or layer-two activation waits for a `disable_coin` of its platform, and the other way round.** The token
  and layer-two activation methods (`enable_erc20`, `enable_nft`, `enable_slp`, `enable_tendermint_token`, the Solana
  token method, the Lightning activation, and the task forms) now take the activation lock of the platform with the lock of
  the token, and `disable_coin` takes the lock of the coin it disables, so a platform cannot be disabled in the
  middle of the activation of its token. A token activation on a platform that is passive is refused with
  `PlatformCoinIsNotActivated` naming the platform (the Lightning ticker for Lightning). Detail: PR #424; CRD ch.43 D43.1F.2;
  ch.55 R55.1.12.
- **A refused platform-with-tokens activation leaves none of its tokens on the platform's token list.** The list a
  platform reports in its activation answer and its balance stream is now written only once the tokens are registered,
  so a request that failed after its tokens were built no longer leaves them on the list of the platform. Detail: PR
  #432; CRD ch.43 D43.1F.2.
- **A standalone activation task for the ticker of a passive platform is refused at `init`.** `task::enable_utxo::init`
  and the other standalone activation tasks answer `CoinIsAlreadyActivated` with HTTP 400, as for an active coin,
  and build nothing. Detail: PR #424; CRD ch.43 R43.1F.9 and D43.1F.4.
- **A transport failure of `withdraw` answers HTTP 502 instead of 500.** The versioned `withdraw`, `withdraw_nft` and the
  start of `task::withdraw::init` answer the error `Transport` (a node or server that does not answer, or answers
  unusably) with status 502, as the reference does; the token and the body are unchanged. A client that treats
  `withdraw` failures by status class now sees a network fault as 5xx gateway class rather than a server error.
  Detail: PR #386; CRD ch.43 R43.1D.2, ch.49 status note.
- **A refused legacy `disable_coin` always carries the lists a client may read.** When running swaps or orders
  being matched block the deactivation, or the coin is not active, the error body carries `orders` (with
  `matching` and `cancelled`), `active_swaps` and `swaps` (the same list as `active_swaps`), all present and
  empty where nothing applies; before, each refusal carried one of them or none. The error text, HTTP 500 and the
  cancel-first behavior are unchanged. Detail: PR #386; CRD ch.43 R43.1E.3, D43.1E.1 status note.
- **Hardware-wallet failures of `withdraw` answer the one error `HwError`.** The versioned `withdraw`, `withdraw_nft`
  and the start of `task::withdraw::init` no longer answer `TrezorDisconnected`, `NoTrezorDeviceAvailable`,
  `FoundUnexpectedDevice` or `HardwareWalletInternal`. A device condition answers `error_type` `HwError` with
  HTTP 410 and `error_data` naming the condition with the same words the activation methods use
  (`NoTrezorDeviceAvailable`, `FoundMultipleDevices`, `FoundUnexpectedDevice`, `InvalidPin`, `ButtonExpected`,
  `UserCancelled`, `InvalidSignature`, ...), so a client handles hardware failures of every method alike. A
  connection timeout answers `Timeout` (408). Device failures that have no such word (transport, protocol,
  invalid key data, internal) answer `InternalError` with HTTP 500, as before under the removed name. Detail:
  PR #386; CRD ch.50 R50.17 status note, ch.43 R43.1D.2, ch.55 R55.2.17.
- **The HD account methods answer hardware-wallet failures as `HwError` too.** `task::create_new_account` and the
  other HD account methods no longer answer `TrezorDisconnected`, `NoTrezorDeviceAvailable`, `FoundUnexpectedDevice`
  or `HardwareWalletInternal`; a device condition answers `HwError` (410) with the same words as above, and a
  device that does not answer in time answers `Timeout` with HTTP 408 instead of 500. Device failures without a word
  answer `Internal` (500). Detail: PR #386; CRD ch.05 §5.9B R35c.
- **A disabled token no longer stays in its platform's token list.** After `disable_coin` of an ERC-20 or TRC20,
  SLP or Tendermint token, its platform no longer reports or serves that token, as was already the case for SPL
  tokens. A single SPL token enabled with `enable_spl` is now listed by its platform, as a token enabled together
  with the platform already was. Disabling a platform that still has active tokens works as before: the platform
  is removed and its tokens stay. Detail: PR #418; CRD ch.43 R43.1E.5 status note of 2026-11-18.

- **RPC answers are sent as the handler produced them.** `<`, `>` and `&` in answer strings are no longer replaced
  with `&lt;`, `&gt;` and `&amp;`, for every method including the `wc_*` ones. The visible effect: URLs with query
  strings, memos, error texts and labels reach the client with their plain characters. A request body that
  contains `<`, `>` or `&` is still refused, as before. Every answer now carries `X-Content-Type-Options: nosniff`, and
  `Content-Type: application/json` where the method set no content type. Detail: PR #421; CRD ch.55 status note of 2026-11-18.

#### Shielded coins

- **The Zcash libraries are upgraded and no longer carry local patches.** ARRR and the other shielded coins now run on
  the published releases of the Zcash wallet, transaction and proof libraries, unmodified; the three patched copies kept
  in the source tree are gone. Transactions are built by the node's own assembler over the libraries' public interface.
  A shielded transaction expires after the same 40 blocks as before, and the Zcash network upgrade NU7 is known to the
  node, so a transparent-only Zcash transaction under its branch identifier is read by both of the node's readers. The
  wallet database is migrated once; read "The shielded (ARRR, Pirate) wallet database is migrated irreversibly" under
  "Breaking changes and upgrade notes" above.
  Detail: PR #411; CRD ch.39 status notes of 2026-11-18, ch.34 ledger entry of 2026-11-18.
- **The shielded coin's library stack now also builds with Orchard scanning enabled; no behavior
  change.** The way the transaction builder is asked for an output-only Ironwood bundle no longer
  changes a public type that the Zcash wallet library constructs itself, which kept that library from
  compiling with its Orchard feature on. Transactions are built exactly as before.
  Detail: PR #375; CRD ch.39 status note of 2026-11-15, ch.34 ledger entry of 2026-11-15.
- **The backup a shielded wallet database gets before a schema upgrade is now consistent, checked and
  space-aware.** Until now the backup was a plain file copy, which could miss recent changes that were
  still waiting in the database's log file, and it was neither checked nor limited in number. Now the
  node first checks the disk has room for two copies of the database plus 16 MiB and, if not,
  refuses to activate the coin with an error that gives the bytes needed and available, without
  writing a backup. It then writes a consistent copy named `<TICKER>_RELOADED_WALLET.db.pre-upgrade.<ms>.bak`
  and checks it (SQLite integrity check, same table layout as the original); a copy that fails is
  deleted and the activation fails, with the wallet database's content unchanged (opening it may
  recover an interrupted write, and closing it may fold its log file into it). The upgrade itself runs on a
  second copy made from the checked backup. After a successful upgrade only the newest
  `.pre-upgrade` backup is kept; `shielded_db_upgrade_backups` in `MM2.json` sets that number
  (default 1, lowest 1). Other `.bak` files are left alone. To go back to an older build, put the
  backup over the wallet database file as described in `RELOADED_VS_GLEEC.md`.
  Detail: PR #407; CRD ch.39 R39.8.0bc, T39.8.0j.
- **ARRR pays one fixed fee of 0.0001 ARRR (10,000 base units) for every transaction, the fees shown before a trade
  are that fee, and its minimum trading volume is 0.001 ARRR.** With no `txfee` in its coin entry (the published
  ARRR entry has none), a shielded coin pays 10,000 base units for a withdrawal, a swap payment, the DEX fee, and
  the spend and refund of a swap payment, in place of 1,000; `trade_preimage` and the swap figures report the same
  10,000 (the receiver fee was 496 before), so what a trade is quoted equals what it pays. The minimum trading
  volume, and the default minimum volume of a new order, is ten times that fee: 0.001 ARRR in place of 0.0001 ARRR.
  A `txfee` in the coin entry still sets the fee and scales the minimum; an entry with `txfee: 0` or the ZIP 317 fee
  class is refused at activation. Other UTXO coins keep their default. Detail: PR #544; CRD ch.39 R39.6.5f (status
  note of 2026-11-26).

#### Tendermint (Cosmos) coins

- **Tendermint activation refuses a hardware key policy instead of ignoring it.** A request that
  names the `Trezor` key policy used to activate the coin with the local key without a word; it now
  fails before any node is contacted, with a `PlatformCoinCreationError` that says the hardware key
  policy is not supported for Tendermint coins and to activate with the local key. An absent
  `priv_key_policy`, `ContextPrivKey` and `IguanaPrivKey` activate as before. Any other value, such
  as the WalletConnect form, fails request parsing with `InvalidRequest` (HTTP 400) instead of
  activating with the local key. Detail: PR #358; CRD ch.36 §36.7, status note of 2026-11-15
- **A Tendermint activation under an HD login now honors `priv_key_policy: IguanaPrivKey`.** It used
  to ignore the member and activate the key at `path_to_address`. It now activates the coin with the
  node's single internal key, as the UTXO family does, and `path_to_address` plays no part.
  An absent `priv_key_policy` and `ContextPrivKey` activate the key at `path_to_address` as before,
  and an iguana login does not change. Tokens follow the platform coin's key, and the task form
  behaves the same. Detail: PR #396; CRD ch.36 §36.7, status note of 2026-11-17

#### EVM, BCH, Sia and Lightning coins

- **Under an HD login, `priv_key_policy: IguanaPrivKey` now activates the node's own single address for
  EVM coins, BCH with its tokens, Sia and Lightning, as the UTXO family does.** Under a global-HD login
  these families ignored the value (EVM treated it as `ContextPrivKey`, so the coin was an HD coin at
  `path_to_address`; BCH with tokens ran on the old passphrase-hash key; Sia and Lightning had no such
  request member). The value now runs the coin on the node's internal key, the key the node uses as its
  own identity under an HD login, and the activation answers in the single-address form. An absent value
  and `ContextPrivKey` keep today's behavior, and an iguana login is unchanged. `get_my_address` answers
  the address of an EVM coin that is active as a single address, instead of the HD address at
  `path_to_address`, and a token activated with `path_to_address` on such a platform is no longer refused.
  Sia and Lightning take the new member `priv_key_policy` (default `ContextPrivKey`; a hardware value is
  refused by both). EVM still takes the value only in the
  object form, `{"type": "IguanaPrivKey"}`, as before.
  **Warning for Lightning:** with an HD login, requesting `IguanaPrivKey` starts a separate Lightning
  identity (its own node key, data directory, backup directory and history). The channels of the other
  identity are not shown and are not watched while this identity is in use: close or settle them first,
  or do not stay on the other identity longer than the channels' dispute delay. A counterparty could
  close such a channel with an old state, and if nobody watches within the dispute delay the penalty
  cannot be claimed and funds can be lost. Nothing is moved or deleted, and the channels are shown and
  watched again when the activation goes back to the default policy. A data directory that was opened
  by another node key is refused instead of being opened. A Lightning activation is also refused while the other identity still has channels that are
  not closed (pending open, open, or closing without a recorded closing transaction): the error tells you
  to close them first by activating under the previous `priv_key_policy`. Closed channels, and an empty or
  missing history of the other identity, allow the activation. Detail: PR #400; CRD ch.05 R46,
  status note of 2026-11-17.
- **An ERC-20 withdraw accepts `from` under an HD wallet.** `withdraw` and `task::withdraw::init` of a token take
  the address from the platform coin's HD wallet (address id or derivation path), check the token balance and the
  nonce at that address, and sign with its key; `from` of the result is that address. A selector that does not
  resolve is refused before any node request. A token on a single-key platform still refuses `from`. Under a
  hardware wallet the task route signs at the selected path. Detail: PR #438; CRD ch.49 R49.34.
- **The HD address methods answer an ERC-20 token ticker from its platform's HD wallet, with the token's balance
  only.** `get_new_address`, `account_balance`, `task::account_balance::init` and `task::scan_for_new_addresses::init`
  asked for a token ticker use the platform's accounts and known addresses, so an address found or created through
  the token is listed by the platform as well. The scan counts an address as used when its nonce or that token's
  balance is not zero. Detail: PR #438; CRD ch.49 R49.32.
- **`task::scan_for_new_addresses::init` serves EVM coins under an HD wallet.** It was answered only for TRON coins
  and UTXO coins; an EVM platform now answers it. Detail: PR #438; CRD ch.05 R54.
- **An EVM fee check no longer aborts on a non-EVM transaction; it returns an error.** The check of a
  swap's dex-fee transaction on an EVM coin or token stopped the running task when it was handed a
  transaction of another coin family. It now refuses the transaction with an error message. In the
  same way, a payment event that a node reports without a transaction hash is an error, not an abort.
  Detail: PR #452.

#### Solana coins

- **Solana nodes are checked before use and spread across users.** A Solana coin's nodes are now put
  into a random order once when the coin is enabled, so users of one coins file spread over its
  nodes; set the `MM2.json` string `solana_node_order` to `listed` to keep the listed order, as
  GLEEC KDF does. A node is asked whether it is healthy (`getHealth`) before it is used when it has
  not been checked in the last 30 seconds; a node that fails is skipped for 20 seconds, doubling up
  to 12 hours, and a recovered node that ranks higher is used again. The reads of one withdraw go to
  one node, and a sent transaction is never rebuilt: a failed broadcast is retried with the same
  signed bytes, and the error names the transaction's signature. This applies to both Solana
  activation surfaces. Detail: PR #426; CRD ch.40 R40.1.5.

- **Solana withdraws send the right amounts and say what they cost.** An SPL withdraw with `max` now
  sends the whole token balance (it used to move the requested amount while the answer showed the
  whole balance). Creating the recipient's token account is paid in SOL: the rent is checked against
  the SOL balance together with the fee, a short balance answers the new error
  `NotSufficientPlatformBalanceForFee` naming SOL, and `fee_details` shows `fee_amount`,
  `rent_amount` and `total_amount` for SOL and SPL withdraws alike, instead of a single `amount`.
  A token balance is read from the wallet's own token account for that token, and is zero when the
  wallet has none. A recipient that is a token account rather than a wallet address is refused. An
  amount below the coin's minimum transaction amount answers `AmountTooLow`. Activation now checks
  that the token's mint belongs to its token program, so a node failure while an inline token is
  checked answers `Transport`; a Token-2022 token is refused at activation until it is supported.
  Detail: PR #425; CRD ch.40 R40.6B.9-R40.6B.12, D40.6B.2-D40.6B.6; ch.43 R43.1D.2.
- **`disable_coin` works for Solana coins and SPL tokens.** A token, and a platform without tokens,
  are removed and can be activated again; a removed token no longer stays in its platform's token
  records. A platform with active tokens is handled as for every other family: the platform is
  removed and its tokens stay. A Solana ticker that is not active answers the usual refusal.
  Detail: PR #413; CRD ch.40 D40.6B.1, T40.6B.20; ch.43 §43.1E.

- **Every wallet request naming a Solana coin or SPL token answers, and Solana swap operations
  answer an error.** Message signing and verification, address validation, the confirmation settings
  and `show_priv_key` now answer; `validateaddress` accepts every valid Solana address (any base58
  text of 32 bytes), and `show_priv_key` gives the 64-byte keypair in base58 that common Solana
  wallets import. `convertaddress`, `get_raw_transaction`, `get_trade_fee`, `sign_raw_transaction`,
  the trade-fee part of `max_taker_vol` / `max_maker_vol` and the legacy `my_tx_history` answer an
  error that names the coin instead of ending the request, and so does every swap operation, also
  for a swap imported or recovered by hand; switching a coin off follows in a later change. The
  minimum transaction amount is now one lamport for SOL and one smallest unit of each SPL token.
  Detail: PR #401; CRD ch.40 D40.6B.1, R40.6B.14, §40.6A.

- **Solana activation errors follow the shapes GLEEC KDF uses, and a token address is checked when
  its coins entry is parsed.** On `enable_solana_with_tokens` a failed block-height or balance read
  is now `PlatformCoinCreationError` with the platform ticker instead of `Internal`, and on
  `enable_spl` a node failure stays `Transport`. An SPL token entry whose `token_contract_address`
  is not a valid public key is now `TokenProtocolParseError` instead of `Internal`. The Solana
  platform always counts 9 decimals; a `decimals` value in its coins entry is ignored, and a token
  takes its decimals from `protocol_data` only. Detail: PR #371; CRD ch.40 §40.3A, R40.6B.2.

- **Solana libraries moved to their 3.x line.** The Solana and SPL token libraries the coin is built on
  moved from the 2.x line to the 3.x line (`spl-token` 9). Addresses, signatures and the bytes of the
  transfers the coin builds are unchanged: fixed test vectors check a single-key and an HD key, a
  signature, a SOL and an SPL transfer, and the token-account address under both token programs. The
  old `ed25519-dalek` 1.0.1 library leaves the dependency tree, so the dependency policy no longer
  ignores RUSTSEC-2022-0093.
  RUSTSEC-2024-0344 is not cleared by this change: `curve25519-dalek` 3.2.0 also came in through the
  peer-to-peer library's encryption layer, which PR #470 moves to the new library line.
  Detail: PR #465.

#### UTXO coins and Electrum

- **Signing a Radiant (RXD) transaction with a malformed output script now fails with an error.** Radiant's
  signature hash is undefined for an output script that cannot be decoded, that pushes a reference it also
  disallows, that holds more than one state separator, or that has a return before a state separator. The
  raw-transaction signing request used to sign such a transaction anyway, and Radiant rejected it. It now
  answers with a signing error that names the output index and the case. Withdrawals and swaps never build
  such outputs and are not affected, and no other coin is affected. Detail: PR #381; CRD ch.58 R58.3.3, D5.
- **The log tells when QTUM and its QRC20 tokens open, share and close a set of Electrum connections.** One INFO
  line is written when a set is created (naming the instance that created it), one each time another instance
  reuses it, and one when the last instance lets go of it and it closes. Each line names the platform (for example
  `QTUM`) and the servers as `host:port` only, without the protocol or any user name, password, path or query of a
  server entry. No line is written per request or on a timer. Detail: PR #476; CRD ch.38 status note of
  2026-11-22 after D38.6.4Q.1.

#### ZEC

- **ZEC pays the Zcash network's standard fee (ZIP 317).** With the version-5 ZEC coins entry (`txversion` 5 and the
  branch id `77190ad9`), every ZEC transaction pays ZIP 317's conventional fee: 0.00005 ZEC per logical action, at
  least two actions, where each transparent input of up to 150 bytes and each output of up to 34 bytes counts as one.
  A withdrawal with up to two inputs and two outputs pays 0.0001 ZEC, a DEX fee or a swap payment with one input
  0.0001 or 0.00015 ZEC, and a swap claim or refund 0.0001 ZEC instead of 0.0003472. The entry's `txfee` is no
  longer used for ZEC; KMD, ZER, BTCZ, FLUX and the other Zcash-family coins keep their per-kilobyte fee.
  - *Previews:* `trade_preimage` and `max_taker_vol` use the same fees, and `get_trade_fee` shows 0.0001 ZEC.
  - *Explicit fee:* a `fee` given in a withdraw request is used as given. ZIP 317 expects at least the conventional
    fee; a lower fee can delay the transaction or keep it out of blocks.
  - *Lower minimum for ZEC orders:* ZEC's minimum trading volume, also the default minimum volume of a new ZEC order,
    is 0.001 ZEC instead of 0.003472, ten times the new claim fee.
  - *Version-2 swaps:* the spends and refunds of a resumed version-2 swap keep the earlier fee (0.0003472 ZEC),
    because the other side of such a swap checks it against that figure.

  Detail: PR #431; CRD ch.39 R39.11.4V.10 status note.

#### Transport, TLS and dependencies

- **The URL library moved to 2.5.9, which clears RUSTSEC-2026-0592.** The `url` crate moved from 2.5.8 to
  2.5.9, and `form_urlencoded`, which the new version requires, from 1.2.2 to 1.2.3. KDF Reloaded's own code
  does not call the affected function. The dependency policy needs no exception for this advisory. Detail:
  PR #492.

- **The peer-to-peer encryption no longer uses the old curve25519 library with the timing advisory.** The
  encrypted handshake between nodes (Noise) was built on `curve25519-dalek` 3.2, which carries RUSTSEC-2024-0344
  (the time of one arithmetic step can depend on secret data). It now uses the fixed library line
  (`x25519-dalek` 2, `curve25519-dalek` 4). The handshake itself is unchanged, byte for byte, and nodes running
  older versions connect as before. With the Solana libraries already moved off it, the old library is now out
  of the build, and the dependency policy no longer ignores RUSTSEC-2024-0344. Detail: PR #470; CRD ch.28 OQ2
  status note of 2026-11-22;
  `vendor-patches/libp2p-noise-0.43.0/PATCH-NOTES.md`.

#### Swap protocol v2

- **A v2 swap never refunds after its own claim, repeats a chain search after a node error, and takes an EVM secret
  only when it matches the secret hash.** After the node's own claim of the other side's payment is broadcast, the
  swap reads its confirmation again every 10 seconds and ends as completed when it is read. It waits until 3700
  seconds past the lock time of its own payment, and at least 3700 seconds from the start of that wait (a restart
  starts the wait anew); then it ends as aborted with a reason whose text names the claim transaction. The maker's
  search for the spend of the taker funding treats an unreachable node, or a node answer that cannot be decoded, as
  a passing error on every coin family and searches again after 30 seconds. On EVM coins the value read from the
  spend of the taker payment is used as the secret only when its hash equals the swap's secret hash.
  - Detail: PR #532; CRD ch.52 R18, R19, R32, R33 and their status notes of 2026-11-23, ch.51 §51.6G R100.
- **A v2 swap waits when a chain read it rests on is undetermined, and neither refunds nor claims on it.** This
  holds for the maker's search for the spend of the taker funding, the maker's and the taker's reads of the other
  side's payment confirmations, the taker's search for the spend of the taker payment, and the refundability check
  of both sides. An undetermined read is read again at the step's poll interval; it is never taken as "nothing
  found", "not spent", "not confirmed" or "refundable". The wait lasts until 3700 seconds past the lock time of the
  node's own payment (for the taker's read of the maker payment's confirmations: of its funding), and at least 3700
  seconds from the first undetermined read at or after that lock time (a restart starts the wait anew). A read that
  follows an undetermined one asks the nodes again also after the step's own deadline; a confirmation, or a spend
  of the taker funding, that is read only then is not acted on and the step takes its refund path. When the
  wait ends while the read is still undetermined, the swap ends as aborted, with a reason whose text names the read,
  the coin and the fix: add a server on a second host to the coin's node list or set `swap_evidence_node`, then run
  `recover_funds_of_swap`. The log has one warning per wait. A read that fails
  in another way is handled as the step's own rule says.
  - Detail: PR #533; CRD ch.52 R18, R19, R21, R30, R31, R34 and their status notes of 2026-11-24, ch.51 §51.6G R100.

### Fixed

#### Orders

- **Saved orders of a coin that cannot be traded are kept, but no longer announced or matched.** After a restart,
  a saved order of a coin this build cannot trade stays in `my_orders` but is not announced to the order book and
  is not matched, and one line in the log names it. This covers wallet-only coins, Lightning, Solana, and
  Tendermint and TRON while their swaps are off. Before, a saved Lightning or Solana order returned to the order
  book and every match failed, and a saved Tendermint or TRON order was cancelled. The operator can still cancel
  such an order with `cancel_order`, which still sends its cancel message. A kept taker order still times out as
  before: fill-or-kill is removed, good-till-cancelled becomes a maker order that is kept and not announced.
  Detail: PR #485; CRD ch.60 R60.6.5.
- **A wallet-only coin is refused as wallet-only before the two-node check.** `setprice`, `buy` and `sell` with a
  coin that cannot trade (a wallet-only coin, every TRON coin while TRON swaps are off) now answer the wallet-only
  error first. Before, a coin that also had fewer than two nodes was told to add a node or set `swap_evidence_node`
  (see the R100 item under "Breaking changes and upgrade notes"), which would not have made it tradable.
  Detail: PR #509.

#### EVM, BCH, Sia and Lightning coins

- **The HD address methods of an EVM platform list its tokens.** `get_new_address`, `account_balance`,
  `task::account_balance::init` and `task::scan_for_new_addresses::init` asked for the platform coin now answer, per
  address, the platform and every ERC-20 token activated on it at that moment, including tokens with a zero balance;
  the total of `task::account_balance::init` sums each. They showed the platform coin only. If one balance cannot be
  read, the request fails instead of returning a partial list; `get_new_address` still keeps the new address, as it
  already did when the platform balance could not be read. Asked for a token, they still answer that token only.
  Detail: PR #463; CRD ch.49 R49.32.
- **A scan for new addresses of an EVM platform finds token-only addresses.** An address counts as used when its
  transaction count, its platform balance or the balance of any activated token is not zero, so an address that only
  received a token is found. The unused addresses between the known range and a newly found address are listed with
  an empty balance map. The scan made during activation now follows the same rule (next item). Detail: PR #463; CRD
  ch.49 R49.32.
- **The address scan at activation of an EVM platform takes the tokens of the same request into account.**
  `enable_eth_with_tokens` and `task::enable_eth::init` under an HD login now count an address as used when its
  transaction count, its platform balance or the balance of any token named in the request is not zero, so an address
  that only holds a token is found and listed. The unused addresses the scan passes over are listed with an empty
  balance map, and each token's balance is asked once per listed address. If a node query of the scan fails, the
  activation fails with `Transport` and the scan records no address. A TRON platform keeps its answers.
  Detail: PR #478; CRD ch.49 R49.36.
- **An HD account that knows no address is scanned again at the next activation under the default scan policy.** A
  first activation that fails while it scans leaves the account stored with no known address, and later activations
  under `scan_if_new_wallet` used to treat it as an existing wallet and never scan it, so used addresses beyond the
  minimum stayed unlisted until a scan was asked for. Such an account is now scanned as a new one would be, for UTXO
  coins, QTUM, Sia and EVM coins including TRON. An account that knows an address is not scanned again under the
  default policy, and `do_not_scan` never scans. The rule does not tell such an account apart from one that knows no
  address because it was never used, for example one made by `task::create_new_account` and activated with
  `min_addresses_number` 0: that account is scanned again at every activation under the default policy, up to
  `gap_limit` addresses per chain, and on an EVM platform every token's balance is asked at each of them. GLEEC KDF
  does not scan such an account. Detail: PR #478; CRD ch.49 R49.36 item 7, ch.05 R35a.
- **A quick disable and re-enable no longer leaves the old history loop running, and a failed legacy activation no
  longer removes another activation's coin.** The history loops (UTXO family, EVM, Sia) now end when the registered coin
  of their ticker is another instance, not only when the ticker is unregistered, so the loop of the removed coin ends at
  once when the same ticker is enabled again. The clean-up of a failed `enable` or `electrum` balance read removes only the
  coin that call registered. Detail: PR #439; CRD ch.55 R55.4.9.
- **EVM transaction history could fail to start after an activation with `tx_history`.** The history collection
  started just before the platform was registered, and its first check for the registered coin could run before
  the registration, which ended the collection. It now starts after the registration, and a failed activation no
  longer leaves it running. Detail: PR #435; CRD ch.55 D55.8.
- **`get_new_address` on an EVM coin answers the full address instead of a shortened one.** The address came back
  abbreviated (the first and last digits with dots between them), which cannot be used. It is now the full
  checksummed `0x` address, as in every other HD answer. Detail: PR #438; CRD ch.35 R35.1.3, ch.49 R49.33.
- **An operation that a Lightning coin does not support answers an error instead of stopping the process.** The swap
  operations, the trade-fee estimates, the waits for a confirmation or a spend and the transaction decoding of a
  Lightning coin aborted the node when they were reached; each now answers an error. Orders and trade previews with
  a Lightning coin were already refused. The public key of a Lightning coin, which `get_private_keys` reads, is its
  node id. Detail: PR #479.
- **Under an HD login the Sia activation answer now lists the coin's address.** The answer of
  `task::enable_sia` is the per-account report under an HD login, but it listed account 0 with no
  address. It now lists account 0 with its single address, the one the coin uses, and that address's
  balance. Detail: PR #402; CRD ch.46 R46.2.4.

#### WalletConnect

- **The pairing link of `wc_new_connection` is usable by a wallet.** Every `&` after the first was
  returned as the text `&amp;`, so a wallet found no key in the link and refused it. The link now arrives
  with plain `&` separators, as every RPC answer does (see "RPC answers are sent as the handler produced them"
  under Changed). Detail: PR #393; CRD ch.22 §22.2.
- **The node reconnects to the WalletConnect relay.** The relay closes the connection about every ten
  minutes; the node did not reopen it, and a new connection request failed until a restart. It now
  reconnects with a growing wait and subscribes again to its pairings and sessions. Detail: PR #393;
  CRD ch.22 §22.5.2.
- **A WalletConnect session keeps working after the relay reconnects or the node restarts.** The node now
  keeps one relay client identity, stored with its sessions, instead of a new one for every connection.
  Field-checked with a wallet after a relay reconnect and after a restart. Detail: PR #393; CRD ch.22 §22.5.2.
- **A wallet lists each node as "KDF Reloaded" with a short identifier.** The wallet used to list every
  paired node as "Komodo DeFi Framework", so nodes could not be told apart. The name is now
  `KDF Reloaded <identifier>`, where the identifier is the first eight characters of the node's relay
  client key, the one the connect log prints; it does not change between restarts. An operator can set a
  name of their own with `walletconnect.app_name` in `MM2.json`. Detail: PR #393; CRD ch.22 §22.5.2.

#### Tendermint (Cosmos) coins

- **The search for the claim of a Tendermint swap payment takes only a successful claim of that payment.** The search
  for a claimed payment and the wait for its claim took the first transaction the node listed for the payment's HTLC
  id, without checking that the transaction succeeded or that it holds the claim. They now page through the node's
  results and take the oldest transaction that succeeded and has a claim message for this HTLC in any of its
  messages; failed or unrelated transactions are skipped. Tendermint swaps stay switched off in this version. Detail:
  PR #493; CRD ch.51 R91, ch.18 R18.11.16 and R18.11.19.
- **The fee reported for a transaction signed by a WalletConnect wallet is the fee of that transaction.**
  `fee_details` of `withdraw` and the staking requests is read from the signed transaction; it showed the
  computed fee even when the wallet signed another one. Detail: PR #393; CRD ch.36 R36.14.15.
- **Sending a Tendermint transaction the node already holds answers its hash.** A repeated
  `send_raw_transaction` was answered with an error ("tx already exists in cache") although the transaction
  was included. Detail: PR #393; CRD ch.36 R36.14.15.
- **A Tendermint node that accepts a request and never answers no longer holds it forever.** Every request of the
  native build to a Tendermint node is now bounded at 30 seconds, the node health checks included. A node that does
  not answer in time counts as failed, so the node choice moves on to the next configured node, and the history
  stops waiting on it. With no node answering, the request fails after at most about a minute per configured node.
  Detail: PR #516; CRD ch.36, status note after R36.14.33.

#### ZEC

- **ZEC can be sent again.** With the version-5 coins entry, withdrawals, DEX fees, swap payments, claims and
  refunds of ZEC are built as version-5 transactions with the version-5 transaction id and signature, as the Zcash
  network requires since NU7. A Trezor refuses a version-5 ZEC transaction with a clear error. Incoming version-5
  transactions are read. Detail: PR #404; CRD ch.39 R39.11.15 and §39.11.4V status notes.
- **Version-6 ZEC transactions are read.** The Zcash network accepts version 6 since NU7. The node now reads an
  incoming version-6 transaction, with the transaction id of the Zcash library (version 5 stays on the node's own reader,
  and a test checks that both readers give the same id and bytes). History rows, the input amounts of a transaction that
  spends a version-6 output, validation of the counterparty's DEX fee and payments, spend detection and secret extraction
  all work on it, where the transaction was skipped or the step failed before. A transaction that cannot be read by
  either reader is still reported by the history. Detail: PR #422; CRD ch.39 D39.11.4V.3 status note of 2026-11-18.
- **A resumed version-2 swap of a Zcash-family coin checks the counterparty's signature against the right data.**
  The signature digest of a swap step received from the counterparty now commits to the amount, the network's branch id
  and, for version 5, the script of the output it spends, for KMD, ZEC and the other Zcash-family coins. Version-2
  swaps run only when a stored version-2 swap is resumed. Detail: PR #404.

#### Browser (WASM) build

- **A cancelled storage request no longer aborts other writes of the same transaction.** In the
  browser build, if the task waiting for a storage request was cancelled before the request finished,
  the browser later called a handler that no longer existed, which made the browser abort the whole
  storage transaction, including other writes in it. The waiting code now withdraws its handlers when it
  finishes or is cancelled, and a finished transaction withdraws its own. Detail: PR #376; CRD ch.26 R9A.

- **Test code of the coin and node layers is no longer shipped in the browser build.** The browser test
  modules of `coins` and of the node library (`mm2_main`), and the test helpers only they use, were
  compiled into the browser build, and the browser test library was a normal dependency of eight crates.
  They are now built only for tests, and the test library is a test-only dependency of `coins` and
  `mm2_main` and no longer a dependency of the six crates that do not use it. The release browser bundle
  (`kdflib_bg.wasm`) shrinks from 23,005,576 to 22,253,053 bytes. The browser tests of `coins` are the
  same 24 as before, test for test (18 pass, 5 fail, 1 hangs, as before). The browser tests of `mm2_main`
  and `mm2_gui_storage` did not compile before and do not compile now, with the same errors.
  Detail: PR #391.

- **Test code of the storage and network layers is no longer shipped in the browser build.** The browser
  test modules of the storage layer (`mm2_db`) and the network layer (`mm2_net`) were compiled into the
  browser build and into the test build of every crate that uses them, and the test library they use was
  a normal dependency of `mm2_net` and `mm2_db`. They are now built only for tests, and the test library
  is a test-only dependency of these two crates. The release browser bundle (`kdflib_bg.wasm`) shrinks from
  23,196,199 to 22,963,595 bytes. The browser test run of `coins` now runs only the tests of `coins`. Detail: PR #389.

- **The browser demo page loads the current WASM build again.** The page `wasm_build/index.html`
  imported a module file name (`mm2.js`) that the build no longer produces, so it could not start;
  it now imports `kdflib.js`. The build guide and the page's README name the files the build writes,
  and the build output directory `wasm_build/deps/` is ignored by git. Detail: PR #374; CRD ch.2 directory table (lists `wasm_build/`).

#### UTXO coins and Electrum

- **A swap no longer times out waiting for confirmations on an Electrum server that has fallen behind or keeps
  answering errors, and a taker whose payment the maker already claimed now finds that claim on any of the coin's
  servers.** A swap's wait for its payment's confirmations polls one server. It used to stay with that server until
  the wait's deadline unless a request to it failed outright, so a server that stayed connected but fell behind the
  chain, or answered every request for the transaction with an error, made the swap step time out even when the
  payment had long been confirmed. The wait now moves on to an up-to-date server when its server falls behind, and
  after three error answers in a row; each move is logged at INFO level with both servers. When such a timeout sends
  a taker into its refund chain, the chain looks for the maker's spend of the taker payment, which reveals the secret
  needed to claim the maker payment. If that look finds nothing or fails, it now asks every server of the coin before
  it goes on with the refund, so that the maker payment can still be claimed in time. Both apply to UTXO coins on
  Electrum; the first only while `electrum_tip_checks` is `true` (the default). Detail: PR #461; CRD ch.38 R38.6.4o
  item 6 and ch.51 R31a, status notes of 2026-11-21.
- **An Electrum server is no longer marked as stuck just because the next block was slow.** A
  server counted as stuck when it had sent no new block for a while (at least 10 minutes) while
  another server had. After any long gap between blocks, every server except the first one to
  report the new block was therefore marked stuck for about a second, which also demoted it and
  started a new measurement of the servers. In a 72-hour run with four nodes, 405 of 411 such marks
  cleared within one second. A server now counts as stuck only when another server has reported a
  block this one has not, for longer than two block intervals and at least 180 seconds (10 minutes
  for a coin without a block time in its coins entry). A server that keeps sending notifications
  without reaching that block now counts as stuck too. A coin with a single connected server keeps
  the old rule. Detail: PR #378; CRD ch.38 R38.6.4o item 4.
- **KRGN swaps can claim and refund over Electrum.** Kerrigan headers are 80, 124, 543 or 1487 bytes long,
  depending on the mining algorithm, and two of the four layouts carry the block time 32 bytes later than the
  standard header. The node could not read them, so the median block time a claim or refund needs was almost
  always unknown, and KRGN claims and refunds failed with an error, including refunds through
  `recover_funds_of_swap`. With `"chain_variant": "KRGN"` in the coin's entry, each header is now read by its
  algorithm. A node connected to a Kerrigan daemon instead of Electrum was not affected. Detail: PR #468; CRD ch.37
  §37.5.11.

#### Transaction history

- **DASH and Kerrigan history rows of special transactions carry the chain's transaction id.** A DASH coinbase
  payout, a masternode provider transaction, a Platform withdrawal (asset unlock) or a Kerrigan Sapling transaction
  carries an extra payload after its lock time. The node ignored that payload, so it computed an identifier the chain
  does not know and showed it as the history row's `tx_hash` and `internal_id`. The payload is now read and kept,
  and the identifier is the chain's. A special transaction whose payload is cut short is refused as malformed instead
  of being read without it. Ordinary transactions are unchanged. Detail: PR #474; CRD ch.38 R38.6.10.5.

- **The history of a UTXO coin says which transactions it skipped.** A transaction the node cannot read is left out of
  the history, as before, and tried again on the next pass. Until now nothing said so: the history reached `Finished`
  as if it were complete. `sync_status` now lists each skipped transaction in
  `additional_info.unreadable_transactions`, with its `tx_hash` and a short `error`. This includes transactions skipped
  because an input transaction cannot be read. When nothing is skipped, the answer is unchanged. The BCH and SLP
  history also no longer pauses at a transaction that does not parse; it skips it and stores the rest. Detail: PR #415;
  CRD ch.39 D39.11.4V.1.

- **`my_tx_history` pages a stored history with the right totals.** For a coin whose history is
  served from the store (BCH and SLP tokens, and now Tendermint coins), the versioned method
  reported the page size as `total`, and a request with `from_id` could return an empty page; it now
  returns the store's total, the right `skipped` count and `total_pages`. Detail: PR #363; CRD ch.53
  R53.2.4, R53.2.8.
- **A history store written by this node is now listed by the other implementation for every coin
  the store serves (as the cleared rule states; checked in the lab for Tendermint stores and for a one-transaction BCH store, not yet for SLP tokens).** The SQLite history store keeps, beside the history rows, a small table of the
  addresses each row concerns, and the other implementation lists only the rows that have entries
  there. Until now this node wrote that table for Tendermint coins only, so a BCH or SLP-token
  history database it had written was listed empty (or incomplete) when the same database was
  opened by the other implementation. The table is now created and filled for every coin that
  opens the store, rows already stored are given their entries when the BCH/SLP fetcher starts, and
  entries whose row is gone are removed. The listing of BCH and SLP tokens, and every stored row,
  are unchanged. The SLP token rows rest on unit tests; no live SLP row is run, because the coins file
  the lab uses has no SLP token entry; the live store comparison covers BCH alone. Detail: PR #380;
  CRD ch.36 R36.14.23 -- R36.14.26.
- **Stale history rows are removed, and changed heights are taken over.** A stored row whose transaction the servers
  no longer list was kept forever in the history of UTXO coins, Qtum and QRC20 tokens, and kept once confirmed for BCH
  and SLP. Now a row is removed when two successful listings in a row, at least one block apart, from servers that are
  up to date, do not contain it. All rows of one transaction go together. A failed or cut-off listing, a listing from
  a server that is behind, and any listing while the servers disagree about the chain remove nothing. One log line
  reports each removal. A transaction listed at another height takes that height and its block's time. Detail: PR #408;
  CRD ch.38 §38.5C, D38.5C.1 and D38.5C.2.
- **The transaction history reacts to a reorganization of the chain at once.** Each coin's Electrum client now notices
  when the chain its servers follow changes: on every new block it compares the recent block headers of an up-to-date
  server with the ones it already holds. When they differ, the history collectors run at once instead of waiting for a
  balance change, and one WARN line names the height from which the chain changed. At most one small request is made
  per new block. Detail: PR #409; CRD ch.38 §38.5C (the amendment after D38.5C.2).

#### Solana coins

- **Solana balances are shown exactly.** `my_balance` and the activation results rounded a SOL balance to
  nine significant digits, so a balance of 10 SOL or more lost its last decimals (12.345678901 SOL read
  12.3456789) and a zero balance read `0.00000000`. SOL and token balances are now the exact amount
  without trailing zeros (`0`, `1.5`, `12.345678901`); token balances are computed from the raw amount
  the node reports. Withdraw values were never rounded; the text of their sums is in the next item.
  Detail: PR #456; CRD ch.40 §40.2A (R40.2A.1, R40.2A.5), R40.2.3, R40.3A.2, T40.6B.7.
- **Sums in Solana withdraw answers carry the coin's full decimals.** Members that add or subtract two
  amounts (`spent_by_me` and `my_balance_change` of a SOL withdraw, `my_balance_change` of a token
  withdraw, a token withdraw's `fee_details.total_amount`, and the `required` member of a SOL
  `NotSufficientBalance` and of `NotSufficientPlatformBalanceForFee`) are written with 9 decimals for SOL
  and the mint's decimals for a token, for example `1.000000000` or `-1.500000`. The values are unchanged;
  single figures such as `total_amount` and `fee_amount` stay without trailing zeros. Detail: PR #456;
  CRD ch.40 R40.2A.4, D40.2A.1.
#### QTUM and QRC20

- **A QTUM delegation is no longer built at the same time as a withdraw.** Building a delegation
  (`add_delegation`) asked for the lock that lets only one transaction from the wallet be built at
  a time but did not wait for it, so the lock was never held, and removing one
  (`remove_delegation`) did not ask for it at all. A delegation and a withdraw started together
  could therefore be built at the same time. Both delegation calls now wait for the lock, as a
  withdraw does, so the builds no longer overlap; as with two withdraws, a transaction that is built
  but not yet broadcast does not keep a later build from choosing the same coins. Detail: PR #354; CRD ch.38 R38.5B.5.
- **QTUM and QRC20 unspent lists and sends now go to the same first Electrum server.** With read
  spreading on (the default), a list of the wallet's unspent QTUM coins could be answered by a server
  that had not yet seen a transaction this node had just sent from a QRC20 token, and the node keeps
  no record of what such a transaction spent. The next send could then choose a coin that is already
  spent and be rejected. QTUM and each token also rank the servers separately. For QTUM and its
  tokens, the unspent list no longer takes turns, and the list and every send now go to the servers
  in the order of the coin's server list (servers that are behind come last), whatever
  `electrum_server_order` says, so they start at the same server. Their other reads still take turns,
  and other coins are unchanged. This narrows the gap and does not close it: a server can still
  answer before it has seen the send, and a token that lists other servers than QTUM does not share
  its first server. QTUM and its tokens now also share one set of connections and one record of
  spent coins; see the two items below. Detail: PR #368; CRD ch.38 R38.6.4a.
- **A QRC20 token keeps working when one of its Electrum servers answers a contract request with an
  error.** Some public QTUM servers answer the token balance, history and receipt requests with an
  internal error. One such answer used to fail the activation, the balance read or the history
  request, although another listed server would have answered. The request now moves on to the next
  server, without dropping the connection, and fails only when every server tried has answered with
  an error. These three requests now also take turns among the servers, as other reads do. Detail:
  PR #417; CRD ch.38 D38.6.4Q.2.
- **QRC20 token history no longer misses a transfer whose details a server failed to deliver.** Such
  a transfer was left out, and the history was not asked for again until the token balance changed.
  The history is now asked for again after a short delay, which grows while details keep failing.
  Detail: PR #417; CRD ch.38 R38.6.4Q.6.
- **QRC20 token history now stops with a clear status when no listed server serves it.** When three
  history passes in a row get an error answer from every server tried, the token's history stops,
  and `my_tx_history` reports it in `sync_status` with code `-2`. Activating the token again starts
  it again. Passes that fail because a server cannot be reached never count, so a node that is
  offline keeps its history. Detail: PR #417; CRD ch.38 amendment after R38.6.4Q.6.
- **A QRC20 token's balance subscription is placed again after a reconnect.** With the balance
  stream on, the token's subscription was lost when its server connection dropped, and the token
  fell back to polling. It is now placed again once on the new connection; a server that refuses it
  changes nothing else. Detail: PR #417; CRD ch.38 D38.6.4Q.3.
- **Disabling a QRC20 token now ends its history work at once.** The token's history kept running
  for up to about 40 s after `disable_coin`, and enabling the token again within that time left two
  of them running. It now ends when the token is disabled, and exactly one runs after the token is
  enabled again. Detail: PR #417; CRD ch.38 D38.6.4Q.5.
- **QTUM and its QRC20 tokens now share one set of Electrum connections when their server settings
  match.** Until now QTUM and each activated token opened their own connection to every server, so a
  server held one session per instance. Instances that list the same servers in the same order and
  have the same `min_connected`, `max_connected`, `listed_order_for_spends` and `block_interval` now
  use one set of connections; any difference keeps a separate set, as before. A token still
  activates without QTUM and keeps working after QTUM is disabled, and disabling one of them, or a
  failed activation, closes no connection the others use. A subscription of a disabled instance is
  no longer placed again after a reconnect unless another instance still uses it. A token that
  would join a set whose servers disagree about the chain is refused with the same error as a new
  activation. On a shared set, activation errors, transport error texts, log lines and transport
  metrics name the platform (for example `QTUM`) also for a token, and `electrum_servers` for a token
  reports the shared set. Detail: PR #427; CRD ch.38
  D38.6.4Q.1 and its status note.
- **QTUM and its QRC20 tokens no longer pick a coin that another of them has just spent.** QTUM and
  each token kept their own record of the coins they had spent, and a token recorded nothing, so a
  QTUM send right after a token send (a swap payment, a fee, a token withdraw that was broadcast) from
  the same address could pick the same coin and be rejected; two token sends could do the same. QTUM
  and its tokens now keep one record per address: every transaction the node built from that address
  and the network accepted is entered, and the next one uses other coins or the change. Token
  withdraws and QTUM delegations are entered when they are broadcast with `send_raw_tx`. A token
  send, and a withdraw or delegation of QTUM or a token, no longer waits for the builds of other UTXO
  coins, and theirs no longer wait for it. Under an
  HD login, QTUM's enabled address and the tokens' address are different addresses with separate
  records. Detail: PR #436; CRD ch.38 D38.6.4Q.4 and its status note.
- **A spend right after another spend from the same address no longer fails when a server has not
  yet seen the first one.** This applies to every UTXO coin that checks the maturity of its coins
  (QRC20 tokens always, QTUM by default, other coins with `check_utxo_maturity`). The next spend uses the
  change of the first transaction, and the maturity check fetched that transaction from one of the
  servers; a server that had not seen it yet could answer with an error, and the spend then failed. The
  node built that transaction itself, so it cannot be a coinbase output, and the check no longer
  fetches it. Every other coin is checked as before. Detail: PR #436; CRD ch.38 status note
  after R38.5B.4.
- **A disabled QTUM or QRC20 instance no longer comes back into the subscriptions of the shared Electrum connections,
  and a subscription lost on a reconnect is placed again when it is asked for.** Two cases left the shared set of
  connections of QTUM and its tokens with a wrong record of its subscriptions. A subscription request that was still
  waiting for its answer when its instance was disabled, or one sent later by a balance stream of the disabled
  instance, recorded that instance again, and its subscription was then placed again after every reconnect. And when
  a subscription could not be placed again over a new connection, it still counted as placed, so a later request for
  it sent nothing. Now a disabled instance records nothing, and a subscription is skipped only while it is placed
  over the current connection. One instance sends at most one request per subscription at a time. Detail: PR #473;
  CRD ch.38 status note of 2026-11-22 after D38.6.4Q.1.
- **The Electrum client no longer sends a second subscription on the same connection, and no longer forgets a
  subscription that a reconnect placed while a request for it was failing.** After a connection is opened, the client
  places its remembered subscriptions on it again. That step also sent a subscription that a coin had just placed
  on the same connection itself, so the server got it twice; this could happen right after activation. And when a
  coin's first request for a subscription was still waiting while the connection changed, the client placed the
  subscription on the new connection; if the waiting request then failed, the client forgot the subscription
  although the server held it, and it was not placed again after later reconnects. Now a subscription that is
  already placed on the current connection is not sent again, and a failed first request keeps the subscription
  when it is placed on the current connection. This applies to the address and token balance subscriptions of
  UTXO coins, QTUM and QRC20 tokens on Electrum. Detail: PR #520; CRD ch.38 status note of 2026-11-23 after
  D38.6.4Q.1.

#### TRON coins

- **A TRON withdrawal whose `fee` is `{"type":"Tron"}` is now refused with `InvalidFeePolicy`.** Before, such a
  request failed parsing, for example when the `fee_details` of an earlier TRON result was sent back as `fee`.
  TRON still works out its own fee and takes no `fee`. Every other coin treats this form as it treats any fee
  form that is not its own. Detail: PR #455; CRD ch.21 R-WP6.

- **Methods that work only for EVM coins now answer "not supported" for a TRON coin at once.** A TRX or TRC20
  coin has no EVM node, yet seven methods asked its TRON nodes in the EVM format and failed with an error
  that looked like a node outage. Each now refuses a TRON coin with a typed error before any request to a
  node: `get_raw_transaction` and `sign_raw_transaction` with `NotImplemented` (HTTP 501, and
  `sign_raw_transaction` no longer waits for a nonce first), `get_eth_estimated_fee_per_gas` with
  `CoinNotSupported` (HTTP 501), `stream::fee_estimator::enable` with `NotSupportedFor` (HTTP 501, no streamer
  is started), `get_token_info` for an `ERC20` protocol on a TRON platform with `UnsupportedTokenProtocol`
  (HTTP 400), and, for a TRON coin without a swap contract, which is every TRON coin while TRON swaps are off,
  the legacy `get_trade_fee` and `trade_preimage` with that coin on the receiving side (`InternalError`), both
  with the text "the coin has no swap contract: it cannot trade". EVM coins are unchanged. Detail: PRs #469,
  #506; CRD ch.21 §21.11 (D7).

#### Swap and order records

- **A crash or power loss while a swap or order record is being saved no longer damages it.** The saved record of a
  legacy swap, its statistics copy and the saved record of an order were rewritten in place, so a crash in the middle
  of a save could leave an empty or cut-off file, and the swap could not be resumed from it. Each save now writes a
  new file next to the record, syncs it to disk, replaces the record with it and syncs the folder, so the record
  holds either its previous or its new complete content. Where a folder cannot be synced (some network mounts), the
  save still succeeds and logs a warning. On Windows a replace refused because another program, such as a virus
  scanner, holds the new file is retried briefly. A temporary `*.tmp` file left by a crash is ignored. Browser builds
  already saved each record in one storage transaction and do not change. Detail: PR #480; CRD ch.51 R3, ch.44
  R44.8A.6 and §44.9A (status update).
- **A swap run whose swap lock was taken over now stops.** Each swap is run by one runner, which holds a lock file
  and refreshes it every 30 seconds. A runner that stalled for more than 40 seconds while another runner took the
  lock over kept running and refreshing, so two runners could work on one swap, and the first one deleted the
  other's lock when it ended. A runner now checks before each refresh that the lock file still holds the value it
  wrote last. When the file is gone or holds another value, or when refreshing has failed for longer than 40
  seconds, the run is stopped where it stands, like an abrupt stop, and leaves the lock file. The swap is then
  handed to the ordinary start of a saved swap, which waits 40 seconds and resumes it if no other runner holds the
  lock. A late refresh that finds its own value goes on with a warning. A new lock file is created in one step and
  replaced in one step, so a reader does not see it empty (on a file system without hard links a new lock file is
  empty for a moment), and its content keeps the form GLEEC KDF reads. Detail: PR #480; CRD ch.51 R5 and ch.44
  §44.9A (status updates).

#### Legacy swaps

- **The claim or refund of an SLP swap payment is found again.** When the node looked for the claim or refund of
  an SLP token's swap payment, before a refund or in `recover_funds_of_swap`, it compared the wrong output of the
  payment with the swap's script, so every such search stopped with an error (since 0.3.0-beta.1). It now compares
  the output that holds the swap's script: output 1 for SLP tokens, and output 0 for other UTXO coins, as before.
  Detail: PR #534; CRD ch.51 R90.
- **A spend of a UTXO swap payment is read from the input that spends it.** When the node looks for the claim or
  refund of a swap payment (UTXO coins, BCH, QTUM, SLP tokens), it now reads the input of the spending transaction
  that spends the payment. The same applies to the search of a version-2 swap's funding transaction. Detail: PR #481;
  CRD ch.51, status note of 2026-11-22 after R31a.
- **The secret of a swap is checked against its hash on every coin.** When the taker, or a watcher acting for it
  (once a watcher receives swap data), reads the secret from the transaction that claimed the taker payment, the
  value is used only when it matches the swap's secret hash. EVM coins (ETH and ERC20 tokens) also read the secret
  from the swap contract's claim event; Tendermint coins look at every message of the transaction. Detail: PR #488;
  CRD ch.51, status note of 2026-11-22 before R31.
- **A swap's payment step stops with an error when the check for an earlier payment cannot decide.** Before a swap
  sends its payment, it checks whether it already sent one. For EVM coins and tokens, QRC20 tokens and Sia, an
  answer that cannot decide is now an error that names the condition, and the payment step stops as it does for any
  other error; the manual recovery answers the same error. Detail: PR #489; CRD ch.39 §39.11.5A.
- **A swap resumed at its payment step repeats a check for an earlier payment that cannot decide.** A swap that a
  restart resumes right before its payment repeats the check within a bound, and then waits without sending until
  the payment is found or the refund deadline passes. A payment it finds is taken over and the swap continues. A
  swap that was not resumed at this step checks once, as before. Detail: PR #489; CRD ch.39 §39.11.5A.
- **The refund of an SLP, QRC20, ETH or ERC20 swap payment waits for the chain's time.** The node asks whether a
  payment can be refunded from the chain: an SLP token by the median time past of BCH, a QRC20 token by that of
  QTUM, and an ETH or ERC20 coin by the timestamp of the latest block, once the node's own clock is past the lock
  time. Detail: PR #490; CRD ch.51, status note of 2026-11-22 after §51.6E R92.
- **A QRC20 swap's spend or refund is recognized only from its own successful contract call.** The search ties the
  swap event to the contract call it belongs to, requires that call to the swap contract to have succeeded, and
  looks for a refund in the history of the payment's sender, so a refund of the other side's payment is found too.
  Detail: PR #496; CRD ch.51, status note of 2026-11-22 after §51.6E R92.
- **The taker's refund waits while the chain's answer about its payment is undetermined.** Before it refunds, the
  taker looks for a claim or a refund of its payment. On every coin, while a look fails or does not finish, and on
  a coin whose reads need two agreeing nodes also while they do not agree, the refund waits and the look repeats,
  with one warning. The look also runs each time the refund becomes possible, just before the refund is built. A
  claim found on one server whose secret matches the swap's hash is used at once; a refund, or another spend, found
  on one server only is not acted on. A swap still waiting an hour after its refund first became possible in the
  current run, and at least until its payment's lock time plus 3700 seconds, ends as refund failed, with a text
  that names the kind of failure and, when the nodes did not agree, the fix; the servers' own answers go to the
  debug log only. Detail: PR #523; CRD ch.51, status note of 2026-11-23 under R31a.
- **The maker's refund, the watcher and the payment checks wait on an undetermined answer.** The maker's refund
  waits while its look at the taker payment, or the check whether the refund is possible, does not give a
  determined answer, with the same bound and the same kind of text as the taker's. A watcher refunds the taker
  payment only after a search that answered that the payment is not spent. When the check of the other side's
  payment cannot be determined, the maker and the taker repeat it until the step's existing deadline instead of
  giving up at once. The manual recovery names the fix when its read is undetermined. Detail: PR #527; CRD ch.51,
  status notes of 2026-11-23 after the description of the maker's validation stage, after R28 and at the end of
  §51.6C; ch.09, status note of 2026-11-23 after §9.6.

#### Shielded coins

- **ARRR can spend the notes on the wallet's change address, and the balance it shows can be
  spent.** In Light mode, ARRR notes on the wallet's change address (the internal address of the
  same seed), such as the change that a Pirate wallet on the same seed makes, are now spent like any
  other note: a withdrawal, a withdrawal to a `pirate1` address and a swap payment can use them. A
  shielded spend signs each note with the key of the ZIP-32 scope the note belongs to. `my_balance`
  counts these notes as before, and the amount it shows can now be spent: a withdrawal of the
  maximum spends exactly that balance. KDF Reloaded's own change goes to the wallet's external `zs1`
  address and was not affected. Nothing needs to be done: no rescan and no new activation, since the
  wallet database already holds these notes. Detail: PR #542; CRD ch.39 R39.8.0ag1 (status note of
  2026-11-25).

### Tests

- **A Tendermint history test no longer hangs when the whole `coins` test suite runs in parallel.** Once a test
  stopped a scripted node, its port was free, and another test could be given it; a request to the stopped node
  then reached a listener that never answered. On Linux a stopped scripted node now keeps its port until the test
  ends, so connections to it are refused. No production code changes. Detail: PR #513.
- **The `get_private_keys` warning tests no longer fail when the whole `coins` test suite runs in parallel.** The
  test log capture is one list for the whole process, and another test writes the same warning line, so the line
  counts of the per-call warning test were sometimes off. Each captured line now records the thread that wrote it,
  and the two tests that count lines count only their own. No production code changes. Detail: PR #507.
- **The Trezor emulator tests now run in CI.** The emulator job passed while every test was skipped: the
  container starts only the emulator's controller, and each test looked for the emulator before anything had
  started it. The job now starts and seeds the emulator and checks that it answers before the tests run, and in
  that job an emulator that does not answer fails the tests instead of skipping them; run without that job's
  setting, the tests still skip. The emulator image is pinned to a fixed version. Of the eleven tests, ten run and
  pass; the PIN test stays ignored. The TRON test had never run and failed in its own setup; only its setup
  changed. Detail: PR #498.
- **Seven Trezor emulator tests of `task::get_new_address`.** They run in the emulator job, where an emulator that
  does not answer fails them; six run and pass there. The test of a refusal on the device is ignored: the emulator's
  screen that shows a new address has no cancel button, so that check stays a manual one on a Trezor.
  Detail: PR #491.
- **The legacy taker-swap unit tests now run in CI.** The 74 unit tests of the legacy taker swap (mocked coins, no
  network) run in the unit-test job with the maker-swap tests; all pass. The tests themselves are unchanged.
  Detail: PR #479.
- **The browser tests run again locally.** `wasm-pack test --firefox --headless mm2src/<crate>` finds and
  runs the tests of the browser build again; before, the test runner found none ("no tests to run!"),
  because the locked test library was far older than the locked `wasm-bindgen`. The test library is
  updated to the matching release and the runner name in `.cargo/config.toml` is the standard one. This also
  updates `wasm-bindgen-futures`, which the browser build uses at run time, to 0.4.67. No CI job runs the
  browser tests yet. Detail: PR #373.
- **The browser tests of `mm2_main` now compile.** `wasm-pack test --firefox --headless mm2src/mm2_main`
  stopped at 52 compile errors, because test code that only works natively (tests written for the `tokio`
  runtime, tests that create random identifiers with a function the browser build does not have, and
  tests that use a native-only database helper) was also built for the browser. That test code is now left
  out of the browser test build, as is the same kind of test code in `mm2_gui_storage`; only test code
  changed. The run finds 12 tests: one fails without the `BOB_PASSPHRASE` test setup it asks for, one (`test_qrc20_tx_history`, which starts a node on network id 9998) hangs and is skipped, and the other ten, the storage tests of orders and swaps, all pass; the list by name is in `docs/DEV_ENVIRONMENT.md`. No CI job runs the browser tests yet.
  Detail: PR #403.
- **The browser tests of `coins` now compile.** `wasm-pack test --firefox --headless mm2src/coins` stopped at
  compile errors, because some test code that only works natively (tests written for the `tokio` runtime and the BCH
  tests that use the SQLite history storage) was also built for the browser, and a test helper
  was built natively only. The native-only tests are now left out of the browser test build and the
  helper is built there too; only test code changed. The `coins` test build runs 44 browser tests: 24 of `coins` itself (18 pass, 5 fail, 1 hangs) and 20 that it
  links in from `mm2_db` (16, all pass) and `mm2_net` (4: 3 pass, 1 fails), whose browser test modules are not limited
  to test builds.
  Three failures and the hanging test need a public node (an Ethereum node, two Electrum
  servers, an SLP node). The other three failures are an NFT store test that hits a database
  constraint error, an NFT store test that reads back nothing, and the linked-in WebSocket test of `mm2_net`
  that expects an error for an invalid address. The hanging test stops the whole run, so it has to be skipped with
  `-- --skip`. The failures are listed in `docs/DEV_ENVIRONMENT.md` and are not fixed here.
  No CI job runs the browser tests yet. Detail: PR #388.
- **The Tendermint unit tests of `coins` now run in CI.** A new CI job runs the Tendermint swap-flow,
  priority-fee and WalletConnect signer tests and the postponed-family switch tests; the loopback
  tests of the native RPC server join the maker-outbox job.
- **The Electrum connection-pool tests and the offline QRC20 tests now run in CI.** No CI job ran them
  before. A new job runs 203 tests: those of the Electrum client, pool and measurement modules and
  the offline QRC20 tests (about 10 s of test time); 32 more need public servers and are skipped. Five QRC20 tests that turned out to need a public
  testnet server are now marked as such and are skipped without network access, like other tests
  that need public servers. The QTUM regtest image of the docker tests is now pinned to a fixed
  version, so a re-published image cannot change what the tests run against. The three docker tests
  that withdraw on public testnet servers no longer fail when two of those servers briefly disagree
  about the chain: they turn the server height checks off, which they do not test. Detail: PR #377.

- **Offline tests now pin how a QRC20 token behaves over Electrum.** A local stub Electrum server
  now answers the QRC20 contract methods. Twelve new tests cover token activation, a balance read
  that a server answers with an error, the history collector's requests and its failures,
  including the server's "history too large" answer. The five QRC20 tests that needed the public
  Qtum testnet server now run against the stub, offline, in CI. Detail: PR #385; CRD ch.38
  §38.6.4Q.

- **More offline tests pin how QTUM and its tokens share servers and coins.** Fifteen tests cover:
  - the connections QTUM and two tokens hold;
  - what disabling a token or QTUM stops;
  - the token event subscription after a reconnect;
  - spends of the shared QTUM coins by QTUM and its tokens;
  - delegation, and the lock that orders withdraws.
  Some of them wait in real time for the history collector's 30-second cycle. Detail: PR #406; CRD ch.38 §38.6.4Q.

- **The node tests of `mm2_main` that need no public server now run in CI.** These tests start real nodes
  on loopback addresses and talk to them over RPC; no CI job ran them as a group before. A new job runs 15 of
  them: start-up, the RPC password, mmrpc 2.0 errors, batch requests, the passphrase forms, the public key,
  swap status, the order book ticker rule, the bot start and stop, and the start on an old database folder.
  Six of them needed a public server, a passphrase variable or a network id before and were changed to need
  none; the nodes of the tests no longer ask a public time server at start. Nine tests were removed: three
  exact copies, four that need a native ZOMBIE daemon, which no CI runner has, and two whose checks already run
  offline in CI. The other node tests still need public servers and stay off by default; the two that were ignored
  without a reason now state it. Only test code changed. Detail: PR #453.
- **Two tests of the shared QTUM Electrum client no longer depend on the moment their holder count is taken.**
  They compared the number of holders of the client right after activation with the number at the end. The
  client's maintenance pass also holds it while it runs, so a count taken during the first pass could be one
  too high. Both counts are now taken once they have stayed the same for 2 s and are compared for equality, so
  a handle that a failed token activation keeps still fails the test. Only test code changed. Detail: PR #460.
- **Tests of a repeated QTUM or QRC20 activation.** A second activation of an active QTUM or QRC20 token, one
  after the other or started together, answers "already initialized" and starts no second connection,
  subscription or history collector. Only test code changed. Detail: PR #462.
- **Tests of a maker restart before the outcome of its claim is saved.** A maker swap restored at the step that
  claims the taker payment claims again; when that send fails, or when the claim time is over and nothing is sent,
  the refund step still looks at the taker payment first and does not refund once it finds the maker's own
  confirmed claim. When nothing spent the taker payment, the refund goes ahead as before. The design chapter
  also states why a recorded but lost claim is answered differently by the automatic refund and by the manual
  recovery. Only test code and docs changed. Detail: PR #464; CRD ch.51, status notes of 2026-11-21 after
  R82 and R83, T35.
- **The test code of the coin activation compiles for wasm32, and CI checks it.** The task-framework tests of the
  standalone-coin family are built only for native targets, like the test helpers they use. A new CI job,
  `wasm32-test-check`, compiles the test code of `coins_activation`, `coins` and `mm2_main` for wasm32 and is part
  of the required `tests-passed` check. Only test code and CI changed. Detail: PR #511.
- **The Windows CI jobs install a pinned protoc.** After a package-manager install of protoc, the build script of
  `coins` at times did not find protoc. The Windows test job and the Windows build now download protoc 27.3, check
  the archive's SHA-256, set `PROTOC` to its path and check the version before the build. Only CI changed.
  Detail: PR #517.

### Plans touched

- [`swap-v2-chain-read-agreement`](docs/plans/swap-v2-chain-read-agreement.md) — new, planning: swap protocol v2
  follows the agreement of chain reads (R100) in 0.5.0, with the recovery and the funding-refund rules decided on
  2026-11-23, PR #528.
- [`swap-v2-state-and-abort-reason-naming`](docs/plans/swap-v2-state-and-abort-reason-naming.md) — new, planning
  (after 0.5.0): a decision, with wallet maintainers, on renaming or adding swap protocol v2 states and abort
  reasons, then compatibility, migration of stored rows and docs; PR #531.
- [`swap-recovery-retry-switch`](docs/plans/swap-recovery-retry-switch.md) — new, planning (v0.6): a decision,
  after a discussion with experienced users, on whether an operator or frontend switch retries the recovery of
  swaps that end needing manual recovery; added with the R100 item 9 note, PR #518.
- [`tron-swap-support`](docs/plans/tron-swap-support.md) — note of 2026-11-22: the TRON arm of the secret-hash
  selection is built, in PR #499.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — in progress: P9, the lab rows, all passed;
  the closing note names what no row covers, in PR #497.
- [`swap-chain-reading-correctness`](docs/plans/swap-chain-reading-correctness.md) — in progress: step 1 in PR #481,
  step 2 in PR #488, step 3 in PR #490, step 4 in PR #496.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — in progress: Phase C item 5, the INFO
  lines for a shared QTUM Electrum client, in PR #476.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — in progress: P8, the subscriptions of the
  shared QTUM Electrum client after a disable or a failed placement, in PR #473.
- [`swap-confirmation-wait-failover`](docs/plans/swap-confirmation-wait-failover.md) — new: steps 1 and 2 (the
  confirmation wait leaves an out-of-date or erroring Electrum server; the refund chain's look asks every server) in
  PR #461; step 3, the lab row, is listed there.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — in progress: P7, one record of spent coins
  and one lock per address for QTUM and its tokens, in PR #436; Phase E items 1 and 2 are covered.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — in progress: P5, one Electrum client for
  QTUM and its tokens, in PR #427; the Phase C items it leaves open are named under Phase C.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — in progress: Phase A (reproduce and measure)
  done in PRs #385 and #406; its gate is met; the balance stream's in-node subscription test skipped, with the reason in
  the plan.
- [`hd-key-unification`](docs/plans/hd-key-unification.md) — the H7 row and the note on the legacy-route decision now
  say that H7 applies to an absent `priv_key_policy` or `ContextPrivKey` only, and that `IguanaPrivKey` stays on the
  internal key in every family; shielded coins read `priv_key_policy` as well (H7 row note of 2026-11-18).
- [`browser-build-gaps`](docs/plans/browser-build-gaps.md) — new, planning: the gaps of the browser (WASM)
  build outside the shielded coins — versioned history, wallet management, event delivery to the page and
  smaller items — in order of cost, and the limits a browser has by nature, also listed in
  `docs/WASM_BUILD.md`. It now also lists the browser tests and lint (a CI job, the failing browser tests,
  the browser-target lint) and the three probe fixes merged since it was written.
- [`mmrpc-v2-completion`](docs/plans/mmrpc-v2-completion.md) — in progress (0.5.0): phase 1 (order
  management) implemented in PR #359, phase 2 (trading) in PR #372, phase 3 (wallet and coins) in PR #379,
  phases 4 and 5 (swaps; daemon and network) in PR #387; decisions D1–D10 recorded; the extension is complete except
  `recover_funds_of_swap`.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — in progress (was parked):
  checked against the code on 2026-11-15: phases A–F not started; the state found and the order of the work packages are in its
  new section "State check (2026-11-15)".
- [`rpc-error-redesign`](docs/plans/rpc-error-redesign.md) — new, planning, no schedule: one error vocabulary
  (token spelling, payload kind, HTTP status) for the whole versioned RPC surface, with a compatibility period;
  ordered 2026-11-15, written in PR #359.
- [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md) — done: it records that the
  stricter signing of malformed output scripts (step 8) is done and that the plan has no open step left.
- [`shielded-coins-in-browser`](docs/plans/shielded-coins-in-browser.md) — new, planning: work packages to
  activate shielded coins (ARRR) in the browser build — the wallet database on the SQLite route, one
  parameter source for both targets, the gRPC-web transport, Worker hosting of wallet and prover, swaps
  last — with the decisions of CRD ch.39 D39.6.6.a–i as proposals.
- [`solana-sdk-upgrade`](docs/plans/solana-sdk-upgrade.md) — done: the Solana crates moved to the 3.x
  line (package S5 of `solana-support`, PR #465); RUSTSEC-2022-0093 is cleared, and RUSTSEC-2024-0344
  moves to the libp2p modernization task (it comes in through the peer-to-peer library); PR #470 clears
  it by vendoring the Noise crate.
- [`solana-support`](docs/plans/solana-support.md) — in progress: checked against the code, gaps
  corrected, packages S0 to S11 in a new order, the maintainer decisions of 2026-11-15 recorded;
  packages S0 and S2 done in PR #361, S1 in PR #371, S4 in PR #392, S3 in PRs #401 and #413,
  S5 in PR #465, S6 in PR #425, S7 in PR #426, S8 in PR #430; new package S6c (a rent-exempt guard for SOL
  withdraws); switch-on lab rows for `disable_coin` and the Token-2022 refusal.
  S9 in PR #475: on 2026-11-22 Solana was switched on as a wallet-only coin family; orders and
  swaps stay off. S10 in PR #494: the transaction history of SOL and SPL tokens.
- [`startup-clock-check`](docs/plans/startup-clock-check.md) — new, planning: the clock check against
  NTP time servers, implemented in PR #348; the open points O9 and O10 stay open.
- [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md) — item 4, the
  Tendermint transaction history, is built in PR #363; its address index now covers every
  store-served coin family (PR #380); item 1a, the WalletConnect form of the activation, is built (PR #393).
  The beta-availability note now says a saved Tendermint order is kept at startup, not cancelled.
- [`tron-support`](docs/plans/tron-support.md) — work package 3 (node access) done on 2026-11-17: the TRON node
  client has a time limit per node, fails over on bad answers, runs requests at the same time and accepts request
  headers and an API key; decisions about what lands before TRON is switched back on recorded. Work package 1
  (the activation route) is built, still switched off: the mmrpc 2.0 methods accept TRON and the legacy route stays
  (PR #405). The TRON HD wallet is built on the same route, still switched off.
  The accuracy of the withdraw fee estimate and the withdraw amounts are done on 2026-11-19, still switched off.
  The transaction history (work package 4) is built with a TronGrid API key, still switched off (PR #440).
  On 2026-11-20 TRON was switched on as a wallet-only coin family; swaps stay off.
  The plan now also says a saved TRON order is kept at startup, not cancelled, and that the GasFree rail is
  reachable.
- [`tron-gasfree`](docs/plans/tron-gasfree.md) — in progress: the bytecode blocker is resolved: the creation bytecode is taken
  from the public chain, embedded and verified against every published mainnet and Nile vector and the live Shasta
  controller; the support modules are built; the activation binding and the `gasfree_address` fields in the reports
  are built and can be used, as TRON is switched on; the gasless withdraw is built as sign-only on 2026-11-20 and
  can be used too; on 2026-11-22 the task form of the withdraw gained submission to the provider and settlement
  tracking, while the direct withdraw stays sign-only. A later step may let the broadcast call take the signed
  authorization.
- [`zec-v5-transactions`](docs/plans/zec-v5-transactions.md) — in progress (0.5.0): route (A) decided 2026-11-17;
  the version-5 format and digests in the shared parser (PR #397) and the ZEC wiring (PR #404); the history report of
  unreadable transactions (PR #415); ZEC fees under ZIP 317 (Z10, PR #431); the coins entry follows. Z1–Z8 done.

### Known limitations (additions since 0.4.0-beta.1)

- **QRC20 transaction history needs Electrum servers that answer the contract-event methods.** As of 2026-11-22, the
  QTUM servers of the published coins file answer them with an error. With only those servers, a QRC20 token's
  history stops with error code -2 after three passes in a row in which every listed server answered with an error.
  List QTUM Electrum servers that answer them, for example `s1.electrum.qtum.site:50001`, in the activation request.
  Detail: PR #417; CRD ch.38 R38.6.4Q.6, amendment of 2026-11-17.
- **TRON coins cannot be traded.** TRX and TRC20 tokens are wallet-only: activation, balances, addresses,
  withdrawals and the transaction history work, and orders and swaps do not. `setprice`, `buy`, `sell` and
  `start_swap` refuse a TRON coin with the "… is wallet only" answer. This replaces the item of 0.4.0-beta.1 that
  says TRON cannot be activated. More in [`tron-support`](docs/plans/tron-support.md) and
  [`tron-swap-support`](docs/plans/tron-swap-support.md). Detail: PR #451; CRD ch.21 R-D1, R21.B3.
- **Solana coins cannot be traded.** SOL and SPL tokens are wallet-only: activation, balances, withdrawals,
  message signing, `disable_coin` and the transaction history work, and orders and swaps do not.
  `setprice`, `buy`, `sell` and `trade_preimage` refuse a pair with a Solana coin with "Trading is not supported for
  coin …". A Token-2022 token is refused at activation. This
  replaces the item of 0.4.0-beta.1 that says Solana cannot be activated. More in
  [`solana-support`](docs/plans/solana-support.md). Detail: PR #475; CRD ch.40 D40.4.
- **Only the task form of `withdraw` submits a gasless TRC20 transfer through GasFree.** `task::withdraw::init`
  with `fee_method` `gasless` signs the transfer, hands it to the provider and follows it until it settles. The
  direct `withdraw` with `fee_method` `gasless` returns a signed authorization instead of a transaction and sends
  nothing: no RPC takes that authorization afterwards, so to move a token from the GasFree address, or back to your
  own address, use the task form. KDF keeps no record of a submitted transfer: if the node stops, the task is
  cancelled or the wait runs out, the transfer may still complete, and the provider's `trace_id` from the status or
  the error text is the way to look it up. A TRC20 withdrawal without `fee_method` pays its fee in TRX. More in
  [`tron-gasfree`](docs/plans/tron-gasfree.md).
- **The TRON transaction history needs a TronGrid API key.** Without a keyed TronGrid node, or without
  `tx_history: true` in the activation request, `my_tx_history` answers as for a coin without a history. Detail:
  CRD ch.21 §21.10C.
- **TRON on public TronGrid without an API key can be refused with HTTP 429.** In a mainnet withdrawal test with
  the keyless public TronGrid endpoint, some requests were answered with HTTP 429 (too many requests). KDF counts
  such an answer as a failed node and asks the next node of the list; when every node refuses, the method returns
  the error (for example a transport error from `withdraw`, or "TRON broadcast failed" from
  `send_raw_transaction`) and does not wait and try again. In the test, a broadcast refused this way sent nothing,
  and repeating the request later succeeded. For regular use, set a TronGrid API key (`tron_api_key`) or use your
  own full node. More in [NETWORK_CONFIG](docs/NETWORK_CONFIG.md#tron-node-list-and-api-key). Detail: PR #467;
  CRD ch.21 §21.9.
- **No watcher can refund a Siacoin (SC) payment yet.** The 0.4.0-beta.1 item "Siacoin (SC) swaps: what is not
  covered" says that a node watching other traders' swaps can refund a Siacoin payment for an offline taker. That
  overstated it: a taker hands its swap data to watchers only when both coins of the swap offer watching, and
  Siacoin is the only coin that does, so no swap hands its data to a watcher, as the 0.4.0-beta.1 item "The swap
  watcher does not work yet" says. Keep your node running until your swaps have finished. Detail: CRD ch.09 R17,
  ch.20 D7.
- **KDF Reloaded neither shows nor spends ARRR funds held in the Ironwood pool.** The Ironwood pool
  is Pirate Chain's newer shielded pool. Funds there, for example funds a Pirate wallet on the same
  seed keeps in it, stay safe and can be spent from that Pirate wallet; the balance KDF Reloaded
  shows covers the Sapling part only. A coin in native mode is still refused at activation.

## [0.4.2-beta.1] — 2026-11-26

### Changed

#### Transport, TLS and dependencies

- **The URL library moved to 2.5.9, which clears RUSTSEC-2026-0592.** The `url` crate moved from
  2.5.8 to 2.5.9, and `form_urlencoded`, which the new version requires, from 1.2.2 to 1.2.3. KDF
  Reloaded's own code does not call the affected function. The dependency policy needs no exception
  for this advisory. Detail: PR #545, the release-line port of PR #492.

### Fixed

#### Shielded coins (ARRR)

- **ARRR can spend the notes on the wallet's change address, and the balance it shows can be
  spent.** In Light mode, ARRR notes on the wallet's change address (the internal address of the
  same seed), such as the change that a Pirate wallet on the same seed makes, are now spent like any
  other note: a withdrawal, a withdrawal to a `pirate1` address and a swap payment can use them. A
  shielded spend signs each note with the key of the ZIP-32 scope the note belongs to. `my_balance`
  counts these notes as before, and the amount it shows can now be spent: a withdrawal of the
  maximum spends exactly that balance. KDF Reloaded's own change goes to the wallet's external `zs1`
  address and was not affected. Nothing needs to be done: no rescan and no new activation, since the
  wallet database already holds these notes. Detail: PR #541; CRD ch.39 R39.8.0ag1 (status note of
  2026-11-25).

### Known limitations

- **KDF Reloaded neither shows nor spends ARRR funds held in the Ironwood pool.** The Ironwood pool
  is Pirate Chain's newer shielded pool. Funds there, for example funds a Pirate wallet on the same
  seed keeps in it, stay safe and can be spent from that Pirate wallet; the balance KDF Reloaded
  shows covers the Sapling part only. A coin in native mode is refused at activation in this
  release, as before.

## [0.4.1-beta.1] — 2026-11-21

<!-- How to add an entry: put it under the matching ### group and #### area (add the area heading if it is missing), as a bold one-sentence summary in plain words, then the detail, ending with "Detail: PR #N; CRD ch.X RY". -->
<!-- The same PR updates every affected document: the CRD chapter, the plan body and its "Plans touched" row, RELOADED_VS_GLEEC.md, docs/GLEEC_COMPATIBILITY.md and "Known limitations" below; a plan's Status line is left to the maintainer. -->

### Upgrade note: a shielded wallet database already migrated by a development build

**0.4.1-beta.1 cannot open a shielded (ARRR, Pirate) wallet database that a newer development
build has already migrated.** This concerns only a data directory that a development build of
the next version has run on. A data directory that only ran 0.4.0-beta.1 is not affected.

To use such a wallet with 0.4.1-beta.1, stop the node, copy the
`<TICKER>_RELOADED_WALLET.db.pre-upgrade.<ms>.bak` file that the development build wrote next to
the wallet database over `<TICKER>_RELOADED_WALLET.db`, delete `<TICKER>_RELOADED_WALLET.db-wal`
and `<TICKER>_RELOADED_WALLET.db-shm` if they exist, and start 0.4.1-beta.1. Anything the wallet
recorded after the migration is not in the backup; the node scans it again. The changelog of the
development line describes the migration and its backup under "Breaking changes and upgrade
notes".

### Changed

#### Order book and peer-to-peer network

- **The order book no longer shows ARRR or BC2 orders from nodes that cannot complete such a
  swap.** Since v0.4.0 KDF Reloaded declines an ARRR swap with a node that does not state that it
  builds version-6 transactions, and a BC2 swap with a node that does not state that it signs
  with the BC2 suffix. The orders of those nodes were still listed, often at the best price, and a
  taker that picked one got no swap: the request timed out after 30 seconds, and a
  good-till-cancelled order was then turned into a maker order. Now `orderbook` and `best_orders`
  (legacy and mmrpc 2.0), the order-book event stream and the `orderbook_depth` counts the node
  takes from its own book leave such orders out, and the counts and volume totals cover the listed
  orders only. `best_orders` reads the pair's full book when the best-priced orders are left out,
  so it still returns the best orders the node can trade with. `buy` and `sell` that name only
  such orders in `match_by` fail at once with an error that names the coin and the missing
  capability, and no order is created. The same rule covers any coin that requires a capability
  of its counterparty later.
  - *Switching it off:* the existing settings `require_v6_counterparty` (ARRR, in the coin's
    `consensus_params`) and `require_suffix_counterparty` (BC2, in the coin entry) list the orders
    again, together with admitting those counterparties. While a setting is on, no request lists
    the hidden orders.
  - *Other nodes:* nothing changes for them. No message changes, and a KDF Reloaded relay still
    passes every order on.
  - *Limits:* counts that `orderbook_depth` gets from a relay still include such orders. A node of
    another implementation still lists a KDF Reloaded maker's ARRR or BC2 order, and its request
    times out.

  Detail: PRs #446, #447; CRD ch.59 §59.4C.

### Plans touched

- [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md) — planning: the
  decision that the order-book answer shows no capability flag was revised; the order book now
  leaves out orders that lack a capability the node requires (PRs #446, #447).

## [0.4.0-beta.1] — 2026-11-14

Within each heading, entries are grouped by area. Each entry ends with the pull request that made
it and, where one exists, the CRD rule, for readers who want more.

### Highlights

The most important changes of this release in short. Each item points to the detailed entry below.

#### What is new

- **Siacoin (SC) swaps are supported.** PRs #239, #278, #290, #291.
- **Electrum servers that fall behind the chain or stop following it are no longer asked, and
  servers that disagree about the chain stop the coin from trading.** PR #324.
- **Under an HD login, each coin trades, withdraws and keeps its accounts with its own key and
  address.** PRs #249, #250, #253, #254, #270, #279, #281, #293.
- **ARRR pays Pirate1 addresses and declines swaps with nodes that cannot build the Ironwood
  format.** PRs #229, #230, #241, #246.
- **Bitcoin II (BC2) works with its sighash suffix (with the `sighash_suffix` key in the coin
  entry), and an Electrum server can be pinned by its key.** PRs #261, #264, #273.
- **The order book holds up better: a lost cancellation heals sooner, and stuck connections, cut
  links and sync gaps recover faster.** PRs #207, #223, #243, #258, #280, #303, #328.
- **Prices come from two sources that check each other.** PRs #287, #326.
- **Tendermint coins take custom tokens, `path_to_address` and the chain's priority gas prices.**
  PRs #319, #323.
- **Releases ship Linux ARM builds and the mobile libraries with a C header.** PRs #300, #321.
- **Compatibility switches restore GLEEC KDF behavior where KDF Reloaded differs.** More in
  [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md).

#### Major fixes

- **A swap between Siacoin, or another coin with a 32-byte hash lock, and an EVM coin or token can
  complete.** PR #290.
- **An EVM taker on a chain with fast blocks, such as Monad, finds the maker's spend, also when it
  comes late, shortly before a refund.** PRs #248, #255.
- **`recover_funds_of_swap` no longer refunds a maker payment after the spend of the taker payment
  was sent.** PRs #276, #284.
- **A coin with several Electrum servers no longer fails to activate when one server is silent and
  another refuses.** PR #324.
- **An order whose save failed no longer comes back after a restart.** PRs #309, #314.
- **The ARRR history names recipients, a swap's addresses and the fees of the wallet's own sends.**
  PRs #268, #271, #277, #288.
- **SLP withdrawals no longer give BCH to the miner (unit tests), and BCA HTLC spends and refunds can
  be built.** PRs #220, #221, #236.
- **A maker whose claim of the taker payment was mined no longer refunds its own payment, and a
  maker whose claim was lost can recover its payment once the taker has refunded.** PRs #344, #345,
  #347.
- **ARRR activates on Windows when the user profile path has non-ASCII characters.** PR #346.

#### Warnings and gaps

All of these are described under Not in this release and Known limitations below.

- **Do not run two KDF instances on the same seed at the same time.**
- **The swap watcher does not work yet**, so keep your node running until your swaps have finished.
- **TRON and Solana cannot be activated, and Tendermint coins are wallet-only.**
- **ZEC cannot send since the Zcash network upgrade NU7; do not trade ZEC.**
- **ARRR swaps work only between two KDF Reloaded nodes of this release or later**; native mode and
  the Ironwood pool are postponed.
- **Every Siacoin swap transaction pays a fixed fee about 450 to 850 times below the rate the Sia
  node recommends**, and some Siacoin recovery paths have unit tests only.
- **Under a Trezor key, some UTXO operations are not available.**

### Upgrade note: name lookups go to Google Public DNS

**If your node runs on a network that blocks Google Public DNS, set `dns_resolver` before you
upgrade.** Some countries, many company networks and captive portals block it.

Earlier builds looked up coin servers and services (Electrum, EVM and other nodes, lightwalletd)
through your computer's own resolver. This release looks up every name through Google Public DNS
unless `dns_resolver` says otherwise. Seeds were already looked up that way.

Where Google Public DNS is blocked, an upgraded node with the default resolves no name and
reaches no coin server until you set one of these in `MM2.json`:
- `"dns_resolver": "system"`: your computer's resolver, as before for coin servers;
- `"dns_resolver": ["192.0.2.53"]`: DNS servers of your choice.

At start the node logs which resolver it uses. A name it cannot resolve is logged with a hint
that names the key. See [Network configuration](docs/NETWORK_CONFIG.md#dns-resolver). Detail: PRs
#216, #219; CRD ch.45 §45.6B.

### Upgrade note: a new node identity under an HD login

**If you log in with `enable_hd: true`, other nodes see your node under a new public key after
this upgrade.** That key is the one `get_public_key` reports. Earlier builds signed orders and
swaps with a hash of the passphrase text instead.
- **Open orders** are announced again under the new key when the node starts. The copies that
  other nodes still hold under the old key expire after the maker-order timeout. You can also
  cancel your orders before the upgrade.
- **Swaps in progress** finish on the key they started with.
- **Swap statistics** show two public keys for your node, one from before the upgrade and one
  after.
- **A relay (seed node)** running with an HD login gets a new peer ID. Other nodes learn it again
  through the relay's address.

A login without `enable_hd` is not affected. Detail: PRs #253, #254; CRD ch.05 R45–R49.

### Upgrade note: Tendermint coins under an HD login move to the seed's standard address

**If you log in with `enable_hd: true` and use a Tendermint coin (ATOM, IRIS and others), move
its funds off the old address before you upgrade.** Under an HD login a Tendermint coin now uses
the seed's standard address at `path_to_address`, as other wallets do; before upgrading, move
funds from the old address with the previous release, v0.3.0-beta.1.
- **The new address** is the BIP-44 address at the coin's `derivation_path` and `path_to_address`
  (by default account 0, `External`, address 0), for example `m/44'/118'/0'/0/0` for ATOM.
- **The old address** came from a hash of the passphrase text, which no other wallet uses. A node
  that is upgraded without moving the funds first shows an empty balance on the new address.
  Start the previous release and withdraw from the old address to the new one.
- **A coin configuration without `derivation_path`** cannot be activated under an HD login and
  fails with `PlatformCoinCreationError`.

A login without `enable_hd` is not affected. Detail: PR #319; CRD ch.36 R36.1.2, R36.1.4 step 3.

### Added

#### ARRR and other shielded coins

- **ARRR can now pay a Pirate1 address.** Pirate Chain's own wallets hand out Pirate1 addresses
  (`pirate1…`, the address form of its Ironwood pool) by default. `validateaddress` now accepts them,
  and `withdraw` pays them from the wallet's Sapling balance at Pirate Chain's fixed fee of 0.0001
  ARRR, keeping the change in Sapling. The withdrawal is recorded at once like any other, so its
  coins are not offered to the next send and its change shows as pending. The wallet itself stays
  Sapling-only and has no Pirate1 receiving address, and this does not change ARRR swaps. The first such
  withdrawal after a start takes longer, because the node builds the key for the Ironwood proof once.
  To record these withdrawals the node now carries a patched copy of the `zcash_client_sqlite`
  crate, with one change. Detail: PRs #229, #246; CRD ch.39 §39.6.5.

#### EVM coins

- **The log shows the gas limits each EVM coin and token will use.** At activation, one line names
  where the coin's gas limits come from: its own entry in the coins file, its platform coin's entry,
  or the built-in defaults. It lists the values that differ from the defaults, and the gas-estimate
  multiplier when one is set; the full list is logged at debug level. A coins file that lacks the
  limits a chain needs is now visible at once, as it was not when Monad token swaps ran out of gas
  with a coins file older than October 28, 2026. Detail: PR #200; CRD ch.57 R57.5.5.

#### Tendermint (Cosmos) coins

- **A Tendermint coin can carry the chain's three published gas prices, and a request can pick one by
  name.** The coin's `protocol_data` in the coins file may now hold `priority_gas_prices`, an array of
  exactly three numbers: the low, average and high gas prices of the chain, in the base denom per gas
  unit (the chain registry publishes them). A withdraw, a delegation, an undelegation or a reward claim
  that names `Low`, `Average` or `High` is priced with the matching figure; `Default`, and any name on a
  coin without the array, keep the coin's usual `gas_price`. The array is optional, the figures may be in
  any order, and a token is priced with its platform coin's array. A coin without the member is priced
  exactly as before. Detail: PR #323; CRD ch.36 R36.3.5, ch.18 R18.6B.1.
- **A Tendermint request can set the gas limit and the gas price, or the price level.** The `fee`
  member of `withdraw`, `task::withdraw::init` and the staking requests now takes two more forms for a
  Tendermint coin or token: `{"type": "CosmosGas", "gas_limit": 125000, "gas_price": 0.03}` and
  `{"type": "CosmosGasPriority", "gas_limit": 125000, "gas_price_option": "High"}` (`Low`, `Average`,
  `High` or `Default`, spelled exactly so). The gas limit is the one the transaction is signed with. The
  fee amount is still derived from the node's gas simulation, `ceil(gas used x 1.5 x price)`, not from
  the limit times the price. UTXO, EVM, QRC20 and Siacoin coins refuse the two forms with
  `InvalidFeePolicy` (a shielded coin refuses any `fee`, as before), and a Tendermint request that
  carries a form of another family ignores it and uses the default price and gas limit. Detail: PR #323; CRD ch.49 R49.28 to R49.30, ch.18 R18.6B.2.
- **`get_swap_gas_fee_policy` and `set_swap_gas_fee_policy` now accept a Tendermint platform
  coin.** The policy (`Legacy`, `Low`, `Medium`, `High`, starting at `Legacy`) selects the gas price of
  the coin's swap transactions and of its trade-fee estimates: `Low`, `Medium` and `High` pick the low,
  average and high figure of `priority_gas_prices`, `Legacy` the default price. It never changes a
  withdraw or a staking transaction. A Tendermint token has no policy of its own and
  the two methods answer `NotSupported` for its ticker. On a coin without `priority_gas_prices` the
  methods still accept and store any value, with one warning in the log that the coin cannot honor the
  level, and prices stay at the default. Detail: PR #323; CRD ch.35 R35.6.8, R35.6.9.

#### Order book and peer-to-peer network

- **How close a connection comes to being closed as stuck can now be watched.** A node closes a
  connection whose peer loses at least 25 % of the messages queued for it in each of three
  consecutive minutes.
  - **Log.** Each minute in which a connection loses 10 % or more is now logged at debug level,
    with the counts and how many such minutes in a row it has had.
  - **Metrics.** `p2p.send_queue.window_share.max` gives the highest recent share, and
    `p2p.send_queue.drop_series.max` the longest current run of such minutes.

  Detail: PRs #204, #214; CRD ch.28 RQ1 items 6 and 8.
- **A coin's protocol information can now carry a capability word that older nodes and other
  implementations ignore.** The
  protocol information a coin sends with its orders, requests and reservations may now end with a
  small extra map that names capabilities the node has, so that a later coin can require a
  capability of its counterparty before a swap starts. A node reads only the part before the word,
  so the order book, the address format it reports and every coin that asks for nothing behave
  exactly as before, and the word changes no order identity, no order book root and no
  synchronization. A reader that cannot make sense of the word treats it as absent. ARRR (PR #241)
  and coins with a sighash suffix such as BC2 (PR #264) send a word; every other coin sends exactly
  what it sent before. Detail: PRs #227, #235; CRD ch.59 R59.4A.1 to R59.4A.7.
- **The log now says why a reservation or a request was turned down by a coin.** When a coin
  declines the protocol information of a counterparty's reservation (on the taker side) or request
  (on the maker side), the node logs one debug line naming both orders and the coin. When a taker's
  pass over the reservations it received ends without a connect and a coin declined at least one of
  them, one info line names the taker order and the number declined, so an operator whose order
  times out can see why. The protocol information itself is never logged. Nothing else changes in
  how orders are matched. Detail: PRs #227, #235; CRD ch.59 R59.4A.6.
- **A new opt-in setting keeps your orders when a re-pricing fails.** Set the top-level
  `MM2.json` boolean `setprice_replace_after_checks` to `true`, and `setprice` with
  `cancel_previous` (which is also what the market-making bot uses) checks the new order first
  and cancels your previous orders on the pair only when every check and the build of the new
  order have passed. A failed check, such as a balance read that times out, then returns its
  error and cancels nothing. **The cost is a stale price:** after a failed re-pricing your
  previous orders stay live and matchable at their old price until your next successful
  `setprice` or a cancel. An empty pair only misses trades, while a stale price can lose
  funds, so turn it on only if you accept that. To bound how long an order can stand at an old
  price, give each `setprice` a `timeout_in_minutes`: the order is then cancelled at the latest
  that many minutes after it was placed (for an order restored at startup, after the restart).
  The setting is off by default, which keeps today's behavior: a failed check cancels the
  previous orders and creates none. Only the boolean `true` turns it on. Nothing on the wire
  changes. Detail: PRs #265, #286; CRD ch.32 R-GH8a.

#### UTXO coins and Electrum

- **A new request, `electrum_servers`, shows which Electrum server a coin uses and how far behind
  each one is.** Send `{"mmrpc": "2.0", "method": "electrum_servers", "params": {"coin": "RIN"}}`
  for an active coin that uses Electrum servers. The answer lists every server of the coin in the
  order the node ranks them, by host and port: whether it is connected, whether reads are being sent
  to it, its latest block height, how many blocks it is behind the other servers, whether it is out
  of date (`behind`, `ahead` or `stuck`), its smoothed round trip, whether it is suspended or marked
  as poor, and whether it was removed from the coin's list. It also gives the coin's tolerance in
  blocks and the block height most servers agree on. The request only reads what the node already
  knows and sends nothing to any server. A coin that is not active answers `NoSuchCoin`; a coin that
  does not use an Electrum client (a coin with a local daemon, a shielded coin, a token of another
  family) answers `CoinIsNotElectrum`. Detail: PR #324; CRD ch.38 R38.6.4o.
- **Radiant (RXD) swaps between two KDF Reloaded nodes are tested on the mainnet.** On 2026-11-09, RXD against
  RIN completed in both directions (RXD as the maker's coin and as the taker's coin), and a refund of an RXD
  payment went out for the maker and for the taker after the lock time. The first swap took 930 seconds, of which
  842 seconds were the maker's payment waiting for its two confirmations. The two
  refunds were broadcast 27 and 18 minutes after the lock time recorded in the payment transaction, and neither
  was rejected. A plain RXD withdrawal was accepted and confirmed within about 90 seconds. RXD swaps between
  a KDF Reloaded node and GLEEC KDF 3.0.0-beta (build d56a7bc50) also completed in both directions on a private
  test mesh (871 and 1531 seconds). Swaps with Komodo DeFi Framework are not part of this. Detail: PR #322;
  CRD ch.58 D2; plan
  [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md).
- **An Electrum server can now be pinned by its key.** A server entry with `protocol` `SSL` may
  carry `spki_sha256`: the SHA-256 digest of the server's public key in hexadecimal (64 digits,
  colons between byte pairs allowed), or a list of up to four, so the next key can be pinned before
  a server changes it. With a pin the node accepts the server only if it proves that it holds a
  pinned key. The certificate's chain, dates and name are no longer checked, so a self-signed
  certificate can be used and still be authenticated, and the pin decides whether or not
  `disable_cert_verification` is also set. A key that does not match is refused like any failed
  handshake: the server is suspended and tried again later, never removed, and every refusal is
  logged as a warning with the server's host and port. A malformed value, or a pin on a `TCP`
  server, removes that server from the coin's list at activation, with a log line, and the other
  servers are used. The log names each pinned server once at activation. A browser build cannot pin
  and removes such a server. Komodo DeFi Framework and GLEEC KDF ignore the key, so an entry that
  carries it still works there, without the pin. How to get the value is described in
  [Network configuration](docs/NETWORK_CONFIG.md#electrum-servers-over-tls). Detail: PRs #240, #273;
  CRD ch.38 R38.6.4n.
- **A coin entry can now name a "sighash suffix", which Bitcoin II (BC2) needs.** Since block 57,750
  the BC2 network checks every signature against a digest that carries an extra 4-byte field after the
  signature-hash type. With the new top-level key `sighash_suffix` (a `0x`-prefixed hexadecimal value
  of at most 32 bits; BC2's is `0x01324342`) every input the node signs for the coin uses that digest:
  withdrawals, the DEX-fee transaction, HTLC payments, spends and refunds, the taker's pre-signature
  of a version-two swap and `sign_raw_transaction`. A coin without the key signs exactly as before. A
  malformed value, or a value on a Radiant or Zcash-family coin, fails the activation of the coin
  with an error naming the key, a hardware wallet
  refuses to sign for such a coin, and a BC2 entry without the key logs one warning that the network
  will refuse its transactions. The optional key `sighash_suffix_height` is checked but not used yet.
  The shared coins repository carries the key on the `BC2` and `BC2-segwit` entries since 2026-11-14
  (see Known limitations). Detail: PRs #261, #264; CRD ch.61.
- **A swap of a coin with a sighash suffix now starts only with a counterparty that signs with it.**
  **BC2 trading in KDF Reloaded is incompatible with other Komodo DeFi Framework implementations:**
  they cannot spend BC2 since block 57,750, and KDF Reloaded declines a BC2 swap with them before it
  starts. BC2 swaps require a counterparty that signs with the suffix. This check is temporary and
  will be removed as soon as the other implementations support suffix signing. A coin with a non-zero `sighash_suffix`
  now states that in its orders and match messages and declines a counterparty that does not, on the
  taker and the maker side, before anything is paid. KDF Reloaded nodes that predate the statement are
  declined too; Komodo DeFi Framework and GLEEC KDF keep matching as before, and their taker sees a
  request that times out. Each decline is logged at debug level with its cause, and at most once an
  hour a warning gives the number declined. Setting `require_suffix_counterparty` to `false` in the
  coin entry turns the requirement off and restores the earlier behavior, in which a BC2 swap with such
  a node can fail after a payment has been made. Detail: PRs #261, #264; CRD ch.61 R61.5A; ch.59 §59.4A.

#### Start-up and configuration

- **A warning when the full key export is switched on.** With `allow_insecure_key_export: true`
  in `MM2.json`, the node now logs a warning at start-up that says `get_private_keys` can export the
  keys of every configured coin, and another warning for each call that uses the full export, with
  the mode and the number of coins asked for. Neither warning contains a key. Detail: PR #305; CRD
  ch.07 R-K1.
- **The start-up log now lists the files the node uses.** At start the node logs, at INFO and as
  absolute paths, where its configuration came from (the command line, the default file or the file
  named by `MM_CONF_PATH`), where its coins list came from, the log file, the database folders and the
  wallet file it reads, so a node started from the wrong folder shows it at once. It never logs file
  contents or configuration values, and no WSS certificate or key path. Detail: PR #209; CRD ch.45 R45.6.6.

#### Tendermint (Cosmos) coins

- **Tendermint custom tokens can be activated.** `tokens_params` of `enable_tendermint_with_assets`
  and `enable_tendermint_token` accept a `protocol` member, the `TENDERMINTTOKEN` protocol of a
  token that is not in the coins file. A `protocol` for a ticker that the coins file already lists
  fails with `CustomTokenError` (`DuplicateTickerInConfig`). A custom token is wallet-only: orders
  and swaps with it are refused, as with a custom EVM token. Detail: PR #319; CRD ch.36 R36.1.2,
  R36.2.1, R36.2.4.
- **Under an HD login, a Tendermint coin honors `path_to_address`.** The activated address is the
  one at `path_to_address` under the coin's `derivation_path` (default account 0, `External`,
  address 0), and the coin's public key is derived from that address's key. See the upgrade note
  above. Detail: PR #319; CRD ch.36 R36.1.2, R36.1.4 step 3.

#### Releases and downloads

- **Every pull request now builds the node for 64-bit ARM Linux.** The check cross-builds `kdf` for
  aarch64, tests the glibc floor and starts the binary under an emulator, so a change that breaks
  the ARM build is found before it merges. It is not a required check at first; it becomes one after
  about ten green runs on `dev` (roughly two weeks). Detail: PR #320; the Tests workflow.
- **The mobile libraries now also cover the Android x86_64 ABI and the iOS simulator.** Each signed release carries `libkdf.a` for Android x86_64 (emulators and Chromebooks), and for the iOS simulator on Apple silicon (`aarch64-apple-ios-sim`) and Intel (`x86_64-apple-ios`) Macs, as separate libraries next to the device one. The dev and staging builds carry them too. Detail: PR #321; `docs/RELEASE_ARTIFACTS.md`.
- **Releases now include a C header for the mobile libraries.** `kdf-<tag>.h` declares the functions of `libkdf.a` with their exact prototypes and the log callback type, so an Android or iOS app can call the library without writing the declarations itself. CI builds the library, compiles and runs a test program against the header, and checks that the header and the library agree. Detail: PR #321; the Release workflow and `docs/RELEASE_ARTIFACTS.md`.
- **Releases now include Linux builds for 64-bit and 32-bit ARM, and the mobile libraries.** Besides the x86_64 binary, each signed release carries `kdf` for aarch64 and armv7 Linux (glibc 2.31 or newer, like the x86_64 one), and `libkdf.a` static libraries for Android (aarch64 and armv7) and iOS (aarch64) apps. The macOS binary that runs on both Intel and Apple silicon is attached as well; earlier releases did not include it. Every file is in the signed checksum list. The names, target triples and minimum system versions are in `docs/RELEASE_ARTIFACTS.md`. Detail: PR #300.

### Changed

#### ARRR and other shielded coins

- **ARRR now declines a swap with a node that cannot build the Ironwood transaction format, before
  anything is paid.** An ARRR node now states in its orders and match messages that it builds
  version-6 transactions, and declines a counterparty that does not, on the taker and the maker side,
  before the DEX fee or any payment. Until now such a swap started normally and failed at the other
  node's first ARRR transaction, which cost the DEX fee, locked ARRR until the refund, or both. KDF
  Reloaded 0.3.0-beta.1 nodes are declined too: they build the format but predate the statement, so
  their users upgrade to trade ARRR. GLEEC KDF keeps matching as before; a GLEEC taker sees a request
  that times out. Each decline is logged at debug level with its cause, and at most once an hour a
  warning gives the number declined. Setting `require_v6_counterparty` to `false` in the coin's
  `consensus_params` turns the requirement off. Detail: PRs #230, #241; CRD ch.39 R39.6.4f.
- **An ARRR withdrawal that sets `fee` or `from` is now refused with the shared "unsupported" error.**
  The answer is `error_type: "UnsupportedError"` (HTTP 400), as for other withdrawals that ask for
  something a coin cannot do, and as GLEEC KDF answers. A custom `fee` used to give an internal
  error (HTTP 500). **A `from` used to be ignored silently**, and the withdrawal was made from the
  wallet's own address; a client that sends `from` for ARRR now gets this refusal instead. Detail:
  PR #256; CRD ch.49 R49.27a.

#### EVM coins

- **A request no longer fails when one node answers a little after 10 seconds while the other
  nodes fail fast.** An EVM request asks one node at a time and moves on to the next after 10
  seconds. Earlier builds then threw away a slow node's answer, so a slow first node followed by a
  second node that refused at once (for example with HTTP 429) failed the whole request, although
  the first node answered. The request now keeps listening to a node that is slow for up to 30
  seconds from the moment it was asked, uses the first good answer from any node it has already
  asked, and sends nothing twice. When every node fails, the error reads as before. A request in
  which every node stays silent can now take about 20 seconds longer than before (two nodes: 40
  seconds instead of 20). A coin with one node is not affected. Detail: PR #313; CRD ch.35 D35.11.
- **The quoted fee for sending an EVM taker fee is now the fee at the gas limit the node sends it
  with.** `trade_preimage`, the taker's balance check before a swap and the maximum taker volume
  used to price the taker-fee transfer at the node's gas estimate, while the transfer is sent with
  the fixed `eth_send_erc20` limit (`eth_send_coins` for a platform coin). On Monad, which charges
  the full limit, the USDC-MON fee transfer is estimated at about 83,000 gas and charged the 150,000
  its coins-file entry sets. The quote now uses the limit, as every other EVM swap quote already
  does. On chains that charge only the gas used, the quoted fee is now higher than what is charged.
  GLEEC KDF sends the fee with the same limit but quotes the estimate; set the `MM2.json` boolean
  `evm_taker_fee_quote_at_limit` to `false` to quote it that way, as KDF Reloaded did before.
  Detail: PR #213; CRD ch.57 R57.4.2, R57.5.2a.

#### Order book and peer-to-peer network

- **A maker order without `min_volume` now advertises a minimum of at least ten times the HTLC spend
  fee** when a coin of the pair pays that fee out of the HTLC: a UTXO coin such as RIN, the BCH and
  Qtum platform coins, and the transparent side of a shielded coin. Sia keeps its own floor. Until now
  the default was the coins' floors only, so on a coin with a changing fee the smallest trade could
  lose most of its value to the fee. The raised minimum is capped at the order's volume, so a small
  order trades only whole. It is computed at `setprice`, and the maker `trade_preimage` checks the
  same value. Peers see it in `orderbook` and `best_orders`; a request below it gets no reservation.
  An order with its own `min_volume`, taker orders and `update_maker_order` keep their minimums. On a
  coin whose floor is below the safe spend amount, a `setprice` without `min_volume` that the
  spend-safety check refused until now is accepted at the raised minimum. When the fee estimate is
  unavailable, the plain default is used if `pre_trade_spend_check` is off, with a warning in the
  log; with the check on, the order is refused as before. *Compatibility:* set
  `spend_safe_min_volume` to `false` in `MM2.json` for the GLEEC KDF default. Detail: PR #315; CRD
  ch.60 R60.6.8.
- **A relay connection that a network cut ended can be dialed again before the 5-minute wait.** The
  node does not dial an address again within 5 minutes of its last dial, as on GLEEC KDF, so a
  connection the node had dialed and that a cut ended could stay down for up to 5 minutes. Now an
  address whose connection was established, lasted at least 30 seconds and ended by a network error,
  a close from the peer or a failed ping check may be dialed again at the next 30-second maintenance
  check while the node is short of relays, at most twice in 5 minutes. A connection closed for being
  idle, by the clock check or by the relay count keeps the 5-minute wait, and so does one closed as
  stuck unless that close ends in a network error. Detail: PR #303;
  CRD ch.28 R27a item 1a.

- **Messages from other nodes are checked for nesting depth and declared lengths before they are
  read.** Every value a peer sends is checked first: arrays and maps may nest at most 64 levels, and every
  declared length and count must fit in the bytes that follow. A value outside these bounds is
  dropped as malformed and counted in the metric `p2p.payload.malformed`; the encoding is
  unchanged and honest peers are not affected. Detail: CRD PR #237, PR #238; CRD ch.28 R16a.

- **A node waits 5 minutes before it dials the same address again, and no longer dials a seed it
  is already connected to.** A node that holds fewer relays than it needs dials seeds and known
  peers every 30 seconds.
  - **Repeated dials.** An address that did not answer used to be dialed again at every round,
    about 120 times an hour. It is now dialed at most once per 5 minutes, as GLEEC KDF does. A
    seed that drops a connection soon after it was made is therefore redialed up to 5 minutes
    later, not 30 seconds later.
  - **A loop with seeds.** A seed that had connected to the node by itself was dialed a second
    time at every round, because the node compared addresses only. The second connection went
    unused and closed after two minutes, so the cycle repeated every 2.5 minutes for as long as
    the node was short of relays. The node now remembers which peer each seed address reached
    and does not dial the address while that peer is connected.
  - **Log.** The warning "P2P relays below low watermark" appears at most once per 5 minutes
    while the number of connected relays stays the same.

  Nothing changes on the wire. Detail: PRs #199, #206; CRD ch.28 R27a.
- **A relay holds more sync requests for a maker it is still catching up on.** During a burst, for
  example after several makers restart, a relay now holds up to 32 such requests per asking node and
  2,048 in all (before: 8 and 256) and answers them when its own sync lands, instead of answering
  with an error at once. The limits are the `MM2.json` settings `sync_answer_deferral_max_per_peer`
  and `sync_answer_deferral_max_total` (at most 64 and 8,192). Nothing on the wire changes. Detail:
  PR #205; CRD ch.32 R-Q5b.
- **A node whose order-book sync from one relay fails, or brings back an outdated set, now asks one
  other relay.** Before, it waited for the maker's next keep-alive, 30 seconds later, and a relay that
  had missed a cancellation passed the cancelled order on to every node that asked it. The node now
  asks one other relay it is connected to, once, for the pairs concerned, and never empties a pair
  because the sets differ. To restore the old behavior, set `sync_request_alternate_relay` to
  `false` in `MM2.json`. Detail: PRs #207, #208; CRD ch.32 R-Q5d.
- **Skipped peer addresses are summarized once an hour instead of logged one by one.** Peer
  exchange skips addresses it can't use, such as hostnames in another form or IPv6 addresses, and
  used to log each new one at info level. It now logs each at debug level, and once an hour, if
  anything was skipped, one info line with the counts per form and the hostname lookups that
  failed, timed out or gave no usable address. Detail: PRs #203, #211; CRD ch.28 R8c item 4.
- **An unused connection to a relay outside the node's relay mesh no longer logs a warning when it
  closes.** No message keeps such a connection busy, so it closes after two minutes as idle. That
  close now logs at info level, as an idle connection closing. A relay in the relay mesh, and
  every other cause, still log a warning. Detail: PRs #203, #211; CRD ch.28 R27b.
- **Two address ranges are judged as GLEEC KDF judges them.** All of `0.0.0.0/8` counts as not
  public, and `192.0.0.9` and `192.0.0.10` count as public. This applies to the addresses a node
  receives from peers, those it looks up, and the one it announces. Detail: PRs #203, #211; CRD
  ch.28 R24.
- **A cancellation this node received now keeps the order out for an hour.** Before, 120 seconds
  after a cancellation, an answer from a relay that had missed it could bring the cancelled order
  back, and the node then passed it on. Now the order stays out of every later message and answer
  for an hour. To restore the old behavior, set `cancel_final_window_secs` to `120` in `MM2.json`.
  Memory stays bounded: cancellations older than two minutes are capped at 100,000, about 20 MB
  (`cancel_final_max_entries`), and past the cap the oldest fall back to the 120-second behavior.
  Nothing on the wire changes. Detail: PRs #223, #224; CRD ch.32 R-K1b.
- **A maker no longer brings back an order it cancelled when the order's stored record could not be
  deleted.** Before, such an order came back live at the next start and was announced again. Now it
  is skipped and deleted when the order history records its removal, and a failed deletion is tried
  three times. An order created with `save_in_history: false` leaves no history entry and is
  restored as before. To restore the old behavior, set `maker_restore_skips_cancelled` to `false` in
  `MM2.json`. Detail: PRs #223, #224; CRD ch.12 R7c.
- **A cancelled order a node missed no longer stays visible once its maker stops advertising that
  pair.** A maker names every pair it still has orders on in each keep-alive, so a pair that goes
  unnamed for the liveness window, 90 seconds or the `maker_order_timeout` setting, was either
  emptied or lost its keep-alives on the way. The node now drops that pair's orders after the window;
  the maker's other pairs stay. If the keep-alives return, the node asks for the pair as a new one
  and gets its orders back. Before, the order stayed until the maker posted on that pair again or
  went offline. Your own orders are never dropped. To restore the old behavior, set `pair_liveness`
  to `false` in `MM2.json`. Detail: PRs #243, #251; CRD ch.32 R-GH14.
- **A relay holds a sync request that reaches it before it has started its own sync for that
  pair.** A relay passes a maker's keep-alive on before it handles it. When many makers start or
  re-post at once, the nodes behind it therefore asked for the maker's orders before the relay had
  started its own sync, got "no data" or "missing pair" at once, and asked again only at the maker's
  next keep-alive, 30 seconds later for each relay on the way. The relay now holds such a request
  while it handles that keep-alive, and answers it once its own sync lands, within the same
  5-second bound and the same limits as the requests it already holds. The keep-alives it is
  handling are limited by the same two settings; one that finds a limit full is handled as before.
  The relay's 60-second sync summary line ends with a new `pre_placeholder` part that counts these
  requests and the keep-alives handled without a hold. To restore the old behavior, set
  `sync_answer_deferral_before_placeholder` to `false` in `MM2.json`. Nothing on the wire changes.
  Detail: CRD PR #258, PR #259; CRD ch.32 R-Q5b item 6.
- **An order-book request no longer fails at once while a relay is still storing a maker's first
  orders.** A relay answers an order-book request from its list of orders, and each maker on that
  list needs a stored record. When many makers post at once, a maker's first orders can be listed
  before its record is stored, and the whole request used to fail, so the asking node moved on to
  another relay or, with none left, failed its `orderbook` call. The relay now waits up to 250 ms
  for the record and answers then; only if it is still behind does the request fail as before. A
  waiting request counts against the same limits as the sync requests a relay holds. To restore the
  old behavior, set `orderbook_answer_waits_for_worker` to `false` in `MM2.json`. Nothing on the
  wire changes. Detail: CRD PR #258, PR #259; CRD ch.32 R-GH12a.
- **The debug line of a sync request to another relay now says which candidate rule would have
  skipped it.** A node asks one other relay when the root a pair reached after its relay's answer
  differs from the one the maker advertised. Behind a busy relay this happened far more often than
  expected. In a separate lab run on 2026-11-07, with the maker repricing every 10 s, all 188 such
  requests followed a change of the pair by the maker after its keep-alive, so the root the node
  reached, which included that change, differed from the one the keep-alive advertised. With the
  repricing finished before the keep-alive, 35 followed, none after such a change and all after a
  relay had re-established its link; their cause is not established. With the maker not
  repricing, none followed.
  With debug logging on, the line `sync alternate relay: … event=mismatch` now also counts, among
  the pairs asked about, those that a later keep-alive of the maker advertised with the root the
  node now holds, those that an order message of the maker changed in the meantime, and those whose
  advertised root the node held at some moment since the keep-alive. Nothing the node sends
  changes, and with debug logging off nothing extra is kept. Detail: PR #269; CRD ch.32 R-Q5d.

#### UTXO coins and Electrum

- **`mixed` is now the default Electrum server order.** When `electrum_server_order` is not set in
  `MM2.json`, or is set to a value the node does not recognize, the node now ranks a coin's Electrum
  servers by what it measures instead of drawing one random order at activation: it times each
  server's answer to a small header request and checks that the server is up to date with the chain,
  then draws the order with a weight that favors the faster up-to-date servers, half of each weight
  staying uniform so that nodes do not all pick the same server. A maintained server that falls
  behind or turns slow is replaced by another listed server, at most once per 5 minutes, and the
  node logs the replacement, with its reason when it has one. To keep the previous behavior, set `electrum_server_order` to `random`; for the
  behavior of GLEEC KDF, which keeps the listed order, set it to `listed`. Detail: PR #349; CRD
  ch.38 R38.6.4b-R, R38.6.4l.

- **A coin whose Electrum servers disagree about the chain now fails to activate, and later refuses
  new trades.** A server that is far ahead of the rest is not simply wrong: it can be the one that
  followed a network upgrade while the others stopped. The node cannot tell which side is right, so
  it stops instead of choosing. A server counts as far ahead when, with three or more servers, it is
  more than the coin's tolerance (three minutes of blocks) above the height the others reach. Two
  servers that answer different block headers for the same height, compared at a height below the
  tip, also count. When the node activates a coin it waits up to 5 seconds after its first
  connection for the other servers' heights and the comparison, then fails the activation with an
  error that names every server with a height, and the coin is not registered. The legacy `electrum`
  and `enable` calls and the task activations report that text. After activation, when the servers
  come to disagree, the node logs one error line, and the coin refuses a new order with it as base
  or rel and a withdrawal with the same text, and does not start a new swap (the error line says
  why), until the servers agree again; balance and other reads and swaps that have started go on. To
  fix it, remove the servers that follow the other chain from the coin's list or activate the coin
  with servers that agree. With two servers the node compares the block headers at a common height
  when they are far apart: the same header means the lower server is only behind (the higher one is
  used), a different header is a disagreement. A coin activated with fewer than three servers logs
  one warning, because with fewer the node cannot always tell a stale server from one on another
  chain. `electrum_tip_checks` set to `false` turns all of this off. The `electrum_servers` answer
  has a new field `chain_disagreement`. Detail: PR #324; CRD ch.38 R38.6.4o items 12–13.
- **A coin's Electrum servers that are behind the chain, or stopped following it, are no longer
  asked.** A server that is 1,000 blocks behind used to keep its share of balance and history reads,
  and a swap's wait for a confirmation could be answered by any server in turn. Now the node asks
  every connected server for its newest block when it joins the coin's set, under every
  `electrum_server_order`, and compares them. A server is *behind* when it is more than the coin's
  tolerance below the height two or more servers have reached (with two servers, below the higher
  one). The tolerance is three minutes of blocks, at least 1 and at most 20: 1 for BTC, 2 for LTC, 3
  for RIN and KMD, 12 for DGB. A server far above that height is not hidden: see the entry on
  disagreement about the chain. A server is *stuck* when it sends no new block for longer than three
  block times or ten minutes while another server does. Such a server stays in the set and keeps
  answering when the others fail, but reads, transaction broadcasts and confirmation waits go to the
  others first, and it is asked again as soon as it has caught up. The node logs one warning, naming
  the server by host and port, when a server falls behind or is stuck, and when it is up to date
  again. A swap's confirmation wait now asks one server for the whole wait (the first connected one
  that is up to date) and moves to another only when that one fails, so the weakest of several
  servers can no longer end it early. A single server that claims a height far above the others, or
  a server of another network in the list, no longer makes the honest servers look behind. With
  `electrum_server_order` set to `mixed` or `measured`, the same tolerance decides which measured
  servers count as fresh, and repeated searches for a replacement wait 5, 10, 20, 40 and then 60
  minutes, so a halted chain does not make every node probe the public servers every five minutes,
  and a search for a replacement connects at most four servers at a time. For the old behavior, set
  the `MM2.json` boolean `electrum_tip_checks` to `false` (and `electrum_server_order` to `listed`
  and `electrum_spread_reads` to `false` to match GLEEC KDF). The default server order is now
  `mixed`; see the entry "`mixed` is now the default Electrum server order" below. Detail: PR #324;
  CRD ch.38 R38.6.4o.
- **`fetch_utxos`, `consolidate_utxos` and the automatic merge (`utxo_merge_params`) now work
  under an HD login**, as they do in GLEEC KDF. Until now the two commands were refused and the
  automatic merge stayed off.
  - `fetch_utxos` lists the unspent outputs of every address the wallet knows, in every account. It
    makes no new address known.
  - `consolidate_utxos` merges only the outputs of the coin's own address, the one at
    `path_to_address`, into one output paid back to it. No other address is touched. With
    `broadcast: false` it returns the transaction without sending it.
  - The automatic merge works on the same address, for a UTXO coin such as RIN and for QTUM.
  - Under a Trezor key, `consolidate_utxos` answers `InvalidAddress` and the automatic merge stops,
    so the device is never asked to sign; `fetch_utxos` works.

  Detail: PR #281; CRD ch.05 R35b.
- **Electrum reads now go only to the servers with the shortest round trip.** Since 0.3.0-beta.1
  a coin's read-only Electrum queries took turns over every connected server, so a distant server
  in the list answered one read in N. Now they take turns only among the connected servers whose
  recent round trip is at most twice the fastest one's, or within 20 ms of it. The others still
  answer when those fail, and come back into the turn once their round trip is short again. The
  round trip is measured from answers the node already gets; nothing extra is sent. From the lab
  host, RIN's six servers answered a balance read in 25 to 266 ms: spread over all six that
  averages about 114 ms per read, and over the three this rule keeps, about 28 ms. When every
  server slows down, as under a per-session limit, all of them stay in the turn. The `MM2.json`
  numbers `electrum_spread_near_factor` (default `2`) and `electrum_spread_near_offset_ms` (default
  `20`) set the factor and the margin; a factor of `0` spreads over every connected server as
  before. Detail: PRs #201, #215; CRD ch.38 R38.6.4a.
- **Electrum reads always keep at least the two fastest connected servers taking turns.** A coin
  with two servers spreads its reads over both again, as before the round-trip rule above. A server
  that answers slowly once, for example one that limits each session to about one answer a second,
  could leave the turn for a while and send every read to the other server, which then reached its
  own limit sooner. With more servers, a distant one is left out only while two others stay near.
  Detail: PRs #260, #262; CRD ch.38 R38.6.4a.
- **A fixed withdraw fee on a UTXO coin is now charged in proportion to the transaction's size, as
  on Komodo and GLEEC KDF.** When `withdraw` or `task::withdraw::init` gives its fee as
  `{"type":"UtxoFixed","amount":…}`, the amount is now a rate per 1,000 bytes: a transaction of `s`
  bytes pays `amount × s / 1000`, rounded down to a base unit, exactly as with `UtxoPerKbyte`. Before,
  KDF Reloaded charged the amount for every started kilobyte. *Upgrade note:* the same request now
  costs less, and below 1,000 bytes less than the amount entered. A 245-byte KMD withdrawal that
  paid the whole fee now pays about a quarter of it, and a 1,404-byte DOGE withdrawal about 1.4
  times it instead of twice. A withdrawal without `fee`, swaps and the DEX fee are not affected. A
  `withdraw` of an SLP token on BCH now charges it the same way, with the amount in BCH (PR #234).
  Detail: PRs #226, #234; CRD ch.38 R38.6.6l, R38.6.6m.
- **A server that skips its certificate checks is now flagged when the coin is activated.** For
  each `SSL` Electrum server that has `disable_cert_verification` set and no `spki_sha256`, the node
  logs one warning at activation, naming the server by host and port: the connection is encrypted,
  but the server is not authenticated. Which certificates the node accepts and refuses is
  unchanged. Detail: PRs #240, #273; CRD ch.38 R38.6.4m.

#### Start-up and configuration

- **The start-up banner now shows the release version.** The first log line reads like
  `KDF-Reloaded DeFi Framework 0.4.0-alpha.1 (a1b2c3d4e) DT 2026-11-02T12:00:00+01:00`, with the version
  of the release package before the short commit hash, so a log tells at once which release it came
  from. The `version` call and the version stored in swap records still carry the short commit hash
  only, unchanged. Detail: PR #210; CRD ch.45 R45.6.7.

#### Memory use

- **The memory limit set in 0.3.0-beta.1 was measured further, and its default of 2 stays.** In two consecutive
  6.5-hour runs on the same host, with 480 orders updated about 20 times a second, both settings levelled off. With the
  limit, resident memory was about 17 MB lower and showed no measurable drift in hours 3 to 6 (0.004 ± 0.003 MB/min; a
  drift of up to about 0.01 MB/min is not excluded); without it, a small residual drift (0.019 ± 0.009 MB/min) is not
  excluded. The limit is not shown to be necessary at that load on 8 cores; it removed the growth seen at about 80
  updates a second on 256 cores. No cost of the limit was resolved on start-up, activation, eight swaps or query latency.
  Not measured: 256 cores or 80 updates a second over several hours, and a shielded swap. Detail: PR #225; plan
  [`malloc-arena-cap`](docs/plans/malloc-arena-cap.md); CRD ch.45 R45.6A.8; issue #191.

#### Trading under an HD login

- **Under an HD login, each coin's swap now uses the coin's own key.** This is what swaps of EVM
  coins and of coins enabled with a single key need in order to complete. An HD login is one with
  `enable_hd` set to `true`, as the Komodo DeFi wallet uses in HD mode.
  - **Every coin's swap announces the key it pays from.** For an EVM coin or token that is its
    signer. For a UTXO coin enabled through legacy `enable` or `electrum` it is the coin's own key.
  - **A UTXO coin such as RIN activated through `task::enable_utxo` at another address**
    (`path_to_address`) now funds the swap from that address, takes its change there, and is paid
    there. The balance a swap checks and the fee `trade_preimage` estimates use the same address.
    Earlier builds took the coins of the first address (account 0, External, address 0). At the
    first address nothing changes.
  - **Swaps already in progress** keep the key they started with.
  - **An iguana login**, without `enable_hd`, does not change.

  Detail: PRs #249, #250; CRD ch.05 R41–R44.
- **Under an HD login, the node now trades under the key `get_public_key` reports.** That key
  signs the maker's orders and keep-alives, fills `sender_pubkey`, and is the persistent key of
  every new swap, as in GLEEC KDF.
  - A maker's own `orderbook` now marks its orders as its own (`is_mine`).
  - The maker sends one keep-alive per pair every 30 s instead of re-sending each order.
  - A relay's peer ID comes from the same key.
  - The hash of the passphrase text is kept only for swaps that recorded it, for a Lightning node's
    seed, and for activating BCH with its tokens, until that activation moves to the coin key
    (Tendermint moved to the coin key; see the upgrade note above).

  See the upgrade note above. Detail: PRs #253, #254; CRD ch.05 R45–R49.
- **Under an HD login, a swap whose announced key would differ from the key its coin pays from is
  refused before it starts.** An HD login is one with `enable_hd` set to `true`, as the Komodo DeFi
  wallet uses in HD mode. `setprice`, `update_maker_order`, `buy`, `sell` and `trade_preimage` check
  the pair before any balance check and answer `<coin> cannot be swapped under this login: <reason>`
  (`trade_preimage`: error type `SwapKeyMismatch` with the coin and the reason). An order of your own
  that fails the check is cancelled when a request matches it, with one warning in the log; no
  reservation is sent, and a swap that has already started is never stopped. Now that each coin's
  swap uses the coin's own key (PR #250, above), no coin fails the check; it stays as a guard. Set the
  `MM2.json` boolean `swap_key_check` to `false` to turn it off. Detail: PRs #231, #233; CRD ch.05
  R36–R40.

#### Accounts and addresses under an HD login

- **`get_mnemonic`, `change_mnemonic_password` and `get_private_keys` work while a Trezor is
  initialized.** They refused whenever a Trezor context existed, even though the mnemonic and the
  seed's keys they handle are the login's, not the device's. Now only a coin activated for the
  Trezor is refused, as on GLEEC KDF: the activated-coins form of `get_private_keys` refuses it as
  before, and the opt-in full export refuses a requested ticker that is activated for the Trezor
  with `HardwareWalletNotSupported`. Detail: PR #293; CRD ch.05 §5.10B R56, ch.07 R-K6.

- **Under an HD login, activation now lists the coin's own address from the start.** A UTXO coin
  such as RIN or QTUM activated through its task method, and an EVM coin activated with its tokens,
  list the address at `path_to_address` (by default account 0, External, address 0) with its balance
  in the activation result, in `account_balance` and in `my_tx_history`, without `get_new_address`.
  If that address's account does not exist yet, activation creates it and reports only that account.
  This is what GLEEC KDF does. Until now the coins that a swap paid to that address could stay
  unlisted until `get_new_address` reached it. An `address_id` of 2^31 or more fails activation with
  an internal error. Detail: PR #279; CRD ch.05 R35a.
- **`task::create_new_account` now works under an HD login without a hardware wallet.** It serves a
  UTXO coin such as RIN, QTUM, EVM coins and Siacoin; until now it asked for a hardware wallet first
  and failed.
  - The request takes an optional `account_id`. Without it, the lowest unused account number is
    taken.
  - With an `account_id` that is already in use, the task ends with `AccountAlreadyExists` (with the
    `account_id`) before anything is derived; `task::create_new_account::status` reports it. GLEEC
    KDF reports an internal error there.
  - `task::create_new_account::cancel` is now served. Cancelling removes the account the task
    created if it is the wallet's highest-numbered account.
  - Under a software login the task reports `Preparing` and then its result.

  Detail: PR #279; CRD ch.05 R35c.

#### Name lookups

- **One setting now chooses the DNS resolver for every name the node looks up.** The setting
  `dns_resolver` covers seeds, names that other peers hand out, Electrum servers, EVM, Solana,
  Tendermint and UTXO node addresses, lightwalletd servers, NFT providers, the coins-file fetch
  and price sources.
  - **Values.** Absent or `"google"`: Google Public DNS, the default. `"system"`: the computer's
    own resolver. An array of addresses such as `["192.0.2.53", "[2001:db8::53]:5353"]`: those
    DNS servers only. Any other value stops the node at start with an error naming the key.
  - **No fallback.** A name that can't be resolved fails only the connection that needed it,
    which is retried as before. No other resolver is asked, and the node keeps running.
  - **Cache.** With Google or chosen servers, answers are kept in memory only, for the lifetime
    the DNS records give, within 30 seconds and 1 hour. While the resolver can't be reached, the
    last known answer of a name is used for up to 24 hours after it expired, and a warning says
    so.
  - **Not yet covered.** Sia, the WalletConnect relay, Lightning peer addresses and the
    statistics method still use the computer's resolver.
  - **Logs.** One info line at start names the resolver. A name that can't be resolved is
    logged at most once an hour.
  - **Metrics.** `dns.lookups.count`, `dns.failures.count`, `dns.stale_served.count` and
    `dns.last_known.entries`.

  See the upgrade note above. Detail: PRs #216, #219; CRD ch.45 §45.6B.
- **Hostnames that other peers hand out are looked up without blocking a worker thread.** The
  lookup now uses the node's resolver, one at a time, given up after 5 seconds as before.
  Detail: PRs #216, #219; CRD ch.28 R8c item 7.

#### Transport, TLS and dependencies

- **One new RustSec advisory is accepted in `deny.toml` until the SQLite library is updated.**
  RUSTSEC-2026-0459 concerns the bundled SQLite (before 3.54.0) returning uninitialized memory when
  a database is opened together with a `-wal` file written by a less trusted party. The node opens
  only database files it creates itself in its own data directory, so the condition is outside its
  model. The fix is a `rusqlite` update, which waits for the shielded-coin wallet library to move
  to that version. The node itself is unchanged. Detail: PR #332.
- **The old DNS libraries are gone from the build.** The peer-to-peer transport no longer uses
  its own DNS layer, so that layer and the unmaintained resolver library it pulled in
  (trust-dns 0.22, with an accepted `idna` advisory) are no longer built. The dependency policy
  now bans them. Detail: PRs #216, #219; CRD ch.45 R45.6B.11.

#### Siacoin

- **Siacoin (SC) swaps are supported.** Atomic swaps of SC were verified on the live chain on
  November 4, 2026: between two KDF Reloaded nodes in both directions, including a refund, and
  with Komodo KDF and GLEEC KDF nodes in the cases that Known limitations does not exclude. Earlier
  releases said SC swaps were postponed; that statement is withdrawn. On November 10, 2026 three
  more cases passed between two KDF Reloaded nodes: a maker that sells SC refunds its payment by
  itself when the taker stops (the refund gate compares the lock with the Sia node's median block
  time, so the refund was broadcast about an hour and a half after the lock, and it was mined in the
  next block); a node restarted after such a stop
  refunds by itself at start; and an SC/RIN swap in both directions on netid 6133, with that
  network's 2 % fee. Detail: PR #239; CRD ch.20 D5, R-S11; release tests, "Siacoin refund,
  recovery and netid 6133".

#### Withdrawals

- **A withdrawal refused as unsupported now answers `error_type` `UnsupportedError`, as on Komodo
  and GLEEC KDF.** Under a Trezor key, an EVM withdrawal through the direct `withdraw` method and a
  TRON withdrawal answered `UnsupportedUnderTrezor`. In the browser build, a TRON withdrawal under
  MetaMask answered `UnsupportedUnderMetamask`. Both now answer `UnsupportedError`, still with HTTP
  400 and the same message text in `error_data`. A `withdraw` of an SLP token that names `from` is
  now refused the same way; before, `from` was ignored and the token was sent from the BCH
  wallet's own address. *Upgrade note:* a client that checks `error_type` for either old value
  must check `UnsupportedError` instead. Detail: PR #236; CRD ch.49 R49.27, R49.27a.
- **Under an HD login, the direct `withdraw` now works for a UTXO coin such as RIN and for QTUM.**
  With `from` it spends from the address `from` names, as on GLEEC KDF. An account or address the
  wallet does not hold is refused with `UnknownAccount` or `UnexpectedFromAddress` (HTTP 400), as on
  `task::withdraw`. Without `from`, both methods spend from the coin's activated address, the
  address at `path_to_address` (by default account 0, External, address 0), and the change returns
  there. GLEEC KDF refuses a withdrawal without `from` under an HD login. Until now the direct method
  answered an internal error under an HD login, and `task::withdraw` without `from` always spent
  from account 0, External, address 0, whatever `path_to_address` named. Under a Trezor key the
  direct method still answers an internal error; use `task::withdraw` there. Detail: PR #270; CRD
  ch.49 R49.4, ch.05 §5.9B R35.
- **Under an iguana login (without `enable_hd`), a UTXO withdrawal that names `from` is now refused
  with `UnexpectedFromAddress` (HTTP 400), as on GLEEC KDF.** This covers the direct `withdraw` of a
  UTXO coin such as RIN, of QTUM and of BCH, whatever address `from` names, the wallet's own
  included. **Until now the direct method ignored `from`** and spent from the wallet's address;
  `task::withdraw` already refused it. A withdrawal without `from` does not change. *Upgrade note:*
  a client that sends `from` under an iguana login stops sending it. Detail: PR #270; CRD ch.49
  R49.27b.

#### RPC interface

- **The test helper `sim_panic` is no longer part of release builds.** This legacy method made
  the node panic on request; it is now compiled only with the Cargo feature `sim-panic-rpc`, which
  no published build enables. A request for the name answers `NoSuchMethod` like any unknown
  method. Detail: PR #331; CRD ch.43 R43.4.1 status note.
- **A request without `mmrpc` whose method only the mmrpc 2.0 interface knows is now answered
  through that interface, as on GLEEC KDF.** When the legacy method table has no entry for the
  request's method, the node retries the request once as an mmrpc 2.0 request and answers in the
  mmrpc 2.0 envelope: the method's result, its typed error, `InvalidRequest` (HTTP 400) when the body
  has members other than `userpass`, `method`, `params` and `id`, or `NoSuchMethod` (HTTP 400) when
  neither interface knows the name. Until now such a request answered the legacy error body with
  HTTP 500. A request that needs a password and lacks a valid `userpass` is still refused with the
  legacy error body before any retry. `setprice`, `buy`, `sell`, `update_maker_order`, `cancel_order`, `cancel_all_orders`,
  `my_orders`, `order_status` and `orderbook_depth` stay legacy-only, as on GLEEC KDF: called with
  `mmrpc`, they answer `NoSuchMethod`. Detail: PR #312; CRD ch.43 §43.1A, R43.1A.1a–R43.1A.1e.
- **RPC refusals that the request itself caused are logged at WARN instead of ERROR.** An unknown
  or inactive coin, a volume or price below the minimum, not enough balance and an invalid
  mmrpc 2.0 request no longer fill the log with errors. The log text, the response body and the
  HTTP status are unchanged; genuine failures such as a node that does not answer stay at ERROR.
  Detail: PR #340.

#### Tendermint (Cosmos) coins

- **Under an HD login, a Tendermint coin now uses the seed's standard address, not the address of
  the passphrase hash.** **Before you upgrade, move funds from the old address with the previous
  release, v0.3.0-beta.1.** Under an HD login a Tendermint coin (ATOM, IRIS and others) now uses the
  seed's standard address at `path_to_address`, as other wallets do. A login without `enable_hd`
  does not change. See the upgrade note above. Detail: PR #319; CRD ch.36 R36.1.2, R36.1.4 step 3.
- **A Tendermint coin or token that holds no local private key is wallet-only.** It is refused when
  an order is placed, so this node's own key cannot stand in for the coin's key on the swap path. A
  token follows its platform coin. No activation produces such a coin yet. Detail: PR #319; CRD
  ch.36 R36.1.2.
- **The Tendermint swap code follows the CRD's swap rules more closely; Tendermint swaps stay off.**
  Swaps of Tendermint coins remain switched off in this release, and their code was brought closer to
  the rules they will follow when they return:
  - the check of a taker's DEX fee requires exactly one message, the exact recipients, sender and
    amounts (the burn form included), the swap uuid as memo, and a byte-equal copy of the transaction
    on the node;
  - after a restart, a node finds its own payment by the HTLC id, so a payment to another receiver or
    of another amount is no longer taken for it;
  - a swap secret must be exactly 32 bytes;
  - a token's trade fees are labeled with the platform coin and its decimals, since the platform coin
    pays them;
  - a chain without an HTLC module, such as ATOM or OSMO, is refused when an order is placed, and a
    counterparty whose order carries protocol information, where an IBC route would travel, is
    declined.

  One change applies today: a coin entry with `avg_blocktime` 0 now fails activation with an error
  naming the key. Detail: PR #299; CRD ch.18 §18.11.
- **A Tendermint coin whose configured gas prices include a figure of zero or less now fails
  activation.** A `priority_gas_prices` array with a figure that is zero, negative or not a finite
  number makes the coin's fee zero or undefined, so activation is refused with a
  `PlatformCoinCreationError` that names the member, and nothing is registered. A malformed array (not
  exactly three numbers) fails with `CoinProtocolParseError`. Detail: PR #323; CRD ch.36 R36.3.5 (note),
  T36.17.
- **Delegations, undelegations and reward claims on a Tendermint coin are signed with a larger default
  gas limit.** They used the 125,000 of a plain transfer. The defaults are now 187,500 for a delegation
  and a reward claim and 250,000 for an undelegation; a request can still set its own limit with the
  new `fee` forms. The fee amount comes from the node's gas simulation and does not change. Detail: PR
  #323; CRD ch.18 R18.6B.3.

#### TRON coins

- **TRON coins show their balance, their own address and can broadcast a signed transaction through
  the usual wallet calls.** The balance of TRX and of TRC20 tokens is read from the TRON node; an
  account the chain has not seen yet shows a zero balance, and a node error is reported as an error,
  never as zero. The coin's address is shown in the TRON form that starts with `T`, and
  `validateaddress` accepts the same forms as a TRON withdrawal. A raw transaction is sent to the
  TRON node instead of an EVM node. A TRON withdrawal that names a `fee` is refused with
  `InvalidFeePolicy` instead of ignoring it, since TRON works out its own fee. Two defects that kept
  every TRON withdrawal from reaching the network are fixed: the current block is read in the form
  the node sends, and a TRX transfer now carries its contract type. There is no user effect while
  TRON is switched off in this build. Detail: PR #333; CRD ch.21 §21.10A.

#### Price service and the maker bot

- **A coin on which the price sources disagree is now priced from the first source, with a warning, instead of
  being left unpriced.** The 5 % check between the two sources no longer leaves a coin unpriced just because the
  two services take its price from different providers and the figures differ. If one price provider's figures
  from two sources agree, the newest of them is used. Otherwise the coin is priced from the first source, in the
  order of `price_urls`, and the log carries one warning that names the coin, both figures, both providers and the
  host used. The bot and the swap prices both follow it. **With this default a wrong figure from the first source
  can be used, with a warning.** `"price_conflict": "refuse"` in `MM2.json` restores the old behavior: such a
  coin is not priced. A value of another form is ignored with one warning that names the key. See
  [Network configuration](docs/NETWORK_CONFIG.md#price-service). Detail: PR #326; CRD ch.62 D62.2.

### Fixed

#### ARRR and other shielded coins

- **A Windows user profile path with non-ASCII characters no longer crashes ARRR activation.** The
  default Sapling parameter directory was read through a call that returns the profile path in the
  ANSI code page and was converted without a check, so a user name such as one with accented letters
  made the activation task panic and never answer. The default directory of ARRR, and the default data
  directory that native-mode UTXO coins and QRC20 coins use to find their daemon configuration file,
  are now read losslessly (the `APPDATA` variable first, then the system's known-folder lookup; the
  home directory on other systems). A lookup that fails is reported as an activation error that
  names the reason and, for ARRR, `zcash_params_path` as the remedy, never as a crash. Checked with
  unit tests on Linux and with a Windows profile run of this PR's build on 2026-11-12 (profile path
  with non-ASCII characters: two activation attempts without the parameter files returned reported
  errors, the third activated and scanned, one swap as maker finished with the node spending the
  shielded HTLC payment, 0 panics); the Windows variant of the unit test has not executed (the
  Windows CI job builds only). Detail: PR #346; CRD ch.39, status note of 2026-11-12 (Sapling
  parameter path).

- **A shielded broadcast now logs the identifier the server returned beside the local one.** In
  light mode the lightwalletd service answers a successful broadcast with the identifier its node
  accepted. The node used to discard the answer and log only the identifier it computed from the
  bytes. It now logs both in one `info` line, and writes a `warning` when they differ. The identifier
  the node returns to the caller is still the locally computed one. The lines name no server.
  Detail: PR #345; CRD ch.39, status note of 2026-11-12 after R39.11.30.

- **The node no longer sends a shielded transaction again that is already mined.** Before a
  light-mode broadcast it looks the transaction id up in the wallet database. A transaction the
  database holds as mined is refused with an error that names its state ("mined in block N"), and
  nothing is handed to a server; the error is also logged next to an `info` line. Until now a
  second send of the same bytes after the transaction was mined reached the server (the
  lightwalletd endpoint), whose node answered with a duplicate-nullifier error that read like a
  note-reuse defect. A transaction that is recorded but not yet mined, or that a reorg rewound, is
  not refused and is sent again as before, so a re-push stays possible. Bytes that are no
  transaction and transactions without a Sapling spend go out as before, and a failed lookup does
  not block the broadcast. A withdrawal build now also logs one line with its duration, its number
  of Sapling spends and whether it has an Ironwood output. Detail: PR #341; CRD ch.39, status note
  of 2026-11-10 after R39.11.30.

- **An incoming ARRR payment now shows as pending before it is mined.** In light mode the node
  checks the mempool for payments to the wallet and counts them in the non-spendable part of the
  balance. It ran that check only after it had scanned a new block. A payment mined in the next
  block was therefore never shown as pending, and while no block arrived the pending figure could
  be older than the 30-second sync period. The check now runs on every sync pass, including a pass
  that finds no new block or fails. Found in a live mainnet test with a deposit from a Pirate1
  address. Detail: PR #263; CRD ch.39 R39.8.0aj1.

- **The ARRR history now names the recipient of a withdrawal, and lists the wallet's own address
  once.** Every history row used to list the wallet's own address twice, once in a unified form
  beginning with `u1` that no ARRR wallet accepts. A withdrawal's row did not name its recipient,
  before or after mining, whether it paid a `zs1…` or a Pirate1 address. A withdrawal now keeps
  its destination, exactly as the request gave it, in a small file beside the wallet database
  (`<TICKER>_RELOADED_TX_ADDRESSES.db`), and the history lists it. Found in a live mainnet test.
  Detail: PR #268; CRD ch.39 R39.8.9d1, R39.8.9d2.

- **An ARRR withdrawal now answers `my_balance_change` as received minus spent.** The answer to a
  withdrawal gave the amount spent minus the amount received, a positive number, while the
  transaction's history row, Komodo DeFi Framework and GLEEC KDF give received minus spent, a
  negative number. The answer now gives the negative number too, for a `zs1…` and a Pirate1
  destination alike. The transaction itself was always correct. Clients that inverted the sign
  of `my_balance_change` for ARRR withdrawals to compensate must drop that workaround. Found in a
  live mainnet test. Detail: PR #271; CRD ch.39 R39.5A.3, R39.6.5c.

- **The ARRR history now lists the addresses of a swap's own transactions.** A swap's payment lists
  the script address it pays, its fee payment lists the fee address, and a spend or a refund lists
  the script address it spends from. Their rows used to list only the wallet's own address. The
  node records these addresses when it builds the transaction, in the same file as a withdrawal's
  recipient, and fetches nothing. Detail: PR #277; CRD ch.39 R39.8.9d3.

- **The ARRR history now reports the fee of the wallet's own sends.** A withdrawal, a swap payment
  and a swap fee payment show the fee they paid in `transaction_fee`, before and after mining. Every
  row used to report 0. The fee comes from the record the node writes when the backend accepts the
  broadcast. Found in a live test. Detail: PR #288; CRD ch.39 R39.8.9e1.

#### EVM coins

- **A taker on an EVM chain with fast blocks, such as Monad, now finds the maker's spend of its
  payment.** While waiting for the maker to spend its payment, a taker asked for the swap
  contract's events from the swap's first block to the newest one in a single request. On Monad,
  with a block every 0.4 seconds, that range passed what public nodes accept (100 or 1,000 blocks)
  within minutes. Every request was refused, and the taker never learned the secret. While the
  requests failed, the taker also never reached the end of its wait, so it did not take its refund
  path either.
  - **Windows.** Every event search now runs in windows of at most `logs_block_range` blocks
    (coins file, default 1,000), and the wait starts at the payment's own block.
  - **Smaller windows on refusal.** A node that refuses a window's size makes the window smaller
    for the rest of the session, with one warning in the log.
  - **Deadline.** The wait ends at its deadline even while requests fail. The same deadline fix
    applies to the version-2 swap's event search.

  Detail: PR #248; CRD ch.17 R17.8.1–R17.8.4.
- **Before refunding on an EVM chain, a taker now finds a late spend of its payment within its time
  budget.** Before a refund, the taker looks on the chain for a spend of its payment by the maker,
  under a budget of two minutes. That look always started at the swap's first block. A maker that
  spent the payment late, near the end of its lock time, left the spend far from the start. On a
  chain with fast blocks, the look could then run out of time on every try. The taker would keep
  trying a refund that cannot succeed and never claim the maker's payment.
  - **Newest blocks first.** The look now scans from the newest blocks down.
  - **No rescanning.** It remembers what it has scanned for the rest of the session, so a later look
    goes on where the last one stopped. The newest 12 blocks are always scanned again, in case of
    a reorganization.
  - **Lagging nodes.** A node behind the newest block can answer a range as empty. So after a full
    pass that found nothing, the look starts over. The taker's wait for the spend, and the
    version-2 swap's event search, go back to their start at most once a minute after an empty
    pass.
  - **Version-2 swaps.** The version-2 swap's event search no longer rescans its whole range on every
    poll.

  Detail: PR #255; CRD ch.17 R17.8.3a.
- **A swap between Siacoin (or another coin with a 32-byte hash lock) and an EVM coin or token can now
  complete.** The EVM payment embeds the value the swap contract checks, derived from the 32-byte
  secret hash, and the validation of a payment expects that value. Before, the counterparty refused the
  payment. QRC20 payments now embed and validate the same derived value, and the secret of a spend is
  checked against it; the QRC20 path is covered by unit tests only. Detail: PR #290; CRD ch.51 R72A.
- **A nonce read that one EVM node fails while another answers is now logged as a warning naming
  that node, and a single node's error no longer says "all EVM nodes failed".** Before a send, the
  daemon asks every node of the coin for the pending nonce. A node that failed got an info line
  starting with "Error" even when another node answered and the send went through, and a coin with
  one node reported that node's failure as "all EVM nodes failed". Now the failing node's line is a
  warning that says "Nonce request ... failed ... on node ...; another node answered". A round in
  which no node answers is logged as a warning that says it is retrying, as "Nonce request ...
  failed ... on node ...; no node answered, retrying (round 2 of 6)", and only a failure in the last
  round, when the send fails, is logged at error level, as "Error ... when getting nonce ...". The
  error of a single node names that node and its failure, for example
  `https://rpc.example.org: HTTP 429 Too Many Requests`. With two or more nodes the text is
  unchanged. Detail: PR #294; CRD ch.35 R35.1.12, R35.1.14.

#### Node names in logs and errors

- **A node URL that a lower layer writes in another spelling is now shortened to scheme, host and
  port as well.** Before, an error text from the HTTP client, TLS or the browser lost a node's URL
  only when it repeated the URL exactly as configured. Now a copy that is percent-encoded, in
  another letter case, with a default port added or dropped, or with a trailing slash added also
  loses its userinfo, path and query, on the native and the browser (WASM) build. Such a URL keeps
  the scheme and port the lower layer wrote, and a URL of another host is left as it is. The
  `mm2_net` crate now also builds on its own for the browser target. Detail: PR #218; CRD ch.35
  R35.1.14.

#### Order book and peer-to-peer network

- **A connection closed for a stuck gossip queue is no longer dialed again early.** A node closes
  every connection to a peer whose gossip send queue stays stuck. Such a close counts as this node's
  own close, so the address waits the full 5-minute redial guard. Earlier builds told that close apart
  only when it ended cleanly; one that ended with a transport error looked like a cut and could be
  dialed again within about 30 s. The gossip layer now marks the connections it closes as stuck, and
  the redial guard reads the mark. Detail: PR #328; CRD ch.28 R27a item 1a.
- **The mmrpc 2.0 `orderbook` names its internal failure `Internal`, as on GLEEC KDF.** It answered
  `InternalError`; HTTP 500 and the message are unchanged. *Upgrade note:* a client that checks
  `error_type` for `InternalError` on `orderbook` must check `Internal` instead. Detail: PR #312; CRD
  ch.43 R43.1A.6, D43.1A.3.
- **A maker with `maker_update_coalescing` on is no longer held at its lowest rate by steady
  traffic.** The maker slows down when a relay falls behind.
  - **The old signal.** It was raised once a relay's queue had stayed non-empty for 2 seconds,
    and cleared only after the queue had been empty for 5 seconds. Toward a busy relay, every new
    message restarted those 5 seconds, so the signal could stay raised for hours. A seed showed
    `p2p.outbox.pressure` at 1 while every queue was empty.
  - **The new signal.** It is judged by how long a queued message has waited: raised at 2
    seconds, cleared 5 seconds after the last such wait ends. The default
    (`maker_update_coalescing` off) was never affected.

  Detail: PRs #204, #214; CRD ch.32 R-MP2 item 2.
- **A connection to a peer that has stopped reading is now closed within 30 seconds.** When the node
  closes a connection, for example to a stuck peer or after failed pings, it first flushes what is
  still being sent. Toward a peer whose process hangs while its system keeps the connection open,
  that flush cannot finish, so the close could last as long as the peer's system kept the
  connection. For that time the connection, its queue and its memory stayed held, the peer still
  counted as connected, and the node did not dial it again. The close now gives up after 30 seconds,
  logs the peer with the cause "close timed out", and the peer is treated as disconnected, so the
  usual redial rules apply. In a lab test toward a frozen peer, such a close ended after 30 seconds
  with the deadline; without it, the close stayed open 13 minutes and ended after the peer was
  resumed. Detail: PR #280; CRD ch.28 R27c.
- **An order whose save to the order history failed is reported as failed and no longer comes back
  as a live order after a restart, and a buy or sell whose save failed no longer sends its request.**
  Detail: PR #309; CRD ch.32 R-GH8a.
- **A buy or sell whose save failed no longer comes back after a restart, even when the first
  attempt to remove its record fails.** The removal is now tried up to three times, as it already
  was for a maker order. Detail: PR #314; CRD ch.12 R7c.

#### Siacoin

- **An unreadable transaction in a Sia node's transaction pool no longer breaks the lookups that read the
  pool.** The pool reply is now read one transaction at a time and a transaction the library cannot read
  is left out, so the fee validation of a swap, the raw-transaction lookup and the wait for a spend no
  longer fail because of it. Before, one such transaction made the whole reply fail. A left-out
  transaction is logged only at debug level by the Sia library. Detail: PR #291; CRD ch.20 R-N4.

- **A wallet without SC can now receive SC in a swap.** The swap refused to start with "Not enough SC
  for swap: available 0, required at least 0.00001", although the receiving side spends no wallet
  funds. The miner fee of the spend is taken from the payment itself, and the node now reports it that
  way. Detail: PR #278; CRD ch.20 R-S11.

- **Siacoin orders with the default minimum volume are no longer refused.** The minimum trading volume
  of SC was one hasting, so the default minimum volume of an order could fall under the miner fee and
  `setprice`, `sell` and `buy` refused it as too small (for example selling 10000 SC). The minimum
  trading volume of SC is now 0.0002 SC, ten times the miner fee; the minimum transaction amount stays
  one hasting. The Siacoin miner fee shown for a swap is now the 0.00002 SC actually paid, not
  0.00001 SC. Detail: PR #278; CRD ch.20 R-S11, ch.60 R60.6.7.

#### Start-up and configuration

- **An invalid `wc_session_persistence` now stops the start, as documented.** The value `encrypted`,
  an unknown word or a value that is not a string is refused at start-up with a configuration error
  that names the setting. Before, the node started and only the first WalletConnect call failed.
  Detail: PR #308; CRD ch.22 §22.5.1.

#### Swap recovery

- **`recover_funds_of_swap` no longer refunds the maker payment of a swap whose spend of the taker
  payment was sent but not yet confirmed.** The maker payment belongs to the taker once that spend was
  sent. The call now refuses, whether or not the refund time has passed, and builds nothing. A swap with a
  confirmed spend was already refused. A swap whose maker payment is spent or refunded on the chain is
  answered as before. Detail: PRs #276, #284; CRD ch.51 R75.
- **A swap that waits to refund its payment now writes a warning when the coin's node has not advanced
  for an hour (and again at most hourly).** Before, a refund could wait for hours behind a stalled node
  and the log did not say so; in a test, a refund started four hours after the HTLC lock expired, while
  the coin's node had stood still for almost six hours. The warning names the swap, the coin, the height
  and how long it has not changed. It does not name the cause, and the wait and the refund themselves
  are unchanged. The node is asked for its height about every five minutes, alongside the wait, so the
  wait is never longer for it. The warning is not written while a swap waits for a payment to be
  confirmed, for swaps of the second protocol version or for the watcher. The warning is verified by
  unit tests only; nobody has yet watched it on a stalled node in real time. More in the release tests,
  [finding F4](docs/release-tests/0.4.0-beta.1.md#findings-and-dispositions). Detail: PR #306; CRD
  ch.51 R79-R81.
- **A maker whose claim of the taker payment was mined no longer refunds its own payment after the
  claim call or the wait for the claim's confirmation failed.** Before, the swap went to the refund
  without looking at the taker coin's chain, so a claim that was in fact mined could be followed by
  a refund, and the maker then held both payments. Now the swap first searches the taker coin's
  chain for the spend of the taker payment. When the spend is found, the swap completes with the
  spend and no refund is made; the swap completes only when that spend is confirmed, and a spend
  that is found but not confirmed holds the refund like an unreadable chain. When the taker's refund
  is found, or nothing is found, the refund goes on as before. When the chain cannot be read, the
  refund is held and the search repeated until the refund deadline; the swap then ends with a failed
  refund whose text says the state of the taker payment could not be determined on that coin (or
  that the claim is found but not confirmed), and manual recovery applies. Swaps that never reached
  the claim are unchanged. Detail: PR #344; CRD ch.51 R82.
- **A maker whose claim of the taker payment was lost, and whose taker then refunded, can now
  recover its own payment with `recover_funds_of_swap`.** Before, the call refused, because the
  record holds the sent claim and the maker payment was taken to belong to the taker, although the
  claim never reached the chain and the taker had refunded: the maker then held neither payment.
  Now the call first looks at the taker payment on its own chain, and refunds the maker payment only
  when the taker's refund of the taker payment is found with at least one confirmation. When the
  maker's own claim is found and confirmed, the call refuses as before. When the claim is found but
  not confirmed, or the chain cannot be read, or a found refund has no confirmation, it refuses and
  says so. When nothing spent the taker payment yet, it refuses and says to repeat the call once
  the taker has refunded. A refusal builds nothing and writes nothing. A completed swap is refused
  as before. The automatic refund of the run loop is unchanged. Detail: PR #345; CRD ch.51 R83.
- **Manual recovery of a maker swap whose claim call failed now looks at the taker payment first.**
  Before, `recover_funds_of_swap` refunded the maker payment after the refund time without looking at
  the taker coin's chain, although the claim may have been mined. Now a claim that is found on the
  chain and confirmed refuses the refund. A claim that is found but not confirmed, or a chain that
  cannot be read, refuses and says so. When the taker's refund is found, or nothing spent the taker
  payment, the refund goes on as before. A refusal builds nothing and writes nothing. The automatic
  refund of the run loop is unchanged. Detail: PR #347; CRD ch.51 R84.

#### UTXO coins and Electrum

- **DOGE and other merged-mining coins no longer refuse new trades because their Electrum servers
  seem to disagree about the chain.** The chain comparison added in this release compared whole
  block headers. On merged-mining (AuxPoW) coins a header carries a merged-mining proof after its
  first 80 bytes, which the block hash does not cover, and two servers can store one block with
  different proofs. On DOGE, two public servers answered the same height with 763 and 727 bytes
  whose first 80 bytes were equal; 2 of about 420 heights checked differ this way. When such a
  block arrived, the node compared the servers at that height, found a difference, and kept
  finding it every 5 minutes, so the coin refused new orders, swaps and withdrawals until it was
  activated again. In a field run the state began 12 minutes after the start and did not end in
  about 16 hours. The comparison now compares the first 80 bytes of the headers, which hold the
  version, the previous block hash and the merkle root, so a real fork is still found. Detail:
  CRD ch.38 R38.6.4o items 7 and 12, D38.6.4o.14.

- **A short disagreement between Electrum servers no longer stops a coin's trading.** After
  activation, a server briefly ahead of the others, or a difference between block headers that the
  next comparison no longer finds, put the coin into the refusal state at once. A field log of a
  maker running 16 hours showed this 43 times for VRSC (one server 4 to 6 blocks ahead for 0 to 11
  seconds), 5 times for NENG, once each for DPC and FIRO, and 5 times for CHTA (a header difference
  gone at the next comparison, after 3.5 to 10 minutes). Each time the node logged an error line
  and refused the maker's orders. After activation the node now enters the refusal state only when
  the disagreement has lasted two block times, and at least 60 seconds: a server ahead must still
  be ahead, and two servers at different headers are compared again at the same height once that
  time has passed. The activation itself still fails at once. The length of this period is watched
  in field runs and may change. Detail: CRD ch.38 R38.6.4o item 12, D38.6.4o.15.

- **A coin with several Electrum servers no longer fails to activate when one server never answers
  and another is refused.** A field report: the legacy `electrum` call for UNO, with
  `electrum_server_order` set to `mixed`, three servers and `max_connected` 2, answered "The balance
  was not received within 20 seconds" in 3 of 16 lab runs, and 0 of 16 with `random`. One server
  never accepted a connection, one was refused at once (its certificate had expired) and one was
  good. Three things went wrong together, and each is fixed:
  - Under `mixed` and `measured` the node offered a connection to the coin's set of connected
    servers only after its first block-height answer, so the activation returned with an empty set
    and the balance read had to try every server. Now every connection is offered as it is made, as
    under `random`, and the block-height answer still ranks the servers afterwards.
  - When the set is empty or every connected server fails, the node tried the remaining servers in
    fixed pairs (`max_connected` at a time), and a pair of the silent and the refused server waited
    for the silent one, so the pair with the good server was not reached in time. Now the next
    server starts as soon as one attempt fails, and each attempt's time is worked out from the
    servers still to try.
  - The 20 seconds of the `electrum` and `enable` balance read were not known to the Electrum
    client, which budgeted its attempts against 60 seconds. Now the read passes its 20 seconds on,
    so the client moves to the next server inside it.
  A server that has answered nothing yet no longer takes its turn of the reads while another has
  answered: before, a server that was connected but silent took its share of the reads and each of
  them waited for the failover. The answer to the block-height request a server gets when it joins
  now counts as its first measured round trip. Detail: PR #324; CRD ch.38 R38.6.4a, R38.6.4f,
  R38.6.4j, R38.6.4k-R.
- **A coin whose Electrum servers keep failing no longer fills the log with two errors every 10
  seconds.** With `tx_history` on, the history loop logged the failed balance read and the failed
  history request on every attempt, for as long as the server failed, for example on an expired
  certificate (a field report). It now logs each error once, also when its text carries a changing
  number such as a time, then one summary line per 10 minutes with the count, and one line when the
  server answers again. While the history request keeps failing, the attempts after the first retry
  wait 60 s and then longer, up to 5 minutes, instead of 10 s each, so after the server recovers,
  the history can resume up to 5 minutes later. A healthy coin is unaffected. Detail: PR #327; CRD ch.38 §38.6.5
  item 8.
- **Under an HD login, a coin's account keys and its stored accounts follow the coin's own key
  policy, also when a Trezor is initialized.** A node logged in with a seed phrase accepts
  `task::init_trezor` afterwards, as GLEEC KDF does. Until now:
  - a UTXO coin such as RIN, or QTUM, whose coins entry names a `trezor_coin` took the key of a new
    account from the Trezor instead of the seed, in `task::create_new_account` and at activation, so
    that account's addresses belonged to the device;
  - a coin activated for the Trezor stored its accounts under the seed's wallet identity, where a
    later activation of the same ticker from the seed would load them;
  - Siacoin activation failed;
  - `get_new_address` on a coin activated for the Trezor answered even when the Trezor context was
    gone; it now answers `HwContextNotInitialized`.

  Now a coin takes its keys from the seed under a seed-phrase login and from the device only when it
  is activated for the Trezor, and each kind stores its accounts under its own identity, as on GLEEC
  KDF. Accounts that an earlier build stored under the seed's identity for a coin activated for the
  Trezor are removed from the seed's records at the next activation of that coin from the seed, with
  a warning that names the coin and the account numbers, so they are no longer listed under the
  seed. The check removes any stored account whose key the seed does not derive under the coin's
  current derivation path, so the same warning also appears for a coin whose derivation path has
  changed since its accounts were stored. The device re-creates its own: the next activation for the
  Trezor creates the account it activates, and any other account appears again after
  `task::create_new_account` for its number. Detail: PR #293; CRD ch.05 §5.10B R52–R55, R35a, R35c.
- **A coin activated for a Trezor refuses to work with a different Trezor.** A node accepts
  `task::init_trezor` again while it runs, and a new init can replace the device. Until now such a coin
  read new account keys from the second device and signed withdrawals with it, against accounts that
  belong to the first, and stored the second device's accounts under the first one's identity. Now such
  a coin remembers the device it was activated with and answers `FoundUnexpectedDevice`,
  `get_new_address` included, before it asks the device or writes anything; it works again with its own
  device, or after it is activated anew with the other one. A swap keeps signing with the coin's own
  device and refuses any other. Detail: PR #302; CRD ch.05 §5.10B R58a and R58a.1.
- **BCA HTLC spends and refunds can now be built, and coins whose block headers carry a fixed-length
  trailer are read correctly for the median time.** Bitcoin Atom (BCA) serves 84-byte headers from block
  505888; Reloaded read them as 80 bytes, so the median time it needs before every HTLC spend and refund
  failed, and no swap of the coin could be completed. When the usual header layouts do not fit, the node
  now reads the time of each header from the list by the size the list itself shows, or fetches the
  headers one by one, checks the times for plausibility and fails with an error naming the check instead
  of guessing. Such a coin needs no new entry in the coins file (header sync and SPV proofs for it are
  not covered). Detail: PR #220, PR #221; CRD ch.37 R37.5.10.
- **SLP token withdrawals and HTLC spends no longer give the BCH of their token inputs to the miner,
  and the fee they report is complete.** Each SLP token input carries a little BCH, normally 546
  satoshi, and so does the HTLC output an SLP swap spends or refunds. KDF Reloaded spent these
  inputs without counting their BCH, so it went to the miner on top of the fee. Token sends and
  swap payments did the same, and the fee shown for an SLP withdrawal left it out. Now that BCH
  comes back in the change, and the fee shown is everything the transaction leaves to the miner, as
  on Komodo and GLEEC KDF. Two smaller cases changed with it. On every UTXO coin, change exactly
  equal to the coin's dust amount now comes back as a change output instead of going to the miner.
  The fee shown for an SLP withdrawal now also includes change too small to keep, as it already did
  for UTXO coins. Detail: PR #236; CRD ch.38 R38.6.6n.
- **A transaction with a change output now pays its fee rounded once over its whole size, as on
  Komodo and GLEEC KDF.** KDF Reloaded rounded the change output's share of the fee on its own, so
  such a transaction could pay one base unit less than the rate gives. A RIN withdrawal of 670
  bytes at 0.00001234 RIN per 1,000 bytes paid 825 satoshi, where Komodo and GLEEC charge 826.
  This covers a coin's dynamic fee and an explicit `UtxoFixed` or `UtxoPerKbyte` withdraw fee.
  Detail: PR #247; CRD ch.38 R38.6.6b, T38.6.6b.1.

#### Price service and the maker bot

- **The price service that no longer exists is replaced, prices now come from two sources that check each
  other, and swaps record their USD prices again.** The address `prices.komodo.live`, which the
  market-maker bot used for its default prices and which the node asks when a swap ends for the USD prices
  it stores with the swap, no longer resolves. Since then the bot could not fetch prices unless you named
  another service, and no swap completed stored USD prices. The default is now two sources asked at the
  same time: the GLEEC price service (and, if it does not answer, its cache) and the prices.cipig.net price
  service, each with `?expire_at=259200` so that no price older than 72 hours is served. A coin that only one
  source prices is priced from it. The prices of a coin both services price should agree within 5 %; for one
  that does not, see the `price_conflict` entry under Changed. A source that does not answer is skipped. The bot
  and the swap prices use the same sources. Two new top-level keys in `MM2.json` change them: `price_urls`
  (an array of sources, each one address or an array of addresses tried in order) replaces the default
  sources, and `price_providers` (an array of names) replaces the price providers the node recognizes
  (`binance`, `coingecko`, `coinpaprika`, `forex` and `livecoinwatch`); a wrong provider list stops
  pricing of the coins it leaves out. The `start_simple_market_maker_bot` request accepts `price_urls`
  beside `price_url`; a request with both, or with a malformed value, is refused. A price service that
  fails is now reported once when it starts failing and once when it answers again, not every 30 seconds,
  and so is the bot finding no source at all; on a native build each attempt takes at most 10 seconds (a
  browser build also sets a timer, which no test covers yet); and log lines name a service by its host only,
  with the text of an error, such as the body of a server's answer, cleaned of the address that was
  asked. A swap you took part in is priced once, when it has ended successfully; a swap that failed gets no prices. Prices from
  livecoinwatch are now accepted, so the bot can quote coins such as RIN that only that provider prices,
  and the bot no longer divides by a price of zero. A bot pair whose price is refused or lost still has its
  order cancelled at once, as in GLEEC KDF; this behavior is under observation in v0.4.0 and will be
  revisited. See [Network configuration](docs/NETWORK_CONFIG.md#price-service).
  Detail: PR #287; CRD ch.62.

### Tests

- **The log-capture test helper answers the same to tests that start at the same moment.** About
  thirty `coins` tests capture the log to check that no credential leaks and skip that check when
  another logger owns the process. Two such tests starting together could make the second one skip
  although the capture was in place. The helper now decides once per process, and a test checks
  eight concurrent callers get one answer. The node itself is unchanged. Detail: PR #329.
- **A lab row checks what a short network outage does to a relay connection.** With every packet
  between a relay and a peer dropped and no socket reset, a 10-second outage kept the connection and
  traffic resumed within 6 seconds, three times out of three. After a 30-second outage the
  connections were closed and the peer connected again; after a 60-second outage both connections
  were closed and came back. Traffic was back 23 and 40 seconds after the restore. The node itself
  is unchanged. Detail: PR #292; CRD ch.28 T14.
- **The Qtum testnet tests now use the one public Qtum testnet server, and the QRC20 docker test
  set-up mines its own blocks and waits for new contracts.** The tests that activate the testnet
  Qtum coin pointed at servers that serve Qtum mainnet, so the test wallets looked empty and the
  delegation and balance tests failed. They now use `s6.electrum.qtum.site`. In the Docker set-up,
  the Qtum test node now mines a block when a test waits for a confirmation, the set-up waits until
  a new contract shows its code before the first call to it, and the waits are longer but still
  bounded. The node itself is unchanged.
  Detail: PR #283.
- **The tests that need public servers are off unless you ask for them, and two stale ones are
  fixed.** About 190 unit and integration tests connect to public servers, and a plain `cargo test`
  failed on them whenever a server was down or had changed. They are now ignored by default. Build
  with the `live-network-tests` feature to run them; CI runs the same tests as before. The two tests
  that read one DOC and one MARTY transaction from the live chains use transactions that exist
  today. The Firo test that expected a fee the server no longer sends is ignored, and an offline test
  checks that fee path instead. The node itself is unchanged. Detail: PR #285.
- **The QRC20 Docker tests now mine a block before their own confirmation waits and wait up to 90 s.**
  Nine swap tests that shared one fixed secret or secret hash and a time lock of now minus 200 s now
  draw their own random secret or secret hash per run. The node itself is unchanged. Detail: PR #310.
- **The tests run before the next beta are listed in
  [`docs/release-tests/0.4.0-beta.1.md`](docs/release-tests/0.4.0-beta.1.md).** The lab rows of
  the cycle's second and third batches are recorded there; the fourth batch and the release campaign
  follow. Detail: PRs #266, #272, #275, #282, #289.
- **ARRR is tested against real transactions from Pirate Chain's Ironwood wallets.** Two mainnet
  ARRR transactions that use the Ironwood pool, one paying into it and one paying out of it into a
  Sapling address, are read by every part of the node that reads whole transactions, and their
  Ironwood proofs and signatures verify with the node's own keys. A payment from an Ironwood wallet
  to the node's address is found, counted once, shown as pending while unconfirmed, and spent like
  any other. Nothing changes in how the node behaves. Detail: PRs #229, #242; CRD ch.39 T39.6.5a, T39.8.0i.
- **A unit test of the maker's update queue no longer fails at random.** The test of the 10-second
  keep-alive wait counted the released and the pending orders while the queue kept releasing, so
  one release between the two counts made it fail now and then. It now stops the queue before
  counting. The node itself is unchanged. Detail: PR #244; CRD ch.32 T-MP6.
- **The memory measurement of the order-matching tests builds again.** The test build with the
  `counting-alloc` feature had stopped compiling when the order-matching state gained a field. Its
  test setup now fills every field it does not set itself from the test defaults, so a field added
  later cannot break it again. The node itself is unchanged. Detail: PR #280.
- **The close deadline of the connection layer is tested in CI.** The tests of the vendored
  multiplexer crate, which carries the 30-second close deadline, now run as their own CI job.
  Detail: PR #280; CRD ch.28 T14.
- **Two more tests for HD wallets.** One checks that the automatic UTXO merge under an HD login
  really sends a merge: one transaction that spends only the outputs of the coin's activated
  address, pays them back to it and is signed with that address's key. The other pins that an
  `address_id` of 2^31 − 1 in `path_to_address` fails activation like 2^31 does, before anything is
  derived, so the highest usable index is 2^31 − 2. The node itself is unchanged. Detail: PR #297;
  CRD ch.05 R35a, R35b.
- **CI runs the withdraw tests of `coins` and its QTUM, BCH and SLP tests.** A new CI job runs
  `cargo test -p coins --lib -- withdraw qtum bch slp`, which until now ran only on developers'
  machines. It runs without the `live-network-tests` feature, so it skips those of the selected
  tests that need public servers and runs the offline ones. The node itself is unchanged. Detail:
  PR #297.
- **The BCH withdraw test retries when the testnet answers "Missing inputs".** The test broadcasts
  to the public BCH testnet from a wallet that every CI run shares. It already retried a broadcast
  rejected as a mempool conflict. A broadcast rejected with "Missing inputs" failed the test
  instead; it is now retried the same way. The node itself is unchanged. Detail: PR #297.

- **The Tendermint swap code has its first swap-flow tests.** 50 unit tests drive an Iris and a
  Nucleus coin against a scripted test node on a loopback port: the HTLC messages, the payment checks
  and searches, the spend search, the refund answers, the check of the DEX fee, the secret
  extraction, the confirmation wait and the activation check. Three of them still record behavior
  that a later change replaces: the payment check does not compare the time lock, and a refund that
  the chain makes by itself is reported as a failed refund. Detail: PR #299; CRD ch.18 §18.3, §18.5, §18.11.

### Documentation

- **The design documents, plans and release documents were kept current through the cycle.** The
  CRD gained a coverage note on three unmerged reference pull requests, chapter 63 on TRON GasFree
  fee delegation, and a correction of the Radiant relay-fee finding; plans were opened for the
  mmrpc 2.0 forms of the legacy-only methods, refreshed for TRON and trading floors, and brought in
  line with their states before this release; the compatibility documents, the release notes and
  the release-tests page were updated at each batch close. The node itself is unchanged. Detail:
  PRs #202, #212, #295, #296, #298, #304, #307, #311, #316, #317, #318, #325, #334, #335, #336, #337;
  the branch and version housekeeping of the cycle is in PRs #198, #217.

- **The RPC method census notes two names that only KDF Reloaded routes on the legacy
  surface.** `peer_connection_healthcheck` answers on both envelopes here, and the test helper
  `sim_panic` on the legacy one; the census tables and totals, which count the reference, are
  unchanged. Two source comments on the Cosmos withdraw-fee forms now say exactly how each other coin
  family treats them. Detail: PR #330.
- **The published signing key carries the release signing subkey.** The key file in
  `docs/keys/takologi.asc` now includes the ed25519 subkey that signs tags and releases, and
  `SECURITY.md` names that subkey, gives the keyserver explicitly, notes that the key file in tags up
  to `v0.3.0-beta.1` lacks the subkey, and describes the `SHA256SUMS` check. The key's fingerprint
  is unchanged. Detail: PR #196.
- **The CRD records two more batches of reference answers.** The first covers how a cancelled order
  can come back on the reference and how a relay answers while its order-book worker lags, when the
  expiry drain runs, the arithmetic of an explicit `UtxoFixed` withdraw fee, and the gas of the
  legacy taker-fee transaction (PR #222; CRD ch.12 R14, ch.32 R-GH11 to R-GH13, ch.38 R38.6.6l,
  ch.57 R57.5.2a). The second covers the per-coin protocol information on orders, which key a
  global-HD login uses on each surface, the watcher's payload and fee check, sender selection under
  HD, the SLP withdraw fee, legacy `enable` under an HD login and the Ironwood address facts (PR
  #232; CRD ch.05 §5.9B, ch.09, ch.49 R49.27, ch.51 R74, ch.55 R55.4.10, ch.59 R59.4.7). The node
  itself is unchanged.
- **The CRD now records more of how Komodo DeFi Framework and GLEEC KDF behave.** It covers HD
  activation and accounts (the address at `path_to_address`, `fetch_utxos`, `consolidate_utxos` and
  `task::create_new_account`), the answer to a shielded withdrawal, the Electrum
  certificate-verification switch, which swap steps publish on the transaction-helper topic, how an
  order-book sync answer is handled when a newer keep-alive arrives, the activation routes of TRON,
  and how the gossip layer treats a peer that reads slowly. The node itself is unchanged. Detail:
  CRD PR #267; CRD ch.05 R35a–R35c, ch.09 R24, ch.21 §21.4, ch.28 RQ1b, ch.32 R-Q6b, ch.38
  R38.6.4m, ch.39 R39.5A.3, ch.49 R49.27b.

- **The design notes record that no released GLEEC KDF build has TRON swaps**, and describe the
  public TRON swap contract, its testnet deployment and the state a TRON swap reads, as the basis for
  the TRON swap work. The node itself is unchanged. Detail: PR #333; CRD ch.21 §21.11B.

### Plans touched

Every plan this release touched in any sense since v0.3.0-beta.1, with what happened to it. Each plan
file carries the detail. "New" means the plan was created in this cycle.

- [`bc2-replay-protection`](docs/plans/bc2-replay-protection.md) — new, parked: the Bitcoin II
  network has required an extra field in every signature digest since block 57,750, so
  transactions signed the ordinary way are rejected. The rule is written (CRD chapter 61, PR
  #261), and the signing change and the counterparty requirement are implemented (PR #264). On the
  live chain a withdrawal and a swap between two KDF Reloaded nodes passed, and the decline of a
  counterparty that does not sign with the suffix was checked (release tests, "BC2 confirmation" and
  "BC2 swap check"). The KDF Reloaded coins file carries the `BC2` and `BC2-segwit` entries since
  2026-11-10, and the shared coins repository carries the key on both since 2026-11-14; the P2PKH
  live check is open.
- [`arrr-htlc-lightwalletd-txwalk`](docs/plans/arrr-htlc-lightwalletd-txwalk.md) — planning: its target now reads "after the 0.4.0 beta".
- [`arrr-ironwood-compatibility`](docs/plans/arrr-ironwood-compatibility.md) — done: the field tests of 2026-10-26 to
  2026-10-29 are recorded, and the two open items of the earlier deadline are closed as history.
  Its last gate, a live HTLC round trip from the desktop wallet on Windows (gate 8), passed with an
  ARRR buy on 2026-11-11 and an ARRR sale on 2026-11-12.
- [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md) — planning: what
  Komodo and GLEEC KDF do with a Pirate `pirate1…` address is now recorded, and so is the fact that
  they would still trade with a KDF Reloaded ARRR order that adds a capability marker after its
  address format, without noticing the marker (PR #232). The scope for this release is set: ARRR
  wallets and swaps stay Sapling-only, payments from Pirate Chain's Ironwood wallets are found and
  spent like any other, and a withdrawal may pay a Pirate1 address (CRD PR #229). The deposits are
  proven by tests (PR #242), and the withdrawal is implemented (PR #246). Both passed on mainnet in
  the SwapLab on 2026-11-06 (release tests, rows ARRR-L2 to ARRR-L6); the pending display those rows
  found missing before mining is fixed (PR #263). Native mode, the rest of the Ironwood pool and
  Pirate1 receiving addresses move to a later release.
  The capability word that lets ARRR require a counterparty that builds version-6 transactions is
  written (CRD PR #227), and its generic part, the word's writer and fail-closed reader and the
  decline log lines, is implemented (PR #235); the ARRR rule that uses it is written (CRD PR #230)
  and implemented (PR #241).
  The shielded history gained three items: a withdrawal's Pirate1 recipient (PR #268), the addresses
  of a swap's own transactions (PR #277) and the fee of an own send (PR #288).
- [`crate-versioning`](docs/plans/crate-versioning.md) — new, planning: the start-up banner now shows
  the version of the release package (PR #210); where that version should live, and whether the
  `version` call and swap records should carry it, are listed as open questions for the next round.
- [`electrum-measured-server-order`](docs/plans/electrum-measured-server-order.md) — done: shipped in PR #144; the lab rows
  of 2026-11-06 that ran `mixed` and `measured` on RIN are recorded as input; `mixed` is the default
  since 2026-11-14 (PR #349).
- [`electrum-server-staleness`](docs/plans/electrum-server-staleness.md) — new, done: the Electrum
  staleness checks and the activation-stall fix shipped in PR #324, the fixes of the chain comparison
  in PR #339, and step 6, the `mixed` default, in PR #349.
- [`electrum-tls-server-authentication`](docs/plans/electrum-tls-server-authentication.md) — new,
  parked: what an `SSL` Electrum server must present, with and without `disable_cert_verification`,
  is now written down (PR #240). So are a warning at activation when the flag is set and an optional
  pin on the server's key, `spki_sha256`; both are now built (PR #273). The pass-through of the pin
  by the coins file and the wallets, and two review follow-ups, are open.
- [`evm-min-trading-volume`](docs/plans/evm-min-trading-volume.md) — parked: PR #66 is merged; only an end-to-end preview
  test on a real 8-decimal coin is open.
- [`evm-swap-gas-multiplier`](docs/plans/evm-swap-gas-multiplier.md) — blocked: the activation log
  now names the gas limits each EVM coin and token will use and where they come from (PR #200);
  open point 3 is decided, and the fee to send an EVM taker fee is now quoted at the gas limit it
  is sent with (PR #213); options (a) and (b) still wait on the reference maintainers' outcome.
- [`ghost-orders`](docs/plans/ghost-orders.md) — new, parked: cancelled orders that other nodes
  keep showing after a lost cancellation; five work packages for this cycle, the first two being the
  higher sync limits on relays and the request to one other relay above (PRs #205, #207, #208). What
  two later packages waited for is now recorded: how a cancelled order can come back on Komodo and
  GLEEC KDF, and that their makers advertise every non-empty pair at every tick (PR #222). A received
  cancellation now stays final for an hour, and a maker no longer restores an order it cancelled
  (PRs #223, #224). A pair the maker stops advertising is now dropped after the liveness window
  (CRD PR #243, PR #251). A relay now holds a sync request that reaches it before its own sync for
  the pair has started, and an order-book answer waits briefly for a relay that is still storing a
  maker's first orders (CRD PR #258, PR #259). The loaded-relay lab showed the request to one other
  relay clearing a cancelled order one keep-alive interval earlier in half of its cycles. A debug
  line now counts, per such request, which candidate rule would have skipped it (PR #269). A
  fault-free run showed no extra requests at all, so the rule stays as it is (PR #274). The B2 cap
  check from field logs and the repeated-cancel refresh are open.
- [`gossip-send-queue-bound`](docs/plans/gossip-send-queue-bound.md) — done: its follow-ups now name the PRs they shipped in
  (CRD PR #129; PRs #148, #159, #160, #166).
- [`hd-account-storage-on-path-change`](docs/plans/hd-account-storage-on-path-change.md) — new,
  planning: after a change of a coin's `derivation_path`, KDF Reloaded removes the coin's stored HD
  accounts and starts it as a new HD wallet at the new path, where GLEEC KDF keeps the earlier
  accounts in use. This release documents the behavior (Known limitations); the options for a
  friendlier handling are analyzed after the beta.
- [`hd-key-unification`](docs/plans/hd-key-unification.md) — new, parked: under an HD login, one
  swap key per coin and one trading identity for the node. Eight work packages, starting with
  corrections to the documentation (PR #228) and with refusing up front the swaps that cannot
  complete, which is now done (item H1: `swap_key_check`, PRs #231, #233), as is one swap key per
  coin (item H3, PRs #249, #250) and one trading identity for the node (item H4, PRs #253, #254),
  whose design decisions the maintainer confirmed. The maintainer's answers on
  withdraw, the legacy methods, legacy activation and Sia are recorded in it. `RELOADED_VS_GLEEC.md`
  now says what GLEEC KDF does under an HD login, and lists the planned change to legacy `enable` and
  `electrum` with its switch (PR #245). The direct `withdraw` under an HD login and the refusal of
  `from` under an iguana login are done (items H5.1 and H5.5, PR #270); the maintainer set the
  default sender to the coin's activated address (2026-11-06). Activation lists the address at
  `path_to_address`, and `task::create_new_account` works without a hardware wallet (items H5.0 and
  H5.4, PR #279). `fetch_utxos`, `consolidate_utxos` and the automatic merge work under an HD login
  (item H5.3, PR #281). H6 and H7 are not started; the hardware-context work of this release is in
  CRD ch.05 §5.10B.
- [`lightning-activation-features`](docs/plans/lightning-activation-features.md) — parked: it records that PR #66 fixed the crash of
  the trading-volume requests for Lightning coins.
- [`maker-gossip-rate-control`](docs/plans/maker-gossip-rate-control.md) — still parked: the
  queue-signal flaw found in the field is fixed (PRs #204, #214); the other default-on items
  remain.
- [`malloc-arena-cap`](docs/plans/malloc-arena-cap.md) — parked: the 2-arena limit of 0.3.0-beta.1
  was measured in two 6.5-hour runs and on start-up, activation, eight swaps and query latency; no cost
  was resolved and the default stays (PR #225); executor sizing and an allocator comparison wait for
  their triggers.
- [`order-posting-latency`](docs/plans/order-posting-latency.md) — parked: the rule that
  spreads read-only Electrum queries only over the servers with the shortest round trip is written,
  with its two switches and their defaults (PR #201), and implemented (PR #215). The SwapLab timing
  rows ran on 2026-11-06: on RIN orders were posted in a quarter to a third of the time, and on DOGE
  about 11 % more slowly. The floor of two near servers, which brings DOGE back to the time of
  0.3.0-beta.1, is written (PR #260) and implemented (PR #262), and the SwapLab run on it passed
  (release tests, rows 4–8). The opt-in `setprice_replace_after_checks`, which replaces the previous
  orders only after the new order passes its checks, is written (PR #265) and implemented (PR #286);
  a lab row with a failing check passed on 2026-11-08 (release tests, "Setprice option"). Nothing
  else is in flight; the tip check (work package 5) has not started.
- [`orderbook-queue-and-history-caps`](docs/plans/orderbook-queue-and-history-caps.md) — parked: PRs #131, #134, #137 and #164
  are merged; the early-purge point waits for the maintainer.
- [`orderbook-wipe-recovery`](docs/plans/orderbook-wipe-recovery.md) — parked: the CRD and both switches are merged (PRs #132,
  #135); its SwapLab run is not scheduled.
- [`p2p-dialing-and-discovery`](docs/plans/p2p-dialing-and-discovery.md) — new, in progress: the
  peer-to-peer items of 0.4.0. The redial guards are in (PRs #199, #206), and so are the peer-exchange
  logging, the address ranges and the idle-close log (PRs #203, #211), the send-queue window
  shares and the queue-signal fix (PRs #204, #214), and one resolver for every hostname (PRs #216,
  #219); the remaining lookups (Sia, WalletConnect, Lightning, statistics) and encrypted DNS are
  new rows for later. A close that a non-reading peer would hold forever now ends after 30 seconds
  (item P6, PR #280). A hold-off for peer addresses that cannot be reached is a new row for a later
  release (item P9; Known limitations).
- [`rpc-api-reference-wire-parity`](docs/plans/rpc-api-reference-wire-parity.md) — parked: it records that PRs #17 and #21 landed.
- [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md) — parked: it records that PRs #54 and #65 landed,
  with their compatibility bookkeeping, and the 2026-11-09 mainnet swap tests; step 8 (a stricter signing of malformed
  caller-supplied scripts) is a separate post-beta change.
- [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md) — parked: two-daemon detection and F3 are
  postponed and not started.
- [`sia-withdraw-fee-control`](docs/plans/sia-withdraw-fee-control.md) — done: the plan now says
  that the fee estimate uses the transaction's signature-hash length, which is smaller than the
  transaction sent (PR #239; see Known limitations).
- [`solana-support`](docs/plans/solana-support.md) — parked: Solana stays switched off; the release it returns in is not
  set.
- [`swap-transaction-path-hardening`](docs/plans/swap-transaction-path-hardening.md) — planning,
  after the 0.4.0 beta: the stub now also names five areas of the legacy swap process for all coin
  families: refund and recovery decisions, the follow-up of sent transactions, the durability of
  swap records, the recognition of the counterparty's transactions and of the secret, and admission.
- [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md) — parked: Tendermint swaps were added to the
  0.4.0 scope on 2026-11-08 and put aside again on 2026-11-10 by the maintainer's decision; the
  coins stay wallet-only. Accepting both forms of the activation parameters is decided and not
  started. Two of its features are implemented: custom tokens (wallet-only) and the HD
  `path_to_address` (PR #319). The trade-fee row owed by PR #323 passed on 2026-11-10 on a local
  chain.
- [`trading-floors-parity`](docs/plans/trading-floors-parity.md) — done: how Komodo and
  GLEEC KDF charge an explicit `UtxoFixed` withdraw fee (in proportion to the size) is now
  recorded (PR #222), and KDF Reloaded now charges it the same way on UTXO coins (PR #226). On an SLP
  token withdrawal they charge it in proportion too; that is now recorded (PR #232), and KDF
  Reloaded now charges it that way there as well (PR #234). The Sia floor was revised (PR #278), and
  the UTXO change-fee rounding found in the release tests was fixed (PR #247). The default-side arm 3 is chosen
  (2026-11-08): a raised advertised minimum, on by default, with a compatibility switch for the
  reference value; it is merged (PR #315), and the lab row L-MINVOL passed on 2026-11-09.
- [`tron-gasfree`](docs/plans/tron-gasfree.md) — parked: TRON support was added to the 0.4.0 scope on 2026-11-08 and
  stops at the wallet path (PR #333).
- [`tron-support`](docs/plans/tron-support.md) — parked at the 0.4.0 freeze: TRON support was added to the 0.4.0 scope on 2026-11-08; the
  decisions a TRON swap needs were put to the maintainer that day. The activation route was decided
  on 2026-11-08 (work package 1), the wallet path's balance, address and broadcast are wired (work
  package 4), and TRON stays switched off in this beta (decided 2026-11-10). The work stops after PR
  #333, the wallet path; swaps are not built.
- [`tron-swap-support`](docs/plans/tron-swap-support.md) — parked: the chapter rewrite on its branch failed the KDF Dirty
  Gate on 2026-11-08 and is not reused; the reference's TRON swap state is recorded by a gated Spec
  Reader round that lands with the first TRON code PR.
- [`utxo-header-family-selection`](docs/plans/utxo-header-family-selection.md) — parked; new step H6: the
  median time of a coin whose headers carry a fixed-length trailer (Bitcoin Atom) is read from each
  header's own bytes; the 1.6 list form of header requests and explicit `header_layout` entries for
  RIC and MEWC in the coins repository are listed as later items (CRD PR #220, code PR #221). The
  remaining H4 family rules and H5 are open.
- [`v0.2.0-dependency-hygiene`](docs/plans/v0.2.0-dependency-hygiene.md) — in progress: the `idna` advisory row is
  closed by PR #219.
- [`withdraw-entry-expiry-by-block-time`](docs/plans/withdraw-entry-expiry-by-block-time.md) — planning: it records that 0.3.0-beta.1 fixed
  the earlier problem (PR #181); the two ideas left are not in 0.4.0.
- [`zec-v5-transactions`](docs/plans/zec-v5-transactions.md) — new, planning: since the Zcash
  network upgrade NU7 the network accepts only transactions of version 5 or 6, and KDF Reloaded
  builds ZEC transactions in the version-4 format, so ZEC cannot send (Known limitations). The plan
  describes what a version-5 send needs, the options (the Zcash library already in the tree is
  recommended) and the work items.

### Not in this release

Work that was planned or started for this cycle but is postponed to a later version. Each item says
why and where the plan lives; what a user meets today is under Known limitations below.

- **TRON (TRX, TRC20) activation and swaps.** The TRON work stops for this release at the wallet
  path: balance, address and broadcast are in the code (PR #333) but cannot be reached, because TRON
  activation stays off in this beta (see Known limitations). The activation route and TRON swaps
  are not finished. More in [`tron-support`](docs/plans/tron-support.md).
- **TRON GasFree (gasless TRC20 withdrawals).** It is specified (CRD ch.63) but not in this build:
  an earlier implementation stays parked on a branch and is not revived, and the new work needs
  TRON activation and a pinned GasFree proxy creation bytecode first. More in
  [`tron-gasfree`](docs/plans/tron-gasfree.md).
- **Tendermint (Cosmos) orders and swaps.** Tendermint coins stay wallet-only. Orders and swaps were
  added to this release's scope on 2026-11-08 and put aside again on 2026-11-10 by the maintainer's
  decision; they return only with a swap plan of their own and its tests, which do not exist yet.
  The activation features not built yet are in
  [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md).
- **Solana (SOL, SPL).** Activation stays off: the support is not finished, and the Solana SDK the
  build uses is pinned to a version that could only be upgraded through a fork. More in
  [`solana-support`](docs/plans/solana-support.md) and
  [`solana-sdk-upgrade`](docs/plans/solana-sdk-upgrade.md).
- **Siacoin (SC): V2 swaps, a watcher's spend of a maker payment, and a rate-based swap fee.** SC
  swaps use the V1 swap protocol; the V2 path is a draft only. A watcher cannot spend an SC maker
  payment for its owner, a gap no coin has closed yet. Every SC swap transaction pays the fixed
  miner fee of 0.00002 SC; a fee computed from the network's fee rate is postponed, and is needed
  only if transactions stop being mined at the fixed fee. No plan yet. Detail: CRD ch.20 D5 and D7,
  ch.54.
- **The swap watcher.** No swap hands its data to a watcher yet (see Known limitations). A watcher
  framework is planned for the next version; its plan is not written yet.
- **mmrpc 2.0 forms of the legacy-only methods.** Methods such as `setprice`, `cancel_order` and
  `my_orders` answer only legacy requests, as in GLEEC KDF. Versioned forms of them are a candidate
  for the next version. More in [`mmrpc-v2-completion`](docs/plans/mmrpc-v2-completion.md).
- **ARRR (Pirate) native mode, the Ironwood pool and Pirate1 receiving addresses.** This release
  receives Ironwood deposits that carry a Sapling output and pays Pirate1 addresses. Native mode (a
  shielded coin served by its own full node) is refused, and the wallet does not yet hold, scan or
  spend notes in the Ironwood pool or hand out a Pirate1 receiving address. More in
  [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md). Finding a swap's
  transactions by walking the light-wallet server's blocks also follows after this beta: more in
  [`arrr-htlc-lightwalletd-txwalk`](docs/plans/arrr-htlc-lightwalletd-txwalk.md).
- **Lightning orders and swaps.** Lightning coins cannot trade, and the work on their activation
  features and on the LDK library upgrade is parked. More in
  [`lightning-activation-features`](docs/plans/lightning-activation-features.md) and
  [`lightning-ldk-upgrade`](docs/plans/lightning-ldk-upgrade.md).
- **QTUM and QRC20 stabilization.** It is analyzed and planned, but not started. More in
  [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md).
- **A typed error for the Radiant (RXD) signature check.** An RXD swap with a build without the
  Radiant signature dialect did not complete in the 0.3.0 tests (see Known limitations); a typed
  error for the four signature-hash cases the dialect treats as fatal is a later step. More in
  [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md) (step 8).

### Known limitations

What does not work yet, or works only in part, in this release. Each item says where more detail
lives.

- **Under an HD login, legacy `my_balance` fails for a UTXO coin such as RIN, and legacy
  `get_enabled_coins` fails as a whole once such a coin is enabled.** Both report a single address,
  and an HD coin has many. GLEEC KDF fails the same way, and this stays. Use `account_balance` for
  the balance and the mmrpc 2.0 `get_enabled_coins` for the list. For an EVM coin both legacy
  methods answer with the activated address. Detail: PR #228; CRD ch.05 §5.9B R35.
- **Changing a coin's `derivation_path` makes it start as a new, empty HD wallet.** Under an HD
  login with the seed as the key source, when a coin's `derivation_path` in the coins file changes
  between two sessions, the coin's stored accounts no longer match the keys the seed derives under
  the new path. KDF Reloaded removes them and logs one WARN per coin at each activation that removes
  accounts, "removed N stored HD account(s) that the wallet's seed did not derive"; for this case
  the wording is misleading, because the seed did derive them, under the previous path. The coin
  then starts as a new HD wallet at the new path: the first account (or the one `path_to_address`
  names) is created and, by default, its addresses are scanned up to the gap limit. Nothing is lost:
  every address can be derived from the seed again at its path. To see the earlier addresses again,
  restore the previous `derivation_path` and activate the coin. By default its addresses are scanned
  again up to the gap limit. Other accounts are added again with `task::create_new_account`, and
  addresses beyond the gap limit with `get_new_address`. Alternatively, move the funds and keep the
  new path. A coin activated with a Trezor keeps its stored accounts. GLEEC KDF behaves differently:
  the earlier accounts stay in use while the reported path is the new one. No switch changes this in
  this release. Normal use does not change a coin's path; it takes an edit of the coins file, or a
  switch between coins files that give a ticker different paths. Detail: CRD ch.05 §5.10B, status
  note of 2026-11-11; plan
  [`hd-account-storage-on-path-change`](docs/plans/hd-account-storage-on-path-change.md).
- **Do not run two KDF instances on the same seed at the same time.** If you do, for any coin (RIN
  included), both instances can pick the same funds for two different transactions. The network
  accepts one and rejects the other, and the instance that loses gets no explanation and may try
  again. KDF guards against this inside a single instance only; shielded coins such as ARRR are the
  known case. More in [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md).
- **A withdrawal that drops out of the mempool keeps its coins set aside until the coin is enabled
  again or the node restarts.** Once a withdrawal's broadcast is accepted, the node no longer offers
  the coins it spent, so the next withdrawal does not pick them again, and it offers the change
  instead. If the transaction later leaves the mempool unconfirmed, for example because its fee was
  too low or it was replaced, those coins stay set aside and its change stays offered, so a
  withdrawal that picks that change can fail. Disabling and enabling the coin again, or restarting
  the node, clears this. In an HD wallet, change that a withdrawal sends to another address of the
  coin is offered only once the Electrum server lists it. More in
  [`withdraw-entry-expiry-by-block-time`](docs/plans/withdraw-entry-expiry-by-block-time.md).
  Detail: PR #181; CRD ch.38 R38.5B.8, D38.5B.2, D38.5B.4.
- **The swap watcher does not work yet.** A watcher is another node that finishes a swap for you
  when you are offline. No swap hands its data to a watcher yet, for any coin (issue #56), so keep
  your node running until your swaps have finished. Bitcoin-family coins (UTXO coins such as RIN,
  BCH and QTUM) also no longer offer to watch other traders' swaps, because the checks a watcher
  runs are not implemented for them; only Siacoin (SC) offers watching. Detail: PR #177; CRD ch.09
  R17.
- **Four kinds of connection still look names up through the computer's own resolver,** whatever
  `dns_resolver` says: Sia, the WalletConnect relay, Lightning peer addresses and the statistics
  method. They follow in a later release. More in
  [`p2p-dialing-and-discovery`](docs/plans/p2p-dialing-and-discovery.md), item P5b. Detail: CRD
  ch.45 D45.6.
- **A maker that changes its orders many times a second can drop out of the order book of a relay
  with a slow link.** In a test, a node that kept 480 orders and changed their prices about 80 times
  a second sent its updates to a GLEEC KDF relay whose incoming link was limited to 100 kbit/s. Within
  2 to 4 minutes the orders were gone from that relay's order book, and from the nodes behind it, for
  the rest of the test; the maker itself reported no errors. Setting the `MM2.json` boolean
  `maker_update_coalescing` to `true` kept all 480 orders visible in the same test. It is off by
  default because it has a cost: price changes reached other nodes about 20 to 25 seconds later, for
  5 minutes after the relay fell behind other nodes learned changes only through the keep-alive
  synchronization, and the relay logged 6 to 18 failed order-book synchronization requests a minute.
  These costs were measured before this release changed when the slowdown ends (PRs #204, #214), and
  were not measured again. More in
  [`maker-gossip-rate-control`](docs/plans/maker-gossip-rate-control.md). Detail: PRs #204, #214;
  CRD ch.32 D9.
- **Other nodes can keep showing a cancelled order for a while if its cancellation is lost on the
  way and the maker still advertises that pair.** Such an order cannot be matched or traded. A pair
  the maker no longer advertises is dropped after the liveness window (90 seconds, or
  `maker_order_timeout`), with the maker's other pairs kept. A
  pair the maker still advertises heals at its next keep-alive, 30 seconds later, or through another
  relay: a node whose copy differs from the maker's asks one other relay, which removes the order if
  that relay received the cancellation. Until then the order stays visible. More in
  [`ghost-orders`](docs/plans/ghost-orders.md). Detail: PRs #188, #208, #224, #251; CRD ch.32 D12,
  R-Q5d, R-GH14.
- **The log warns repeatedly about peer addresses that cannot be reached.** A node logs warnings
  such as "P2P outgoing connection failed for peer …: Failed to negotiate transport protocol(s):
  […]", where the brackets hold the operating system's connection-refused text ("os error 10061" on
  Windows) or "Timeout has been reached", at most once per hour for each address. The wording comes
  from the transport library; no protocol was negotiated, because each of these is a TCP connection
  that was refused or timed out. The addresses are peer records that other nodes hand out in the
  peer exchange and that no longer accept connections. KDF Reloaded relays store and pass on such
  records the same way: a record is dropped only when a dial reaches a different peer, not when the
  address is unreachable. The node asks a random known peer for more peers every 5 minutes; when the
  dial fails, it drops that peer and asks another at once, and a live seed's next answer hands out
  the same records again. A node that is not a relay and has fewer than 2 relays also dials them
  while looking for relays, and each dial tries the address twice. In a 29-hour run of the desktop
  wallet on Windows, 557 such warnings named 33 addresses of 32 peers; a check of all 33 on
  2026-11-12, after the run, found none that accepted a connection (11 refused, 22 gave no answer);
  two of them are the addresses that names still used for seed nodes resolve to. The effect is log
  noise and wasted dials, and a peer-exchange request sent to such a peer waits for the dial to fail
  before another peer is asked. Order books and swaps were not affected in that run, in which the
  node stayed connected through its six configured seeds. A hold-off for unreachable addresses is
  planned for a later release. Detail: CRD ch.28 R8c item 6; plan
  [`p2p-dialing-and-discovery`](docs/plans/p2p-dialing-and-discovery.md).
- **ARRR (Pirate) atomic swaps work only between two KDF Reloaded nodes of this release or later.**
  ARRR now uses a newer transaction format (v6, from the Ironwood upgrade) that no other KDF
  implementation can handle, so a swap with a Komodo or GLEEC counterparty cannot complete (observed
  on 2026-10-28: a GLEEC KDF maker's first ARRR payment was rejected at broadcast). An ARRR node
  therefore declines such a counterparty before the swap starts, and KDF Reloaded 0.3.0-beta.1 nodes
  as well (see Changed). A swap between two KDF Reloaded nodes has completed on the live chain,
  including the spend of the locked ARRR (October 2026). The decline rests on a capability
  statement in the order and match messages (the setting `require_v6_counterparty` in the coin's
  `consensus_params`, on by default). This check is temporary: it will be removed once version 6 is
  generally supported by the other implementations. More in
  [`arrr-ironwood-compatibility`](docs/plans/arrr-ironwood-compatibility.md). Detail: PRs #230,
  #241, #271; CRD ch.39 R39.6.4f.
- **Shielded coins such as ARRR cannot be activated in a browser (WASM) build.** Activation ends with
  an error saying shielded activation is not available in the WebAssembly build: the browser build
  has no shielded wallet database, and building a shielded transaction would also need the Sapling
  prover, which only the native build has. Use a native build for ARRR. Detail: CRD ch.39 R39.10.1,
  §39.6.1.
- **The ARRR history names no recipient for a withdrawal made before this release, or by another
  node on the same seed, and no transparent address of a transaction another wallet built.** The
  node lists the addresses of the transactions it builds itself, from its own record, so a row
  without a record names only the wallet's own address. A payment that reaches the wallet from a
  transparent address does not list that address, because the node reads only compact blocks and
  fetches nothing more. Swap rows from before this release keep the sets they had. More in
  [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md). Detail: PRs #268,
  #277; CRD ch.39 R39.8.9d2 (d), R39.8.9d3 (d).
- **The ARRR history reports a fee of 0 where the node does not know the fee.** That is the case
  for a transaction another wallet built, such as a deposit, for a swap's spend or refund, and for
  an own send that a rebuilt wallet database found by scanning. The node reads compact blocks,
  which carry no fee data, and fetches nothing more. Detail: PR #288; CRD ch.39 R39.8.9e1 (b), (c).
- **An ARRR withdrawal takes no memo, and its answer has no `memo` member.** The `withdraw`
  request has no `memo` field, so a memo sent with it is ignored and the shielded output carries
  none. Komodo DeFi Framework and GLEEC KDF echo the request's memo in the answer. Memo support is
  a separate, later item. Detail: PR #271; CRD ch.39 status note under R39.5A.3.
- **ARRR has no Pirate1 receiving address.** The wallet can pay a Pirate1 address, but it receives,
  keeps its change and swaps in Sapling only. Native mode and the rest of the Ironwood pool are
  planned for a later release. More in
  [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md). Detail: PRs #229,
  #246; CRD ch.39 §39.6.5.
- **ARRR swaps use the Sapling pool only.** An ARRR swap's HTLC payment is funded from the Sapling
  pool, and its spend and refund pay into the Sapling pool. The Pirate network accepts an HTLC
  payment only in a transaction that spends at least one Sapling note, so a payment funded from the
  Ironwood pool alone is invalid; changing that needs a Pirate network upgrade, so KDF Reloaded
  cannot offer swaps funded from the Ironwood pool until then. Funds held at an Ironwood (Pirate1,
  `pirate1…`) address, for example in another Pirate wallet, must be sent to the wallet's Sapling
  (`zs1…`) address before they can be traded. Withdrawals to a Pirate1 address, and incoming
  payments that carry a Sapling output, keep working as described in the ARRR entries above. More in
  [`arrr-ironwood-compatibility`](docs/plans/arrr-ironwood-compatibility.md). Detail: CRD ch.39
  §39.6.5.
- **The Komodo DeFi Framework and GLEEC KDF builds available in November 2026 cannot spend BC2
  (Bitcoin II).** Since block 57,750 the BC2 network rejects every spend signed without the chain's
  sighash suffix, and those builds do not sign with it, so they can neither withdraw BC2 nor
  complete a BC2 swap with any counterparty. KDF Reloaded declines a BC2 swap with such a node
  before it starts, and signs with the suffix (PR #264), but only for a coin entry that carries
  `"sighash_suffix": "0x01324342"`; for an entry without the key, a BC2 withdrawal or swap payment
  is refused at broadcast ("Signature must be zero for failed CHECK(MULTI)SIG operation"), and the
  node logs a warning when BC2 activates without the key. A hardware wallet cannot sign this digest,
  so BC2 cannot be used with one. The optional key `sighash_suffix_height` is checked but not used
  yet. The shared coins repository carries the key on `BC2` and `BC2-segwit` since 2026-11-14; a
  coins file from before that date needs `sighash_suffix` (`"0x01324342"`) added to both entries, or
  use the KDF Reloaded coins file. Detail: PRs #261, #264; CRD ch.61; issue #252.
- **A browser (WASM) build cannot pin an Electrum server's key.** It removes a server entry that
  carries `spki_sha256` and uses the coin's other servers. Wallets and the shared coins file may not
  pass `spki_sha256` through to activation yet. Detail: PRs #240, #273; CRD ch.38 R38.6.4n,
  D38.6.4n.1.
- **Under a Trezor key, some UTXO operations are not available.** The direct `withdraw` answers an
  internal error (use `task::withdraw`), `consolidate_utxos` answers `InvalidAddress`, and the
  automatic merge (`utxo_merge_params`) stops; `fetch_utxos` works. Detail: PRs #270, #281; CRD
  ch.49 R49.4, ch.05 R35b.
- **The miner fee of a Siacoin withdrawal is estimated from a transaction length that is smaller than
  the transaction sent.** Sia nodes accepted these fees in tests. Detail: PR #239; CRD ch.20 D6.
- **Orders that sell Siacoin (SC) are not shown to Komodo KDF and GLEEC KDF takers, and a GLEEC maker
  can fail to validate a Siacoin taker fee.** Takers on those two nodes do not see orders whose base
  coin is SC, so a KDF Reloaded maker that sells SC cannot be matched by them. A GLEEC maker can fail
  to validate a Siacoin taker fee when the transaction pool of its Sia node holds a transaction it
  cannot read (a storage proof whose proof field is null). KDF Reloaded reads past such a transaction
  since this release (PR #291); the GLEEC release does not, so such swaps fail intermittently. A
  GLEEC node that sells SC as the order base is not affected. Detail: PRs #239, #291; CRD ch.20
  R-N4.
- **Siacoin (SC) swaps: what is not covered.** SC swaps run on the first swap protocol only; the
  second protocol is not implemented for SC. A node that watches other traders' swaps can refund a
  Siacoin payment for an offline taker but cannot spend the maker's payment for it. Every Siacoin
  swap transaction pays a fixed fee of 0.00002 SC, about 450 to 850 times below the rate the Sia node
  recommended for the transaction's size in the tests; every such transaction so far was mined in
  the next block, and the fee has not been tested under load. Manual recovery (`recover_funds_of_swap`) of a Siacoin swap
  was tested only on swaps whose record already held the final event; it refused each time and
  broadcast nothing, as it should, so the recovery method's chain reading for a Siacoin payment has
  unit tests only; a taker that was stopped after its Siacoin payment and resumed after its lock did
  find the maker's spend on chain and completed the swap (November 10, 2026). Siacoin in a browser
  (WASM) build is untested. A GLEEC KDF
  3.0.0-beta maker on netid 6133 validated a KDF Reloaded taker's Siacoin fee at the network's base
  rate on November 10, 2026, with its Sia node's pool reply passed through a lab proxy that rewrites
  the entry it cannot read (see the item above), and that swap completed. The same check at netid
  8762's discounted rate, with KMD in the pair, passed the same day against a GLEEC KDF development
  build, again behind that proxy, standing in for the netid-6133 discounted case (GLEEC in the pair),
  which needs GLEEC and was not run; without the proxy the development build failed twice at the pool
  reply (see the item above). The fee rule and the fee address are the same on both nodes. Detail:
  PRs #239, #278, #290, #291; CRD ch.20 D5, D7, R-S8, R-S11, R-S12, T-S1.
- **Lightning cannot place orders or swap.** Orders and trade previews involving a Lightning coin
  are refused with an error naming the coin; `trade_preimage` answers `TradingNotSupported`. More in
  [`lightning-activation-features`](docs/plans/lightning-activation-features.md). Detail: PR #66; CRD
  ch.60 R60.6.5.
- **Radiant (RXD) swaps are tested between KDF Reloaded nodes and with GLEEC KDF 3.0.0-beta.** An
  RXD swap with Komodo DeFi Framework 2.6.0-beta, a build without the Radiant signature dialect,
  did not complete in the 0.3.0 tests. More in
  [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md).
- **SLP tokens on BCH are untested on mainnet.** The shared coins file lists BCH and tBCH as plain
  UTXO coins and carries no SLP token entries, so SLP tokens cannot be enabled with it. The SLP
  changes of this release are covered by unit tests only. Detail: PRs #234, #236.
- **ZEC cannot send since the Zcash network upgrade NU7.** Since NU7 activated on the Zcash mainnet
  (height 3,543,405), the network accepts only transactions of version 5 or 6 (ZIP 2003). KDF
  Reloaded builds ZEC transactions in the version-4 format, so the network rejects every ZEC
  withdrawal, swap payment and refund; a field report shows the rejection "transaction version 4 not
  supported by the network upgrade Nu7". The balance, read from the Electrum servers, should still
  show incoming payments (not tested); transactions made since NU7 may be missing from the history
  or shown wrongly, because the node does not read the version-5 format yet. Do not place or take
  ZEC orders: a swap fails at its first ZEC transaction, and a ZEC refund cannot be sent either. The
  other Zcash-family coins of the coins file (KMD, ZER, BTCZ, FLUX) run their own networks; NU7 is
  an upgrade of the Zcash network only. Detail: plan
  [`zec-v5-transactions`](docs/plans/zec-v5-transactions.md); CRD ch.39 R39.11.15.
- **TRON (TRX, TRC20) and Solana (SOL, SPL) cannot be activated in this beta.** Their support is not
  finished. TRON activation answers "TRON support is not available in this build";
  `enable_solana_with_tokens` and `enable_spl` answer "Solana support is not available in this
  build"; the rest of the node is unaffected. TRON stays switched off in this beta because its
  wallet path (balance, address and broadcast) was only now wired, its node client has no time limit
  per request and no API-key handling yet, its transaction history is not available, and its
  activation route and swaps are unfinished; it returns in a later version. Solana stays switched
  off as well: the Solana SDK the build uses is pinned to a version that could only be upgraded
  through a fork. More in
  [`tron-support`](docs/plans/tron-support.md) and [`solana-support`](docs/plans/solana-support.md).
  Detail: PRs #93, #333.
- **Tendermint (Cosmos) coins are wallet-only in this beta.** Activation, balances and withdrawals
  work; orders and swaps are planned to return in a later release. A swap already in progress still
  completes, and an order with a Tendermint coin saved by an earlier version is cancelled at startup.
  More in [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md). Detail:
  PR #93.
- **Swaps of Monad tokens need a coins file from October 28, 2026 or later.** On Monad, a token's
  swap payment needs more gas than the default limit of 150000 (about 158000 for USDC-MON). Coins
  files from that date on give each Monad token, such as USDC-MON, its own `gas_limit` block, which
  KDF Reloaded uses. With an older file the payment runs out of gas and fails on-chain: its gas fee
  is spent, the tokens stay in the wallet and the swap stops. A counterparty whose payment is already
  locked gets it back by refund once the swap's lock time passes. With an older file, the token's
  activation log line says `gas_limit: defaults`. More in
  [`evm-swap-gas-multiplier`](docs/plans/evm-swap-gas-multiplier.md). Detail: PR #105, PR #200; CRD
  ch.57 R57.5.2, R57.5.3, R57.5.5.
- **An EVM request fails at once when every node of the coin refuses it.** When every configured
  node of an EVM coin refuses a request at the same moment (for example public endpoints answering
  HTTP 429 Too Many Requests), the request fails at once; KDF Reloaded does not pause and retry.
  Order placement and balance RPCs report the failure and can be repeated; a swap step without a
  wait loop (taker-payment validation, the spend) fails the swap into its refund path. For an EVM
  coin used for making orders, configure at least one node with its own request allowance. Detail:
  CRD ch.35 R35.1.8, R35.1.11.
- **A maker payment is not refunded after the spend of the taker payment was sent, until the other
  side's refund is shown.** If the node's spend of the taker payment was broadcast but dropped from
  the chain, `recover_funds_of_swap` refuses to refund the maker payment while the other side's
  payment is not refunded: the other side knows the secret from the broadcast and can still claim it.
  Once the other side's refund is on its own chain with a confirmation, the call refunds the maker
  payment (Fixed). Detail: PRs #276, #284, #345; CRD ch.51 R75, R83, D8.

## [0.3.0-beta.1] — 2026-11-01

Within each heading, entries are grouped by area. Each entry ends with the pull request that made
it and, where one exists, the CRD rule, for readers who want more.

### Highlights

The most important changes of this release in short. Each item points to the detailed entry below.

#### What is new

- **Seeds and relays cope with slow peers, send each message once and use seeds handed out by
  name.** PRs #122, #172, #178, #185.
- **Seeds, relays and busy makers hold much less memory under heavy order traffic.** PRs #131,
  #134, #137, #164, #190.
- **Order posting is faster, with fewer Electrum and node requests.** PRs #114, #128, #154.
- **Electrum servers are kept in priority order, recover and can be ranked by performance.**
  PRs #104, #128, #144.
- **ARRR swaps work on the Ironwood format, with refunds and recovery that hold up.** PRs #109,
  #117, #130, #142, #146, #152.
- **EVM swaps use the coins file's gas limits and typed transactions, and leave a dead node within
  10 seconds.** PRs #24, #25, #90, #105, #107, #174.
- **Trading floors and minimum volumes match the reference.** PRs #66, #72, #73.
- **Radiant (RXD) signs with its own signature hash, and Siacoin withdrawals take a custom fee.**
  PRs #65, #77.
- **Logs hide node addresses and credentials, TLS moved to rustls 0.23, and releases ship the
  license files.** PRs #36, #39, #40, #42, #161, #165.
- **Compatibility switches restore GLEEC KDF behavior where KDF Reloaded differs.** More in
  [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md).

#### Major fixes

- **A taker that falls back to a refund now takes the maker's coins** when the maker has already
  claimed the taker's payment. PR #167.
- **A swap that resumes after a restart no longer gives up on a payment it already sent.** PR #119.
- **After a restart, the transaction history of UTXO coins no longer lists every earlier transaction
  twice.** A history already doubled heals itself. PR #81.
- **Swaps on merged-mined (AuxPoW) coins can be completed and refunded again**, and PIVX, LBRY (LBC),
  RIC and MEWC swaps can complete. PRs #55, #62, #67, #82.
- **ARRR wallets no longer wedge after a reorg of their scan tip** or pile up checkpoint backups when
  their sync keeps failing. PRs #31, #150.
- **A node no longer drops a market maker's whole order book when only its keep-alives are
  delayed.** PR #132.
- **Lighter networking and bookkeeping.** Gossip message ids use the compact form other KDF nodes use,
  and a node no longer keeps memory for every swap it has ever run. PRs #99, #100.

#### Warnings and gaps

All of these are described under Known limitations below.

- **Do not run two KDF instances on the same seed at the same time.**
- **The swap watcher does not work yet**, so keep your node running until your swaps have finished.
- **ARRR swaps work only between two KDF Reloaded nodes**, and shielded coins cannot be activated in
  a browser (WASM) build.
- **Siacoin (SC) swaps are not available, and Radiant (RXD) is only partly usable.**
- **TRON and Solana cannot be activated in this beta**, and Tendermint (Cosmos) coins are wallet-only.
- **Swaps of Monad tokens need a coins file from October 28, 2026 or later.**
- **Withdrawals:** an explicit fixed fee can cost more than on Komodo or GLEEC KDF, and a withdrawal
  that drops out of the mempool keeps its coins set aside until the coin is enabled again or the
  node restarts.
- **A maker that changes its orders many times a second can drop out of the order book of a relay
  with a slow link.** The opt-in setting `maker_update_coalescing` keeps the orders visible.
- **Other nodes can keep showing a cancelled order for a while** if its cancellation is lost on the
  way; such an order cannot be traded.

### Added

- **Orders too small to be spent at current fees are now refused.** `setprice`, `update_maker_order`,
  `buy`, `sell` and `trade_preimage` refuse an order whose smallest trade would leave a UTXO-family
  or Sia payment unspendable, meaning below the current spend-fee estimate plus dust, or the Sia
  miner fee plus one hasting. The refusal is the usual volume-too-low error, with that amount as the
  threshold. Example: BTC at 0.0001 when the fee is high. The minimum volume an order advertises does
  not change, and peers see nothing new. Set the `MM2.json` boolean `pre_trade_spend_check` to `false`
  to restore GLEEC KDF behavior, with no check. Detail: PR #73; CRD ch.60 R60.6.7.
- **Metrics for the gossip send queues and for the maker's pending order messages.** The gauges
  `p2p.send_queue.len`, `p2p.send_queue.max_per_peer` and `p2p.send_queue.dropped.count` show the
  messages queued toward peers, the longest single queue and the messages dropped since start.
  `p2p.outbox.pending`, `p2p.outbox.rate` (in thousandths of a message per second),
  `p2p.outbox.pressure` (1 while a relay's send queue is backed up), `p2p.outbox.degraded` (1 while
  order updates are held back), `p2p.outbox.signals.queue.count` and `p2p.outbox.signals.close.count`
  show the order messages waiting to be sent, the release rate, whether the relays are falling
  behind, and how often a relay did. They carry no peer and no order. Detail: PR #148; CRD ch.32
  §32.9B.
- **Radiant (RXD) now activates and signs with Radiant's own signature hash.** A UTXO coin whose
  `signature_version` is `fork_id_rxd` signs every input with the Radiant dialect: the Bitcoin-Cash
  BIP-143 preimage with one extra digest, a 76-byte summary per output, inserted before the outputs
  digest. RXD's shipped entry therefore no longer refuses to activate, and needs no switch.
  Withdrawals, swap transactions and `sign_raw_transaction` all use it. The digest is checked
  against two Radiant mainnet transactions, one of whose outputs carry Radiant references. Shapes
  with no on-chain check yet: duplicate references, the three reference opcodes that are skipped,
  and sighash modes other than `ALL | FORKID`. RXD's fitness for swaps beyond signing is not
  established, so this does not announce RXD trading. Detail: PR #65; CRD ch.58 §58.3, §58.5.
- **A custom fee for Siacoin withdrawals.** A Siacoin `withdraw` now honors the fee you enter in the
  wallet's custom-fee box, given as SC per kilobyte; leave it out and the fee is estimated as before.
  Any positive fee is accepted, including one below the estimate; a fee one tenth of the estimate was
  accepted and mined within about ten minutes on an uncongested transaction pool (checked live
  2026-10-26; a full pool is untested). Any other fee type sent for Siacoin is now rejected with an
  invalid-fee-policy error instead of being silently ignored. More in
  [`sia-withdraw-fee-control`](docs/plans/sia-withdraw-fee-control.md). Detail: PR #77.
- **Activating a shielded coin without `sync_params` now logs a warning when the default recent scan
  window applies.** Without `sync_params` only a recent window is scanned (2,880 blocks, two days at
  a 60-second block time), so funds older than that stay invisible while the sync completes
  normally; this is why a migrated ARRR wallet can read a zero balance. The warning names the start
  height and window size, notes that earlier funds will not be visible, and says to re-activate with
  `sync_params`; `first_sync_block` remains the machine-readable signal. The reference scans an even
  shorter window, so the remedy is the same there: pass `sync_params`. More in
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md#zhtlc-zero-balance-after-migrating-a-wallet).
  Detail: PR #61, PR #57; CRD ch.39 R39.8.0aw.
- **A shielded (ARRR) withdrawal short only because of an unconfirmed transaction now says so.**
  Instead of `NotSufficientBalance` with `available: 0` beside a healthy balance, it fails with
  `BalanceLockedByUnconfirmedTransaction` (HTTP 400) naming the amounts and that it clears when
  that transaction confirms. After a node restart the bare error is reported again. Detail: PR #50.
- **Releases and the runtime Docker image now ship the license files.** `COPYING`, `LICENSE` and
  `THIRDPARTY-LICENSES` are attached to every release and covered by the signed `SHA256SUMS`, and
  the image carries them in `/usr/share/doc/kdf/`. Detail: PR #36.

### Changed

#### Order book and peer-to-peer network

- **A maker that changes its orders faster than its relays can take them can keep its order book
  (opt-in).** Every node drops a maker 90 seconds after its last keep-alive. A relay that reads
  slowly, for example on a busy host, used to leave the keep-alive stuck behind a pile of order
  updates, so the book vanished on every node. Since PR #132 a Reloaded node keeps the book while
  the maker's order messages arrive; nodes running GLEEC KDF or Komodo do not. This protection is
  **off by default** for now: set
  the `MM2.json` boolean `maker_update_coalescing` to `true` to turn it on (only a JSON `true`
  does). Without it, every order message is sent at once, as GLEEC KDF does. A field run showed
  that, in a network where relays and makers also run GLEEC KDF, held-back updates make peers ask
  relays for order state the relays do not have yet, and an ordinary relay disconnect lowers the
  rate. With it turned on, the maker keeps one pending message per order, the latest values win, and
  the messages leave at a bounded rate (at most 20 per second per maker key, a burst of 200). When a
  relay stops keeping up, either its send queue stays backed up for 2 seconds or the last connection
  to it closes while order messages are in flight, the rate drops sharply to 0.5 per second and
  order updates stop being sent. Peers then learn the changes through the keep-alive
  synchronization. Five minutes after the last such signal the rate climbs back by 0.5 per second
  every 30 seconds, and stops one step below the rate at which the relay failed; after 30 quiet
  minutes it climbs to the ceiling. A keep-alive now waits up to 10 seconds for the pending messages
  of its pairs to go out within that rate, and no longer forces them out; it does not wait at all
  while the rate is lowered. A cancellation or a swap negotiation message is never delayed and jumps
  ahead of queued order updates. A peer may see an order's latest price a moment later, or only at
  the next keep-alive synchronization, and may never see an intermediate value of an order that
  changes quickly. Your own order list and the answers of `setprice`, `update_maker_order` and
  `cancel_order` do not wait. After a relay connection is restored the maker sends an extra round of
  keep-alives. One short gap remains: the first signal comes only after the load has started, so a
  burst of changes can still cost one expiry of the maker's orders per new relay connection. Set the
  `MM2.json` number `maker_update_rate_max` to change the ceiling of 20 per second. More in
  [`maker-gossip-rate-control`](docs/plans/maker-gossip-rate-control.md). Detail: PR #148, PR #159,
  PR #166; CRD ch.32 §32.9B, D8, D9, ch.28 §28.9B.
- **`update_maker_order` no longer broadcasts an update that changes nothing.** A market-making bot
  that re-submits all of its orders on every refresh used to send one message per order to every
  node, even when price, volumes and confirmations stayed the same. Such an update is still saved and
  answered as before, but no message goes out, so the network carries only real changes. Set the
  `MM2.json` boolean `suppress_unchanged_order_updates` to `false` to broadcast every update again.
  Detail: PR #111; CRD ch.59 R59.4.6.
- **Quieter peer-to-peer logs on seeds and relays under load.** Warnings that report expected
  conditions, not faults, are now lower or summarized:
  - "Message not in cache" is now debug. It meant a relay's second copy of a message came too late;
    the first copy had already been forwarded.
  - "Order … was recently cancelled" is now debug. It meant a late copy of another maker's
    cancelled order was ignored.
  - "HEARTBEAT: relays low" is now "Relay mesh below its low watermark". It is logged when the
    relay count changes and every ten minutes, not every ten seconds.
  - A request that times out is now a warning, not an error. Further timeouts toward the same peer
    are counted in one line per minute. The same holds for requests that fail because the
    connection to the peer closed.
  - A failed outgoing connection is a warning at most once per address per hour, and debug
    otherwise. Relay upkeep keeps trying addresses that never answer, such as makers behind a
    firewall.
  - An incoming connection that fails to negotiate its transport, such as a scanner probing the
    WebSocket port, is logged at info level once per source address per hour, then at debug level.
  - "Request … is not processed" is now debug and no longer prints the answer. It meant an answer
    arrived after the request had already been given up.

  Detail: PR #170, PR #176.
- **Seed and relay logs now show how order-book synchronization is going.** When a node asks
  another for a maker's orders, or answers such a request, and it fails, the log at the default
  level now shows a summary once a minute. The summary counts the answers this node gave with an
  error, and why: no data for the maker, a pair it does not hold, a pair it is still synchronizing,
  or an internal error. It also counts this node's own requests that failed, and why: no data, an
  error from the peer, a timeout, a lost connection or an answer it could not read. At debug level
  each request and answer gets one line, with the peer, the maker, the pairs and the time taken.
  Nothing is logged when all went well, and nothing changes on the wire. Detail: PR #179.
- **Peer exchange now also uses seeds that other peers hand out by name.** Nodes ask each other for
  peer addresses every five minutes. Some peers that do not run Reloaded give a seed by its
  hostname, such as `/dns/seed02.kmdefi.net/tcp/42845`, instead of its IP address.
  - **Lookups.** Reloaded now looks such a name up through the operating system's resolver, one
    name at a time and away from the network task. It keeps the result only if it is a public
    IPv4 address, and stores that address, never the name.
  - **How often.** A name is looked up at most once per 30 minutes, and not at all while the node
    is connected to that peer. When a seed moves to a new IP address, the next lookup replaces the
    old one.
  - **Reloaded's own answers.** They now carry only public IPv4 addresses. Nodes that drop a whole
    list for one hostname therefore keep Reloaded's lists.
  - **Switch.** `"p2p_resolve_peer_hostnames": false` in MM2.json turns the lookups and the
    filtered answers off, as GLEEC KDF behaves.
  - **Tests.** In a private test network, `"p2p_allow_private_addresses": true` in MM2.json accepts
    private addresses as well; the node then warns at start-up that the setting is not for
    production.

  More in [`NETWORK_CONFIG`](docs/NETWORK_CONFIG.md). Detail: PR #184, PR #185; CRD ch.28 R8c items 7
  to 10.

#### Order posting, fees and balances

- **`my_orders` answers faster for an operator with many orders.** With 2,000 open orders a repeated
  call took about 56 ms (decimal prices) to 210 ms (prices such as one third) and now takes about
  12 ms to 18 ms on the same machine; the first call after a restart is as slow as before. The
  answer is byte for byte the same. `setprice`, `update_maker_order` and `order_status` share the
  speed-up for the order they return. If the answer cannot be written, `my_orders` now returns an
  error instead of stopping the node. More in [`my-orders-speed`](docs/plans/my-orders-speed.md).
  Detail: PR #154.
- **Fee readings are reused across order-posting requests.** `setprice`, `update_maker_order`,
  `buy`, `sell`, `trade_preimage`, `max_maker_vol` and `max_taker_vol` (and the maker balance
  watch) may reuse the dynamic-fee estimate of a UTXO coin, the node's minimum relay fee of a coin
  that forces it, and the gas price of an EVM coin that an earlier request obtained. By default the
  time a reading is reused adapts by itself. It starts at the coin's block time in seconds (at
  least 2, at most 600; 15 seconds for a coin without one), doubles each time a fresh reading turns
  out to be the same as the previous one, up to 10 minutes, and goes back to the start as soon as
  the value changes. A chain with a fixed gas floor reaches one gas-price reading per 10 minutes
  without any configuration; a changed value resets it at once. A platform coin and all its tokens
  share one gas-price reading, so a maker with hundreds of token orders on one chain asks the node
  once. To fix the time instead, set `fee_estimate_cache_secs` in a coin's configuration entry
  (0 to 600; 0 turns the reuse off for that coin) or in `MM2.json` (0 to 30, for every coin without
  its own value; a value outside the range stops the node from starting). This is also what lets
  the pre-trade spend check run on every order update again, price-only ones included, without
  extra requests. A swap's own transactions never use the reuse and always read a fresh value. Set
  `fee_estimate_cache_secs` to `0` in `MM2.json`, and set none per coin, to restore GLEEC KDF
  behavior most closely: a request then still reads each value once, but nothing is reused between
  requests. Detail: PR #128; CRD ch.38 R38.6.6j and R38.6.6k, ch.57 R57.2.6 and R57.2.7.
- **Posting and updating maker orders makes fewer Electrum and node requests.** Electrum servers
  serve each connection about one request per second once its first burst is used, so every extra
  request cost about a second. A legacy address such as RIN or DOGE now reads its balance and its
  coins in one request instead of two. Each order request reads a coin's fee estimate and an EVM
  coin's gas price only once, and an EVM gas price's two node queries run in parallel. Detail:
  PR #114; CRD ch.38 R38.6.3a, R38.6.6j, ch.57 R57.2.6.
- **A node now reads a coin's balance only while it has a maker order selling that coin.** It used to
  read the balance of every activated coin every 10 seconds, whether or not any order needed it, and
  those reads competed with order posting for the same throttled Electrum session. Now the balance of
  a coin is read every 30 seconds, and only while at least one of your maker orders has that coin as
  its base; a coin that is only activated, or only the other side of your orders, is not read at all.
  The interval is fixed and cannot be changed. For a market maker this means an order's volume
  follows a deposit or spend within about 30 seconds instead of 10, and a freshly posted order
  is checked against the balance read while it was created. An order restored at start or converted
  from a timed-out taker order is re-checked once, 30 seconds after the node starts watching its coin.
  Detail: PR #116; CRD ch.60 R60.4.5, R60.4.6.

#### UTXO coins and Electrum

- **Electrum servers are kept in priority order and recover.** Servers are tried in an order drawn
  at random once when the coin is enabled, so users spread across the listed servers (set
  `electrum_server_order` to `listed` to keep the listed order); a recovered higher-ranked server is
  used again; a server that keeps failing is skipped for a growing time (20 s doubling up to 12 h);
  if every connected server fails, every listed server is tried; connecting is bounded by
  `timeout_sec` (default 20 s) and each request by 20 s; errors name each server tried as
  host:port. Detail: PR #104; CRD ch.38 §38.6.4.
- **Electrum servers can be ranked by how they perform.** Two new values of the `MM2.json` string
  `electrum_server_order` rank a coin's servers by measurement instead of by one random draw. `mixed`
  weighs measured speed and chance half and half; `measured` goes by measured speed alone and is meant
  as an opt-in, because nodes in one region would all pick the same fastest server. When the coin is
  enabled, each listed server is asked for its newest block once (a small request that carries no
  address), and servers that are slow or behind the chain rank last. While running, a connected
  server that falls behind the chain or turns slow is replaced by a better one found by asking the
  unused servers again, at most once per 5 minutes; nothing is probed on a schedule. The default stays
  `random`. Set `electrum_server_order` to `listed` for GLEEC KDF behavior. More in
  [`electrum-measured-server-order`](docs/plans/electrum-measured-server-order.md). Detail: PR #144;
  CRD ch.38 R38.6.4l.
- **Read-only Electrum queries are spread over the connected servers.** With the `MM2.json` boolean
  `electrum_spread_reads` at its default `true`, each balance, unspent-list, history, header,
  block-count, transaction and fee query of a coin goes to the next connected server in turn instead
  of always to the first, so an order-posting burst can use the request budget of every connected
  server. Broadcasts, subscriptions and pings are routed as before, and a query whose server fails
  moves on to the others. Set `electrum_spread_reads` to `false` to restore GLEEC KDF behavior, where
  every query goes to the highest-ranked connected server first. Spreading sends one read in N to
  each of N connected servers, a slow or distant one included. On a list with one such server, a loop
  of balance reads and order posts can be slower than before: in a lab loop of 200 RIN orders, one
  server about nine times slower than the other two made the loop 48 % longer. To avoid it, remove
  distant servers from the coin's list, or set `electrum_spread_reads` to `false` and
  `electrum_server_order` to `listed`, which sends every read to the first listed server. Leave it on
  where the servers limit each session, as DOGE's do: there every order post draws on the budget of
  every connected server. More in [`order-posting-latency`](docs/plans/order-posting-latency.md).
  Detail: PR #128; CRD ch.38 R38.6.4a.
- **Fixed-fee UTXO coins now pay their `txfee` per kilobyte proportionally, as the reference does.**
  Every coin with a nonzero `txfee`, or none, used to pay the whole per-kilobyte fee for every
  started kilobyte, so a 226-byte KMD transaction cost 1,000 base units instead of 226. Withdrawals,
  swap payments and the DEX fee transaction now pay `floor(txfee × size / 1000)`, and a coin that
  forces the node's minimum relay fee pays that rate proportionally as well. A segwit transaction's
  size is its virtual size rounded up, as nodes count it, so a fee at the relay minimum is never one
  unit short. Swap spends and refunds are estimated over 496 bytes instead of 305 bytes rounded up
  to a whole kilobyte: a KMD spend costs 496. An explicit `UtxoFixed` withdraw fee is still charged
  for every started kilobyte, which the reference does not do (see Known limitations). Detail:
  PR #72; CRD ch.38 R38.6.6b–c.
- **The coins-file option `dingo_fee` is now honored.** With `dingo_fee: true` and a positive
  `txfee`, a coin pays `txfee` for every started kilobyte. DINGO, DEV, NYC, SHIC, TRMP and XVG carry
  it. Their version-two swap spend fee is now one `txfee`, matching a reference peer that validates
  it exactly; before, such a swap failed that peer's check. Their minimum trading volume uses the
  same fee. Detail: PR #72; CRD ch.38 R38.6.6a, ch.60 R60.6.2.
- **A UTXO coin's header layout now reaches every header read, and `protocol.chain_variant` is
  honored for the first time.** The key is read from the coin's entry (eight values; an unrecognized
  one fails the coin's activation naming the ticker and value), and the coin's layout
  (`header_layout`, else `chain_variant`, else the Qtum reader for a `QTUM`/`QRC20` protocol type) is
  used by header sync, the reorg walk, block timestamps, SPV, the stored-header re-reads and the
  WalletConnect genesis lookup, not only the median-time-past. The structural fallback for `RIC` and
  `MEWC` now applies on all of those paths too. The families without a dedicated reader still read
  the standard layout. Detail: PR #74; CRD ch.37 R37.5.1, R37.5.1a, R37.5.9e.

#### EVM coins

- **An unresponsive EVM node is given up after 10 seconds instead of 60.** The limit now covers the
  whole attempt, from connecting to the last byte of the answer, on every platform, so a slow node no
  longer holds a request for a minute before the next node is tried. A request given up on is now
  closed too, instead of staying open in the background for as long as the node keeps silent.
  Detail: PR #90; CRD ch.35 §35.1.F.
- **The initial swap gas-fee policy no longer depends on how the coin was activated, and a coin's
  `max_eth_tx_type` now caps the transaction type it will sign.** The legacy `enable` / `electrum`
  route resolves the policy the way the other routes do — the `swap_gas_fee_policy` request field,
  else the `swap_gas_fee_policy` key of the coin's entry, else `Legacy` — and that request field is
  new on the legacy route and parsed leniently there: an unrecognized value is ignored, whereas
  `enable_eth_with_tokens` and `task::enable_eth` still reject it with `InvalidRequest`. A swap send
  whose EIP-2718 type exceeds the coin's `max_eth_tx_type` (a token falls back to its platform
  coin's entry) is now refused at signing, with nothing broadcast and no lower type substituted.
  **A coin with no `max_eth_tx_type` key counts as `0`**, so a typed swap send for it is refused
  too; an operator who wants an absent key to mean unrestricted sets the node-level boolean
  `max_eth_tx_type_absent_unrestricted` to `true` (default `false`). Activation logs the effective
  policy and cap and where each came from. Detail: PR #30; CRD ch.35 D35.6 and D35.7.

#### ARRR and other shielded coins

- **`requires_notarization` no longer breaks ARRR swaps.** A shielded coin such as ARRR has no
  notarized confirmation count, so the setting has nothing to select. It is still accepted, from
  the coins file, the activation request, `set_requires_notarization` or an order's
  `base_nota`/`rel_nota`, and now has no effect: a warning names the coin, the coin reports
  `false`, and a swap counts ordinary confirmations. Before, such a swap failed once a payment was on
  chain, and the funds came back only by refund after the swap's lock times had passed.
  Older coins files that set the flag for ARRR need no edit. A peer whose coins file still sets
  it makes the swap lock time four times as long on both sides, as before; the published coins
  file sets `false` for ARRR. GLEEC KDF honors the flag through a notarized confirmation count;
  no switch restores that, because no such count exists here. Detail: PR #169; CRD ch.39
  R39.11.4e, D39.11.f, D39.11.i.
- **ARRR native mode is refused in this release; use light mode.** Activating ARRR (or another
  shielded coin) with `mode.rpc` `Native` now fails at once with an error saying that native mode is
  not supported in this release and that light mode should be used. Native mode could not build a
  working wallet for a fresh activation, and after Ironwood it could build transactions the network
  rejects. Light mode is unchanged. Native mode returns in a later release, see
  [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md). GLEEC KDF accepts
  native mode. Detail: PR #115; CRD ch.39 R39.11.1a.
- **A taker paying with ARRR is refused at the start of the swap when nothing could watch its
  payment.** A taker learns the swap secret only by seeing the maker spend the taker's payment.
  Before sending its dex fee, a taker whose payment coin is ARRR in light mode now checks that at
  least one source can answer an address question about it: a `lightwalletd` server, or an Electrum
  server that is on the same chain. If none can, the swap ends at once with `StartFailed`. The error
  names each server and what is wrong with it, and nothing is sent. Before, such a swap could reach
  the payment and lose it. Detail: PR #136; CRD ch.39 R39.11.28.
- **A shielded (ARRR) send that has to wait for an earlier unmined send now waits up to 10 minutes
  instead of 5, retrying every 15 seconds, as GLEEC KDF does.** The wait still ends at once when no
  waiting could free enough funds. More in
  [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md). Detail: PR #85.
- **Quieter logs while a shielded coin such as ARRR is enabled.** Keeping the shielded wallet up to
  date logged six `info` lines on every new block. A sync that only adds the next block now logs
  them at `debug`. They stay at `info` for the first sync after activation, for a sync that covers
  more than one block or resets the wallet's scan state, and when another lightwalletd server than
  the last one serves the sync. Failures are still logged as warnings. Detail: PR #168.

#### Node names in logs and errors

- **Node and server addresses and credentials no longer appear in logs, error texts or debug output.**
  Every log line, error text and status message now names a node or server by scheme, host and port
  only (Electrum servers as `host:port`), for Tron, Tendermint, Electrum, the native coin daemon,
  lightwalletd, Solana and the Siacoin wallet daemon, and nothing prints a password, API key,
  Authorization or header value, or the userinfo, path or query of a URL. A Tendermint node's response
  body is no longer logged. The debug-build RPC wire dump is not affected. A shielded coin's
  broadcast error and a warning log line now name the lightwalletd endpoint that rejected the
  transaction, and a lightwalletd endpoint that fails to connect or answer is tried after the others
  for a growing time (20 seconds, doubling, up to 12 hours) until it answers again. Detail:
  PR #161; CRD ch.35 R35.1.17, ch.39 §39.11.6.
- **EVM and Tron log lines and errors name a node by scheme, host and port only, and EVM lines say
  which coin and which list position.** Paths, query strings and credentials, where hosted providers
  keep API keys, are never printed: the activation probe, the per-node failure logs, the error that
  lists the nodes tried, and the lower-layer texts they carry. The same goes for the gas-station URL
  and the gas-API provider (Infura, Blocknative) URL, which also carry API keys. When a node answers
  with an error, the log says so (`EVM RPC <host> answered <method> with error <code>: <message>`)
  and the error returned to the caller ends with `(node <host>)`, so a revert reported by one of
  several nodes can be attributed. A line such as
  `[ETC] Couldn't get client version for node https://rpc.example.org (node 2 of 4), ...` starts
  with the ticker and gives the node's position in the list as you configured it, so two coins that
  use one provider with different paths, or two nodes of one coin on one host, can be told apart.
  There is no switch to print the full URL. Detail: PR #90, PR #133; CRD ch.35 §35.1.F, R35.1.16.

#### Transport, TLS and dependencies

- **The wss transport now uses rustls 0.23, and a malformed wss key file is a startup error instead
  of a panic.** A relay whose configured wss key or certificate cannot be loaded now fails to start
  with `P2PInitError::InvalidWssConfig`. Detail: PR #39.
- **The Electrum TLS client moved to rustls 0.23, with a different trust-anchor set.**
  `webpki-roots` goes from 0.22 to 1.0, which drops 45 root certificates — among them roots Mozilla
  marks distrust-after (DigiCert Global Root CA, GlobalSign Root CA, Baltimore and others) and the
  removed TrustCor — and adds 32. A server whose chain terminates only at a dropped root is now
  refused where it was accepted; none of the 225 reachable servers in the public Electrum lists is
  affected, but a privately configured server can be. TLS 1.2 is still offered, and
  `skip_validation` keeps its previous meaning. Detail: PR #40.
- **The native HTTP client and the RPC listener moved to hyper 1; rustls 0.20 and hyper 0.14 have
  left the build.** The RPC port still serves both HTTP/1 and HTTP/2 prior-knowledge, the SSE
  endpoint and its event format are unchanged, and TLS 1.2 is still offered. Error strings from a
  failed HTTP fetch now carry the whole source chain (for example `client error (Connect): invalid
  peer certificate: UnknownIssuer`), so anything that parses or logs them sees longer text. The
  lightwalletd gRPC transport moved to the same stack earlier in the series, which changes its
  default user-agent and the wording of its transport errors. Detail: PR #42 and PR #37.
- **The WalletConnect relay transport moved to `tokio-tungstenite` 0.24 on rustls 0.23.** No
  protocol or configuration change; not yet exercised against a live relay session. Detail: PR #40.
- **The `sia-rust` pin now requires `web-sys >= 0.3.70`**, which reqwest 0.12's wasm backend needs;
  our own lockfile resolves well above that, so only an integrator who vendors or re-resolves for
  `wasm32` has a floor to raise. Detail: PR #38.
- **Lockfile bumps clear three RustSec advisories:** `tokio` 1.52.0 to 1.54.1 (RUSTSEC-2026-0386,
  `AsyncFd::new` I/O safety), `rustls-webpki` 0.103.15 to 0.103.16 (RUSTSEC-2026-0382, distrusted
  intermediates) and `webpki-roots` 1.0.8 to 1.0.11 (RUSTSEC-2026-0381, emSign intermediates). The
  bundled trust store drops four roots (Atos TrustedRoot 2011, emSign Root CA - C1, emSign ECC Root
  CA - C3 and G3) and adds four (SECOM TLS RSA and ECC Root CA 2024, Telia EC and RSA TLS Root CA
  v3); connections to servers chained only to a dropped root stop verifying. A fourth advisory,
  RUSTSEC-2026-0387 (`crypto-bigint` 0.5.5 timing variability, reached through `k256`/`ecdsa` 0.16),
  cannot be fixed by a lockfile update; it is accepted in `deny.toml` until the RustCrypto EC stack
  moves to `crypto-bigint` 0.7. Detail: PR #68.

### Fixed

#### Order book and peer-to-peer network

- **A market maker's keep-alive is now sent once per trading pair, as other nodes expect.** A maker
  with orders on several pairs used to send, every 30 seconds, one message per pair that each listed
  every pair, so each node that relays the order book asked for every pair once per copy. When the
  node answering could not serve one of the pairs, the whole request failed and the maker's orders
  stopped spreading further. Each message now carries the one pair of the topic it is sent on, and
  pairs without orders get no message. Detail: PR #111; CRD ch.32 R-K2a.
- **A node now asks for exactly the order-book changes it is missing.** When a maker's keep-alive
  shows that a node's copy of the maker's orders differs, the node asks another node for an update.
  It used to describe its copy by the maker's advertised state instead of its own, so while the maker
  kept changing orders the answer could be meant for a different copy, and the node kept only part of
  the maker's orders, asking again every 30 seconds. The node now names the state it actually holds
  and gets the changes it can apply, or the full set. Set the `MM2.json` boolean
  `orderbook_sync_names_held_root` to `false` to ask the way GLEEC KDF does. Detail: PR #111; CRD
  ch.32 R-Q4a.
- **A node answers every order-book sync request at once and asks for one only when it needs it.** A
  node whose own order data was a few milliseconds behind used to refuse other nodes' sync requests
  with a "retry" answer; a relay that got that answer stopped passing the maker's orders on until the
  next keep-alive, 30 seconds later. The node now answers from the data it has, as other KDF nodes do,
  leaving out an order whose cancellation it is still applying. Before comparing a received
  keep-alive with its own data, it waits briefly for that data to catch up, so it no longer requests
  a sync it does not need. Detail: PR #111; CRD ch.32 R-T3b items 2 and 4.
- **A node that relays the order book now answers other nodes' sync requests for a maker it is still
  catching up on, as soon as it has caught up.** Before, a node asking a relay for a maker's orders
  while the relay was itself fetching them (the usual case right after a stall or a restart) got an
  error at once and waited for the maker's next keep-alive, 30 seconds later, for each relay hop. The
  relay now holds such a request for at most 5 seconds and answers it when its own sync lands, so
  the nodes behind it rebuild the maker's orders in seconds. If the relay's own sync fails or takes
  longer than that, the asking node gets the same error as before. At most 8 requests per peer and
  256 in all are held. Nothing on the wire changes, and nodes running other versions benefit too. To
  restore the old behavior, set `sync_answer_deferral_ms` to `0` in `MM2.json`. Detail: PR #135; CRD
  ch.32 R-Q5b.
- **A relay whose own order-book sync fails at once now asks once more after a second, so the
  nodes behind it no longer get an error for the next 30 seconds.** A relay that fetches a maker's
  new pair from another relay often gets an error, because that relay is still fetching the pair
  itself. Until the maker's next keep-alive, 30 seconds later, it then answered every node that
  asked it for that pair with the same error, and each relay hop added another 30 seconds. On the
  public network this showed up on GLEEC KDF nodes for about two minutes after a Reloaded maker
  added pairs. The relay now waits 1 second and asks the same peer once more, only for the pairs it
  still has nothing for, and the requests it is holding for other nodes wait for that answer. It
  asks again only when the answer can still arrive within the 5-second hold, never after a timed-out
  request, and never a third time. Nothing on the wire changes. To restore the old behavior, set
  `sync_request_retry_ms` to `0` in `MM2.json`. Detail: PR #173; CRD ch.32 R-Q5c.
- **A relay now sends an ordinary order-book message once instead of twice.** A relay forwards each
  message as soon as it arrives, and used to send it again after handling it. The receiving nodes,
  GLEEC KDF and Reloaded alike, drop that second copy as a duplicate, so for new orders, order
  updates and keep-alives it only doubled what a relay sends toward each peer. On the public network
  a seed's far relays dropped up to a fifth of that stream in busy hours. Cancellations and swap
  negotiation messages still get their second copy, through the send queue's priority lane, and so
  do swap and watcher messages. The relay still marks every message as handled, so it announces and
  serves it to peers that ask exactly as before. Nothing on the wire changes. To restore the old
  behavior, set `relay_second_copy_priority_only` to `false` in `MM2.json`. Detail: PR #178; CRD
  ch.32 R-F3b, D11.
- **An idle second connection between two relays no longer logs a warning when it closes.** Two
  relays that dial each other hold two connections, and a connection that carries nothing for two
  minutes is closed as idle while the peer stays connected through the other. Since a relay sends
  ordinary order-book messages once, this happens every few minutes when a peer keeps redialing.
  That close now logs at INFO as an idle connection closing; the warning stays for a peer's last
  connection and for every other cause. Detail: PR #183; CRD ch.32 R-F3b, D11.
- **A node no longer drops a market maker's whole order book when only the maker's keep-alives are
  delayed.** A maker's order book is kept for as long as either its keep-alives or its order
  messages (an order created, updated or cancelled) keep arriving; before, only keep-alives counted,
  so a maker whose keep-alives were held up in a busy send queue while its updates still arrived lost
  its book after 90 seconds. A maker that really stopped is still removed after the same time. A
  message from a maker the node does not know yet adds nothing, and nothing on the wire changes. To
  restore the old behavior, set `order_messages_refresh_liveness` to `false` in `MM2.json`. Detail:
  PR #132; CRD ch.59 R59.6.1a.
- **A maker that goes offline now drops out of the order book on schedule, whatever its clock says.**
  The keep-alive expiry used the timestamp the maker put in its own message, so a maker with a fast
  clock stayed listed for that skew plus 90 s after it died, and one that wrote a far-future
  timestamp stayed listed indefinitely. The expiry now runs from when this node received the
  keep-alive; the wire format is unchanged. Keep-alives also no longer advertise empty trie roots.
  Detail: PR #52; CRD ch.32 R-P3.
- **A maker no longer advertises or serves a just-cancelled or just-repriced order from stale data.**
  For a few milliseconds after a cancel or a price change, the node's order index was already up to
  date while the data behind its keep-alive messages and its answers to other nodes' sync requests
  was not, so it could still announce the old state, and a node that had already seen the cancel
  could log "recently cancelled" when the old order was listed again. Keep-alives are now built only
  once both are up to date; until then the keep-alive is skipped for that round. Detail: PR #98; CRD
  ch.32 R-T3b.
- **A cancelled order's identifier is no longer ignored once its two-minute window is over.** A node
  remembers for 120 seconds that a peer cancelled an order and ignores a late announcement of the
  same order during that time. The check did not look at the age of the memory, so an old entry could
  keep ignoring the same order after the window, until the next cancellation cleared it. The check
  now treats an entry older than the window as absent. Orders get random identifiers, so this was
  very unlikely to matter in practice. Detail: PR #102; CRD ch.11 R10.
- **A maker node that is not a relay now keeps its order book of other makers up to date after a
  restart.** After a restart the node announced its stored orders again, but it did not record that
  it was following their trading pairs, so a keep-alive from another maker with a different order
  list never made it ask for that maker's orders, and its view of the other makers on its own pairs
  went stale until something else (an order book request, a new order on the pair) recorded the
  pair. Re-announcing a stored order now records the pair first, without requesting the order book.
  Detail: PR #101; CRD ch.59 R59.6.7.
- **A maker order for a shielded-coin pair (such as ARRR) no longer drops out of the maker's own order
  book every minute and a half and gets announced again.** Orders for those pairs are placed under a
  separate key of their own, and the cleanup that forgets silent peers treated that key as a silent
  peer, because the node never hears its own keep-alive back. The cleanup now leaves the node's own
  keys alone, so these orders stay put and the extra announcements stop. Detail: PR #94; CRD ch.59
  R59.6.5.
- **Reading the order book of a pair that includes an ARRR order no longer fails on the older
  `orderbook` method.** Before, one ARRR order on the book made the whole older `orderbook` request
  fail with an internal error, and the older `best_orders` silently left the order out. The order now
  appears with the address `Shielded` (the same word the newer interface uses), and an order whose
  coin has no address to show is left out on its own instead of failing the response. Tendermint,
  Siacoin and Lightning orders now show up on the older methods too. Detail: PR #88; CRD ch.32
  R-F6d, R-F6e.
- **Gossip message ids now use the compact 8-byte form other KDF nodes use, which should cut the
  bandwidth a relay spends on duplicate messages.** Our nodes announced each message with an id
  written as decimal text, while the other implementations write the same 64-bit value as 8 raw
  bytes, so an id announced by one side never matched the id the other side already held and every
  announced message was requested again, then dropped as a duplicate. The id is now the 8-byte
  big-endian form; what it is computed from is unchanged. Nodes on the old and new encoding still
  exchange messages normally, but each message is sent once more between them until both sides are
  upgraded. Detail: PR #99; CRD ch.28 R13.
- **A peer that cannot keep up no longer makes a node's memory grow without limit.** The queue of
  gossip messages waiting to be sent to one peer is now capped at 2,048 messages or 4 MiB, whichever
  comes first. When a message does not fit, the oldest waiting messages toward that peer are dropped,
  so a peer that catches up keeps the newest; other peers are not affected, and the keep-alive
  exchange repairs what a dropped message carried. A peer is disconnected, on every connection it
  holds to the node, when it stays at the limit for a full minute or loses at least a quarter of the
  messages sent to it in each of three minutes in a row. It resynchronizes when it reconnects. Seeds
  usually hold two connections to each other, and closing only one used to move the backlog onto the
  other. A node closes at most one of its relays this way within 30 seconds, and never one that
  would leave it with fewer than two; meanwhile that relay's queue keeps dropping its oldest
  messages. A peer that keeps up never reaches the limit.
  Messages are also sent in the order they were queued, where a backlog used to go out newest first.
  The first drop toward a peer logs one warning with its peer ID, then at most one summary line per
  minute. Cancellations and swap negotiation messages go out ahead of the others, through a lane of
  256 messages per connection; when more of them are waiting, the rest join the end of the normal
  queue. They used to disconnect the peer, so a maker cancelling hundreds of orders at once cut a
  seed off from all its slower relays in the same second. Nothing new is sent on the wire. Detail:
  PR #122, PR #172, PR #175; CRD ch.28 §28.9B, D8.
- **A seed or relay now gives back the memory a burst of order updates used.** Every order update a
  node applied left about 1.2 KB behind (the stored change history, replaced trie nodes, and
  bookkeeping), and none of it was released until the same order pair changed again, so a node that
  had seen 500 updates a second for a few minutes stayed about 150 MB larger afterward. The change
  history is now expired and its tables shrunk every 30 seconds, whatever the traffic; the trie node
  store also shrinks after each ten-minute clean-up; refreshing a cache entry no longer adds to its
  queue; and clearing a pair or a pubkey now frees the nodes it leaked (about 6 KB per full resync of
  a small pair). On Linux, the system's memory allocator (glibc) kept the freed pages in the process
  even then, so after each ten-minute clean-up the node now also asks it to hand them back: in a test
  where the order data itself was 1 MB, the memory the operating system showed fell from 160–210 MB
  to 30 MB. The memory use before and after goes to the debug log; other systems are unchanged. A
  peer that has been out of sync for more than five minutes now receives a full snapshot instead of
  a delta (as it already did after a write to the pair); within five minutes nothing changes. The
  metrics now also show the number of stored change histories and changes, tracked pubkeys and
  queued trie updates, and, after each ten-minute clean-up, the node store's size and entry count.
  Detail: PR #131, PR #134, PR #137; CRD ch.32 R-K4.
- **The order-book store no longer grows without bound when a maker updates orders very fast.** The
  node now applies queued order updates in bulk (up to 1,024 batches or 2,048 events at a time),
  drops the unused entries of its node store as soon as 100,000 updates have piled up (at most every
  five seconds) instead of only every ten minutes, and keeps at most 2,048 stored changes per maker
  and pair. In a test of 120,480 updates for 30 orders the memory left after the node caught up fell
  from 134 MB to 26 MB (3 MB when the node was held back first). A peer that is behind, or asks
  about a state older than the kept changes, receives the full set of orders instead of a delta;
  nothing else changes for peers. The metrics gain `orderbook.trie_store.early_purges` and
  `orderbook.history.evicted`. More in
  [`orderbook-queue-and-history-caps`](docs/plans/orderbook-queue-and-history-caps.md). Detail:
  PR #164; CRD ch.32 R-K5, R-K6.
- **On Linux the daemon now limits the memory allocator to two arenas at start-up.** The system's
  memory allocator (glibc) gives each thread that allocates an area of its own, and the daemon runs
  from about 30 threads on an 8-core host to several hundred on a many-core one. Memory freed in
  one area is mostly not reused by the others, so a maker that updated hundreds of orders every few
  seconds held steadily more memory although the data it holds stayed the same. In tests a maker
  with 480 orders on 16 pairs, updating about 80 times a second, grew by about 1 MB a minute with
  the allocator's own default, and by about 0.2–0.3 MB a minute with two arenas over 40 minutes;
  whether that levels off is not measured yet, and there was no change in update rate or processor
  use. The limit applies only to the daemon on Linux with glibc, not to the embedded
  library or other systems, and the start-up log has one line that says where the limit came from.
  Set the environment variable `MALLOC_ARENA_MAX` to choose your own (the daemon then leaves the
  allocator alone), or the `MM2.json` whole number `malloc_arena_max`; `0` keeps the allocator's own
  default, and an invalid value is reported and replaced by `2`. Its effect on swaps and coin
  activation is not measured yet. Detail: PR #189, PR #190; issue #191;
  [`malloc-arena-cap`](docs/plans/malloc-arena-cap.md); CRD ch.45 R45.6A.
- **Peer exchange keeps the usable addresses of a peer list.** Nodes ask each other for peer
  addresses every five minutes. Some peers that do not run Reloaded include the hostname under which
  they reached a seed, such as `/dns/seed04.kmdefi.net/tcp/42845`. Reloaded used to throw away the
  whole list for one such entry and stop asking the peer that sent it. It now skips only the entries
  it does not accept and keeps the peer. It still accepts only a public IPv4 address with the
  network's port; a hostname is now looked up first (see "Peer exchange now also uses seeds that
  other peers hand out by name" under Changed). A skipped address is logged
  once at info level, then at debug level. A node also no longer keeps or dials its own entry, which
  other peers hand back to it, and it drops a stored address when a dial of it reaches a different
  peer. Detail: PR #170, PR #176; CRD ch.28 R8c.

#### Orders, swaps and activation (all coins)

- **A taker that falls back to a refund now takes the maker's coins when the maker has already
  claimed the taker's payment.** When a taker could not confirm its own payment, for example during
  a node or server outage, or stopped waiting at the lock time just before the maker claimed it, the
  swap went on to refund the payment. If the maker had claimed it, the refund was rejected and the
  swap ended as failed: the taker got the maker's coins only by running `recover_funds_of_swap`
  before the maker's own lock time ran out, and lost both sides otherwise. Now the taker checks the
  chain before the refund, every few minutes while it waits to refund, and after a rejected refund;
  when the maker has claimed the payment, the taker reads the secret and claims the maker's payment,
  as in a normal swap. Such a swap still lists the earlier error events, so a wallet may show it as
  failed although the trade completed. A refund already on the chain, after a restart, now ends the
  swap as refunded. Detail: PR #167; CRD ch.51 R31a.
- **A node now re-sends other traders' swap transactions as a backup, instead of failing every
  time.** Traders publish their claim and refund transactions to the network so that other nodes can
  broadcast them too. A Reloaded node passed the whole signed message to its server instead of the
  transaction inside it, so every such broadcast was refused ("TX decode failed") and logged as an
  error; on a seed with coins enabled that was dozens of error lines a day. The node now takes the
  transaction out of the signed message and broadcasts it. A refusal is the normal outcome, because
  the traders have usually broadcast the transaction already, and it is logged only at debug level;
  a message that is not a valid signed transaction is logged as a warning. Detail: PR #177; CRD
  ch.51 R50B.
- **A node no longer starts swap watchers it cannot run for Bitcoin-family coins.** A node offered to
  watch other traders' swaps for every enabled coin of the Bitcoin family (UTXO, BCH, QTUM), but the
  checks a watcher needs are not implemented for these coins yet. Every watcher it started gave up
  after a minute, without doing anything, and logged seven warnings; it started again for the next
  copy of the same swap. These coins no longer offer watching, so no watcher starts and nothing is
  logged. Nothing else changes: these watchers never sent a transaction. Siacoin keeps its
  watcher. Detail: PR #177; CRD ch.09 R17.
- **Listing, cancelling or updating an order no longer waits for another path's disk or network
  work.** The balance-update handler, the periodic balance check, the removal of timed-out matches,
  an order's timeout expiry, the cancel paths and `update_maker_order` held the order's lock while
  they wrote the order's file and history row, and the background maintenance that re-announces a
  node's own orders missing from the order book held it while it read the balance and the fee, so
  `my_orders`, `order_status` and other calls on that order waited for the disk or a network round
  trip. They now do that work without the lock and take it only to apply the result, re-checking the
  order first; the maintenance also reads each base coin's balance once per pass. One order's writes
  still land in the order of its changes, and a cancelled order's record is never written back.
  Detail: PR #128, PR #149; CRD ch.12 R7a, R7b, ch.60 R60.4.6 item 5.
- **Two activations of the same coin no longer run at the same time.** If a coin is activated again
  while its first activation is still running, through any activation method or the legacy `enable`
  and `electrum`, the second now waits for the first. If the first activated the coin, the second
  ends with the usual already-activated error and builds nothing; if the first failed, the second
  goes ahead. Before, the two built the coin in parallel. A second ARRR activation already waited,
  but then built the coin a second time over the wallet database the first coin was using, and only
  then failed. A platform activation that brings tokens also waits for activations of those tokens.
  Callers get the same answers as before; only the second call can return later. Detail: PR #156;
  CRD ch.55 R55.1.12.
- **A maker whose swap negotiation fails is now banned by the taker for an hour, as specified.**
  Before, a taker banned a maker only when the maker's payment failed validation or the maker never
  spent the taker's payment. A maker that failed during negotiation could be picked again for the
  taker's next order. Now that maker's offers are ignored for an hour, so a second attempt goes to a
  different maker, as other KDF nodes already do. The ban appears in `list_banned_pubkeys` and can be
  lifted with `unban_pubkeys`. Detail: PR #158; CRD ch.59 R59.5.2, R59.5.4.
- **A taker now keeps trying to send its swap payment until the same moment the reference does.**
  The taker gave up on sending its payment one third of the swap's lock duration after the start. It
  now uses the maker-payment wait deadline, two fifths of the lock duration, which is the same
  deadline it already uses to wait for the maker payment's confirmations. A taker whose maker payment
  confirmed late used to end the swap at this step, losing its dex fee and leaving the maker's payment
  locked until its refund. The maker's own deadlines do not move. Detail: PR #126; CRD ch.51 R29.
- **A swap that resumes after a restart no longer gives up on a payment it already sent.** Suppose a
  node restarted after broadcasting its swap payment but before recording it, and came back after
  that payment's send deadline. The swap checked the deadline first and ended as "payment failed", so
  the payment stayed on chain with no refund until someone ran `recover_funds_of_swap` by hand. The
  swap now first looks for a payment it may already have sent and carries on with it. The deadline
  stops only a payment that has not been sent yet. This applies to maker and taker, for every coin.
  Detail: PR #119; CRD ch.51 R17, R29, T21.
- **A swap spend or refund whose fee would exceed the payment is now refused instead of broadcast
  as garbage.** The UTXO spend and refund builders subtracted the fee unchecked, so the value
  wrapped and the coin daemon rejected the transaction. They now fail with an error and broadcast
  nothing. The same holds for the ZHTLC transparent spend and for Sia, where the fixed miner fee is
  0.00002 SC. The version-two funding spend now also requires the fee plus dust. Detail: PR #73;
  CRD ch.60 R60.6.4.
- **An 8-decimal EVM token such as WBTC no longer has a 0.01 minimum trade.** An EVM coin's minimum
  trading volume was 10^-(decimals / 3) with integer division, so every 8-decimal token refused a
  buy, a sell, a maker order or a trade preview below 0.01 (`The volume 0.00072437 of the
  WBTC-PLG20 coin less than minimum transaction amount 0.01`), and a maker that set no minimum
  advertised 0.01 to its peers. It is now one smallest unit of the coin, 10^-decimals, matching the
  reference. QRC20 tokens get the same fix, and TRX, which shares the EVM path, moves from 10^-2 to
  10^-6. `min_trading_vol` reports the new value. The minimum transaction amount, which the DEX fee
  and maximum-volume paths use, stays zero. Detail: PR #66; CRD ch.57 R57.7A.1, R57.7A.6.
- **BTC's minimum trading volume is now 0.0001, as for BTC-segwit and in the reference.** The exact
  ticker `BTC` was fixed at 0.00777; it now follows the UTXO rule, the larger of ten times the dust
  and ten times the fee for a 496-byte spend. That fee term is now truncated rather than rounded up,
  which changes no coin in the public coins file. Detail: PR #66; CRD ch.60 R60.6.1, ch.38
  R38.6.6e–f.
- **`min_trading_vol`, `setprice`, `buy`, `sell` and `trade_preimage` no longer crash the request for
  Lightning, Solana or SPL coins.** Lightning now reports its minimum trading volume and minimum
  transaction amount as the reference does: the largest counterparty minimum HTLC over its open
  channels, or 1 msat. Orders and trade previews involving Lightning, Solana or SPL, which cannot
  trade yet, are refused with an error naming the coin; `trade_preimage` returns the new typed error
  `TradingNotSupported` (HTTP 400). Detail: PR #66; CRD ch.60 R60.6.5, ch.41 R41.11A.
- **A node no longer keeps memory for every swap it has ever run, and stopping it no longer waits
  for the swap bookkeeping.** Each finished swap left a small record behind for as long as the node
  ran (about 3 KB for a maker or taker swap in total), so a node that runs many swaps grew slowly
  without bound. The record of a running swap is now dropped as soon as the swap ends, on every way
  it can end, together with the messages kept for it. When the node is told to stop, it clears this
  list at once instead of waiting for it to empty. Detail: PR #100; CRD ch.51 R58A, R58B.
- **Swaps with a shielded coin such as ARRR now report their outcome to the network like every other
  swap, and the report no longer names the traders.** When an older-style swap ends, whatever the
  outcome, the node publishes the usual `swapstatus` summary that statistics nodes collect; before,
  it stayed silent whenever a shielded coin was involved. The summary leaves out both parties' public
  keys (the sender's own key too, when the swap used a one-off key), and a statistics node now fills
  in only the key of the party that sent the report. Detail: PR #108; CRD ch.51 R50A.

#### ARRR and other shielded coins

- **An ARRR swap's HTLC key is now derived from the wallet, as other KDF nodes derive it.** Before,
  each swap got a random key that was saved in the swap file, so a swap started by another KDF
  implementation could not be recovered here, nor one of ours there. The key is now computed from the
  wallet's transparent key and the swap (the secret hash for the maker, the swap UUID for the taker),
  and it is never saved: it is computed again on restart and on recovery, so either implementation
  can recover a swap with the same seed. Under an HD wallet the wallet's transparent key for ARRR and
  other shielded coins is now the key at the coin's `derivation_path` (for ARRR
  `m/44'/141'/0'/0/0`), and activating such a coin under an HD wallet fails if its coin entry has no
  `derivation_path`; with a passphrase wallet nothing changes. Swaps already in progress keep their
  saved key. Detail: PR #152; CRD ch.39 R39.5.6–R39.5.11.
- **An ARRR swap refund no longer fails as "non-final" right after the lock time.** The refund of a
  shielded-coin payment now waits until the chain's median time (the clock the network judges lock
  times by) has passed the lock time, and a refund the network still rejects as not yet final is
  retried until the refund deadline instead of ending the swap with its refund failed. Calling
  `recover_funds_of_swap` too early now answers "too early" with the time it waits for. Detail:
  PR #146; CRD ch.39 R39.5.13.
- **Recovering an ARRR swap no longer fails with "does not pay the HTLC script".** Every ARRR swap
  uses its own key for the swap's payment, but `recover_funds_of_swap` checked the payment with the
  wallet's permanent key, so recovery always stopped with that error, even for a payment that had
  long been refunded. Recovery now tries the swap's own key first and the wallet's permanent key
  second, so swaps saved by older or other builds are recovered too. The log, and any error, say
  which key matched. A refunded payment is reported as refunded, naming the refund transaction; a
  payment that is still waiting is refunded; and the swap completes if the other side spent it. The
  error, when it is genuine, now says both keys were tried and shows the transaction id the way
  explorers do. Detail: PR #109; CRD ch.39 R39.5.4.
- **Recovering an ARRR swap saved by another build no longer fails with "Invalid address".** Some
  swap files carry an HTLC public key that does not belong to the private key saved with them.
  Recovery recognized the payment by that public key, then built the refund with the saved private
  key, which produced a different script, and the transaction builder rejected it. Recovery now
  always signs with the private key that belongs to the matching public key: the coin's own
  persistent key when that is the one. If it holds no such key, the error now says so, naming the
  key. Detail: PR #142; CRD ch.39 R39.5.4.
- **ARRR swaps work again when their `lightwalletd` servers cannot answer address questions.** An
  ARRR swap asks three questions about its HTLC: was my payment already sent, was this payment
  spent, and has the counterparty spent it yet. When no `lightwalletd` server can answer them, for
  example because its node keeps no address index, the swap now asks the Electrum servers given in
  the activation's `electrum_servers`, as other KDF nodes do. A server is used only if its block
  hash at a recent height matches the `lightwalletd` servers' and its tip is at most 10 blocks
  behind; nothing else of the coin uses Electrum. Activation never connects to these servers; the
  first question does. When neither source can answer, the error names both. More in
  [`arrr-htlc-electrum-fallback`](docs/plans/arrr-htlc-electrum-fallback.md). Detail: PR #130; CRD
  ch.39 R39.11.25.
- **An ARRR swap that restarts finds its own earlier payment without asking a server.** Before
  sending its HTLC payment, a swap asks whether that payment is already on chain. The answer now
  comes first from the wallet's own record of the transactions it broadcast. This works even when
  the configured `lightwalletd` servers cannot answer address questions. The record only ever answers
  "sent"; anything else still goes to the servers. Detail: PR #125; CRD ch.39 R39.11.26.
- **A fresh ARRR swap no longer asks whether it has already paid.** The "already sent" question
  exists for a swap that resumes after a restart. An ARRR payment goes to a script made with a key
  used for this one swap only, so a swap started in this run cannot have paid it yet. The question is
  now skipped there. A resumed swap, and every other coin, still ask it. Detail: PR #136; CRD ch.39
  R39.11.27.
- **ARRR swap recovery and spend detection no longer hang on a lightwalletd server whose node keeps
  no address index; such a server is skipped.** Two servers in the published ARRR server list answer
  every address lookup with "No information available for address". Each of those refusals was taken
  for one transaction the server could not deliver, and the search was narrowed down to single
  blocks to step around it. For a swap a few weeks old that meant tens of thousands of requests per
  server, with nothing logged, so `recover_funds_of_swap` seemed never to return. Any activation that
  listed such a server before a working one was affected. Now:
  - such a server is recognized after a few requests, logged once as a warning, and skipped for the
    rest of the activation;
  - each server gets at most 120 seconds per check;
  - the search starts at the payment's own block;
  - each step is logged.

  `recover_funds_of_swap` now returns within 10 minutes for every coin. If it stops, its error names
  the step it was in, and for ARRR also the server it was reading. Detail: PR #117; CRD ch.39
  R39.5.5, R39.11.4h, R39.11.4i.
- **An ARRR swap that fails on a lightwalletd server without an address index now says so.** When
  the server's node refused an address lookup ("No information available for address"), the error
  blamed a missing transaction index, which is a different setting; a maker payment failed with that
  explanation on a server whose node runs with the transaction index on. Such an answer now ends the
  lookup at its first request, and the error says the server's node keeps no address index and
  quotes the node's own message. Detail: PR #118; CRD ch.39 R39.11.4i(e).
- **An ARRR payment is no longer counted as confirmed one block early after the chain tip moves
  back.** Confirmations, and the decision that a transaction has expired, are now counted against
  the current tip, not against the highest one ever seen. A lagging server can now only make a wait
  longer, never shorter. Detail: PR #112; CRD ch.39 R39.11.10a.
- **The same lightwalletd server listed twice no longer counts as two votes.** When an ARRR node
  checks its `light_wallet_d_servers` against each other, a server listed twice, also under a
  different spelling such as with or without `https://` or the default port, now counts once.
  Before, `[A, A, B]` let A outvote B with only two real servers. Each dropped duplicate is logged.
  Detail: PR #113; CRD ch.39 R39.11.7b.
- **A ZCash-family (ARRR) light-server endpoint whose block cache is stale is no longer used.** An
  endpoint can report the current chain height and the right branch while its cache of blocks
  stopped much earlier, so it cannot serve anything newer; it is now excluded at admission, as is
  one whose cache answers with an error or whose height is more than 40 blocks behind the others.
  A failed admission now lists every excluded endpoint and why, and its message no longer claims
  that an endpoint is "not on this coin's chain". Detail: PR #51; CRD ch.39 R39.11.7a(j).
- **A shielded (ARRR) wallet that met a reorg of its own scan tip no longer wedges forever.** Every
  sync pass failed with `Compact block hash discontinuity` and the wallet never rewound. The scan
  now re-checks its anchor against lightwalletd on every pass, rewinds within the commitment-tree
  checkpoints the wallet retains, clears compact blocks cached above the rewound height, and, for a
  divergence deeper than it can rewind, rebuilds at the wallet's sync start rather than at height 0.
  A compact cache that fails validation is deleted instead of renamed to `.invalid-chain.<ms>.bak`,
  which also ends the multi-gigabyte backup flood. No RPC or wire change. Detail: PR #31; CRD ch.39
  R39.8.0al and R39.8.0n.
- **A shielded (ARRR) wallet no longer piles up `.checkpoint.<ms>.bak` files when its sync keeps
  failing right after a rebuild.** Until the first block is scanned, a wallet holds nothing but its
  account, and every sync pass starts that wallet over. Each restart kept the old copy as a backup,
  so a server that kept failing left one more file every 30 seconds. Such a copy contains nothing the
  wallet cannot recreate from its viewing key, so it is now deleted. A wallet that has scanned blocks
  or holds transactions is never deleted this way, and the one backup a rebuild keeps of the previous
  wallet is unchanged. Existing `*.checkpoint.*.bak` files can be deleted safely. No RPC or wire
  change. Detail: PR #150; CRD ch.39 R39.8.0ax.
- **A second ARRR withdrawal no longer fails with `bad-txns-duplicate-nullifier-requirements-not-met`
  because the first one's notes were still unspent.** `withdraw` and `task::withdraw` are unchanged
  (the client still broadcasts), but when the client submits the transaction through
  `send_raw_transaction` the notes it spends are now reserved until it is mined and scanned, or
  expires. A withdrawal that is built and never submitted reserves nothing. `withdraw` with `max`
  now reports an insufficient balance instead of an internal error when the spendable balance does
  not cover the fee. Two daemons on one seed with separate wallet databases are not protected.
  Detail: PR #49; CRD ch.39 D39.8.0e.
- **An ARRR send now appears in the transaction history at once, at the top, with its final
  amount.** It used to appear at the bottom of the list until it was mined, and showed the whole
  spent note as lost (for example -0.00198 instead of -0.00051), so the desktop wallet showed
  another transaction twice and hid the send. Now the send is listed at the top from broadcast on,
  with the change it pays back to the wallet already counted, and moves to its block's place once
  mined; a send that expires unmined disappears. Transactions of one block keep a fixed order.
  GLEEC KDF lists the send only once it is mined; set the `MM2.json` boolean
  `shielded_history_unmined_sends` to `false` for that behavior. Detail: PR #81; CRD ch.39 §39.8.5.1.
- **Right after an ARRR send, the change now shows as unspendable balance.** Until the send was
  mined, the balance showed neither the spent note nor its change, so the wallet looked poorer by
  the change amount for a block or two (for example 0 instead of 0.00106 unspendable). The change
  is now shown as unspendable from the moment the send is broadcast, also after a restart, and
  moves to the spendable balance once mined. A send that expires unmined drops out of the balance.
  The change of a send made by another instance on the same seed still shows only once mined. Run
  one instance per seed, as the known limitations already advise. Detail: PR #87; CRD ch.39
  R39.8.0ag1.

#### UTXO coins and Electrum

- **Enabling a UTXO coin no longer fails or stalls when the first listed Electrum servers are down
  and a later one is up.** Enabling a coin now tries every listed server at once, within the call's
  time budget, and succeeds as soon as one connects; if none does, the error names each server.
  Detail: PR #104; CRD ch.38 §38.6.4.
- **Electrum change notifications are no longer dropped.** A server's message that an address or the
  chain tip changed was read as an answer to no request and discarded, so balance streaming and
  history refresh never saw it; it now reaches the waiting watcher, also after the connection that
  carried the subscription was replaced. Detail: PR #104; CRD ch.38 §38.6.4, §38.6.5.
- **The legacy `electrum` method now honors `min_connected` and `max_connected`.** Before, both were
  ignored and every listed server was connected; with `max_connected: 1` only one server is connected
  now, and unusable values (zero, a minimum above the maximum, a wrong type, an empty server list) are
  refused with the usual error answer, as on the newer activation methods. This also applies to BCH
  and QRC20 activated through `electrum`. Detail: PR #91; CRD ch.55 R55.4.3, ch.38 R38.6.4.
- **A hostile or corrupt Electrum header response can no longer crash the node.** A header (or
  header list) whose length fields asked for gigabytes of memory could abort the process or kill the
  sync task, for any UTXO coin. The Equihash solution, Qtum's block signature and AuxPoW merkle
  branches now have length limits, and an over-long one is rejected as a malformed header like any
  other. Detail: PR #52; CRD ch.37 R37.5.6a.
- **A swap on a merged-mined (AuxPoW) coin could be neither completed nor refunded once the chain
  changed its block version.** The header reader recognized AuxPoW only by two exact version values,
  so Syscoin headers at version `0x20100180` (from about height 1,320,000) were read as plain
  80-byte headers and every header list was rejected. That broke median-time-past, which every HTLC
  spend and refund needs, so a swap that reached the payment stage was stuck. SYS, NMC, PEP, DINGO,
  MYDOGE, UNO, DOI, SHIC, TRMP and XMY were affected; DOGE worked only because its version equals
  one of the two values. AuxPoW is now recognized by the version's flag bit 8, per block, as the
  protocol defines it. Balances, history, deposits and plain withdrawals were never affected.
  Detail: PR #55; CRD ch.37 R37.5.2, R37.5.4.
- **RIC (Riecoin) and MEWC (Meowcoin) swaps can now complete the HTLC spend.** Their Electrum servers
  serve headers the standard 80-byte reader could not lay out (RIC: 112 bytes; MEWC: native blocks
  at 120 bytes interleaved with merged-mined ones at 80), so every header list was rejected, and with
  it the median-time-past the spend queries before it signs. A new per-coin key,
  `protocol.header_layout`, beside `type` in the coin's entry, selects the layout: `"riecoin"`
  (112 bytes: 64-bit time, 256-bit nonce) or `"kawpow_auxpow_mixed"` (per header: AuxPoW flag bit 8
  set reads the standard 80 bytes, clear reads the 120-byte KawPoW form). The median-time-past is then
  taken from each parsed header's time, and the full served bytes are what is kept. An unrecognized
  value fails the coin's activation. A coin with no key is read as before and, only when the
  standard read fails, retried as Riecoin and as the mixed layout (the one exact fit wins, one `info`
  line per coin naming the layout and suggesting the key); the new `MM2.json` switch
  `header_layout_fallback_families` (array of distinct `chain_variant` names, default `["BTC"]`;
  `[]` = reference behavior; anything else refuses the launch) names the families that may do so. A
  list that both alternatives consume exactly is rejected as ambiguous, naming both layouts, until
  the coin is pinned with `header_layout`. RIC and MEWC swaps therefore complete with the shared coin
  registry untouched; the registry maintainer may still add the key (`"riecoin"`,
  `"kawpow_auxpow_mixed"`) to pin the layout, and other implementations ignore it. Detail: PR #62,
  PR #67; CRD ch.37 §37.5.9.
- **PIVX and LBRY (LBC) swaps can now complete.** Their block headers are now read correctly.
  Before, a PIVX swap failed at the final step, after the taker had already paid, because the
  headers it needs to set the spend's time lock could not be read; LBC had the same gap. PIVX headers
  carry an extra field only in some chain versions (the headers are 80 or 112 bytes depending on the
  version), and LBC headers always carry one (112 bytes); both are now read exactly, as stored and as
  served. A PIVX header of version 7 is no longer misread. Detail: PR #82; CRD ch.37 R37.5.5.
- **A UTXO coin whose `signature_version` is not recognized now fails to activate instead of signing
  with a different algorithm, unless the new node-level boolean
  `coin_conf_unrecognised_enum_falls_back` is `true`.** The accepted values are `base`,
  `witness_v0`, `fork_id` and `fork_id_rxd`; any other value used to be silently replaced by the
  coin's default, so its transactions were signed wrongly and rejected by the network. RXD's
  `fork_id_rxd` was such a value until the Radiant signature hash was implemented (see Added). This
  is a deliberate divergence from GLEEC KDF, which falls back silently. By default activation now
  reports the offending value and the accepted set; with the switch set to `true` the coin activates
  with the default, as under GLEEC KDF, and the node logs one warning per activation naming the key
  and value. A coin with no `signature_version` is unchanged. The switch is named for
  coin-configuration enumerations in general; `signature_version` is its only reader so far. Detail:
  PR #54.
- **A malformed or non-string `fork_id` in a UTXO coin's configuration now fails the activation
  instead of crashing the node or taking the default.** A string such as `"0xZZ"` used to panic; a
  number such as `64` was silently replaced by the coin's default. An absent `fork_id` is unchanged,
  and the switch above does not apply: a malformed value is not an unrecognized enumeration member.
  Detail: PR #54.
- **UTXO refund polling no longer waits a full hour when the lock time is nearer.** The refund
  eligibility check floored its "wait this long" answer at one hour, so a refund that became
  spendable in minutes was re-checked an hour later. The wait is now capped at one hour instead;
  nothing signed changes and no funds were at risk. Detail: PR #60; CRD ch.37 §37.6.
- **After a restart, the transaction history of UTXO coins no longer lists every earlier
  transaction twice.** Each restart re-read the stored history under a wrong key, fetched every
  transaction again as if it were new, and stored it a second time beside the old copy, so RIN,
  KMD, LTC and every other UTXO coin showed two identical rows for each transaction it had before
  the restart. A history already doubled on disk heals itself: the first sync after the upgrade
  rewrites it with one row per transaction, and nothing needs to be deleted. Detail: PR #81;
  CRD ch.38 R38.5A.4.
- **The transaction history now lists transactions in a fixed order.** UTXO and QRC20 histories
  list unconfirmed transactions first, then newer blocks before older ones, and transactions of
  one block always in the same order. Before, transactions of one block could swap places
  between two refreshes, and a wallet that merges history pages by position could show a
  transaction twice and miss another. On a node that reads its transactions from a coin daemon
  instead of Electrum, a transaction's block height is now the real height, not its position in
  the block. Detail: PR #81; CRD ch.38 §38.5A.
- **One malformed transaction no longer stops a coin's transaction history.** A transaction whose
  input points past the end of the previous transaction's outputs, a BCH transaction with no
  outputs, or an SLP token whose creation carries no decimals made the history task panic, and the
  coin's history stopped updating until a restart. Such a transaction is now logged and skipped, and
  the rest of the history keeps updating. Detail: PR #84; CRD ch.38 R38.5A.9.
- **Unconfirmed BCH and SLP transactions in the history now get their block height once mined.**
  The history compared the hash in two different forms that never matched, so a BCH or SLP
  transaction first seen unconfirmed stayed unconfirmed in the history. Detail: PR #84; CRD ch.38
  R38.5A.10.
- **Two withdrawals in quick succession from one address no longer fail with a mempool conflict.**
  When the first withdrawal was broadcast through this node (`send_raw_transaction`) and the
  Electrum server still listed the spent coins, the second withdrawal could pick the same coins and
  its broadcast was refused. A withdrawal's spend is now remembered once its broadcast is accepted,
  so the next withdrawal skips those coins and may spend the change. A withdrawal that is never
  broadcast, or whose broadcast is refused, hides nothing. Two withdrawals both built before the
  first is broadcast still pick the same coins. Detail: PR #181; CRD ch.38 R38.5B.9.

#### Logs and error texts

- **The native coin daemon's and the Sia wallet daemon's error texts no longer quote the daemon's raw
  answer.** They carry the daemon's error code, its message and the answer's size only. Detail:
  PR #163; CRD ch.38 §38.6.9, ch.20 §20.8.
- **The log line about an abandoned HTTP request now says which host it was.** When a request
  finished after its caller had stopped waiting, the node logged only "drive03 receiver is dropped".
  This happens, for example, each time an EVM node misses the failover cutoff and the next node
  answers. The line now names the scheme, host and port, never the path, query or credentials, and
  says whether the request reached its time limit, got a late answer, or failed (only the kind of
  the failure, without the error text). It stays one line per abandoned request at the normal log
  level. Detail: PR #165; CRD ch.35 R35.1.17.
- **HTTP error messages no longer show the full URL of the server.** An error from a request to an
  NFT provider, the 1inch API, the market-maker price service, a gRPC-web server or, in the browser
  build, a Tendermint node used to print the whole URL, path and query included, and so any API key
  in them. Such an error now names only the scheme, host and port. The Telegram notifier's errors
  are covered too, though none is shown today. Detail: PR #165; CRD ch.35 R35.1.17.

#### EVM coins

- **A gas estimate now carries a fresh fee, and a fee below the chain's base fee gets its own
  error.** A node may check the fee sent with a gas estimate against the current block base fee.
  - **Before.** The fee sent could be a gas price reused from the short fee cache of order posting,
    or one a rising base fee had already overtaken. The node then refused the estimate, and the call
    failed with a raw transport error. That hit `setprice`, `buy`, `sell`, the trade preimage, a
    withdraw, and the approval a token swap sends before its payment. A failed `setprice` also
    cancelled the previous orders of that pair. No running swap was left stuck by it.
  - **The fee sent now.** Every gas estimate carries the fee the transaction will be signed with,
    read fresh for it: both EIP-1559 caps of the tier under `Low`, `Medium` and `High`, or the gas
    price under `Legacy`. The approval a token swap sends is estimated and signed with the same fee.
  - **When a node still refuses.** The fee is read once more and the estimate retried. A second
    refusal, or a broadcast refused for the same reason, returns the new error `FeeCapBelowBaseFee`,
    with the fee offered and the base fee. Either refusal also drops the cached gas price of that
    chain. `send_raw_transaction` reports the same text.

  Detail: PR #174; CRD ch.56 R5, R6, D5.
- **An EVM node that answers with an error no longer ends the request; the next configured node is
  tried.** Before, the first node's answer was accepted even when it carried an error object, such as
  "block range too large" or a revert, so one picky node failed the call although the others could
  serve it. Now the nodes are tried one at a time, a refusing node counts as a failed node, and the
  node that finally answers is tried first the next time. When every node fails, the caller gets the
  last node's own error. Detail: PR #90; CRD ch.35 §35.1.F.
- **A swap or withdraw on an EVM coin no longer waits for all nodes to agree on the nonce.** One
  lagging node used to hold the transaction indefinitely. Now the highest pending nonce wins and the
  signed transaction is sent at once to every node that reported it; the send succeeds as soon as one
  of them accepts it. A transaction you broadcast yourself with `send_raw_transaction` is tried node
  by node instead. Detail: PR #90; CRD ch.35 §35.1.F.
- **EVM swaps now use the gas limits from your coins file instead of one fixed number.** The coins
  file can set per-coin limits (`gas_limit` for the HTLC calls, sends and refunds, `gas_limit_v2` for
  the newer swaps), and a token inherits its platform coin's block. They used to be ignored in favor
  of 150000 for every swap call, which is too low on some chains, so swap transactions on chains such
  as Base and Arbitrum could fail on-chain. Limits are now read when the coin is activated and used
  for every swap call, the trade-fee estimates and the approval; a value that is not a whole number
  stops that coin from activating and names the key. Where the coins file sets nothing, the defaults
  now match the other KDF implementations: for example a platform-coin payment is 65000 and its
  refund 100000, so the trade fee shown for a platform coin goes down. Detail: PR #105; CRD ch.57
  R57.5, R57.6.
- **The per-coin `estimate_gas_mult` setting in the coins file is now honored for the ERC-20 approval
  a swap sends.** Some layer-two coins (the public coins file sets it to 1.3 on its Arbitrum and Base
  entries) ask for the gas the node estimates for the approval to be multiplied, and a token takes
  its platform coin's value unless it sets its own. It used to be ignored, so an approval could run
  out of gas on those chains. The setting is read when the coin is activated (anything other than a
  number stops the coin from activating and names the key) and applied only to that approval's
  estimated gas, rounded down; the fixed gas limits and the trade-fee quote are not scaled. Detail:
  PR #107; CRD ch.57 R57.5.3b.
- **An EVM gas price is no longer floored at +1 gwei.** The proportional increase added a fixed
  gigawei whenever its own increment was smaller, so sends and swap fee quotes on sub-gigawei
  chains (Ethereum today, Arbitrum, Base, BNB) overpaid or overquoted by up to hundreds of times,
  and the trade-preimage path applied the floor twice. The increase is now purely proportional.
  Detail: PR #59; CRD ch.57 R57.3.2.
- **The swap gas-fee policy now takes effect.** `set_swap_gas_fee_policy` stored a policy and
  `get_swap_gas_fee_policy` echoed it, but nothing read it, so every swap transaction was signed in
  the legacy form and `High` behaved exactly like `Legacy`. `Low`, `Medium` and `High` now emit an
  EIP-1559 (type 2) transaction priced from the matching tier of the same fee estimate the
  fee-estimation RPC reports, and the send fails rather than falling back to the legacy form if no
  estimate can be obtained. `Legacy` stays the built-in default, for peers on a reference build at
  or before `v2.0.0-beta` that cannot read a typed transaction — but the shipped coins file puts
  ETH, MATIC, AVAX, ETH-ARB20, ETH-BASE and MOVR on `"Medium"`, so swaps of those coins and of
  their tokens now emit type 2 unless the policy is overridden at activation or with
  `set_swap_gas_fee_policy`. Six live ETH-ARB20 swaps on the `Medium` policy finished in the
  pre-beta tests (2026-10-31). Detail: PR #25.
- **EIP-1559 swap transactions are no longer priced from a stale base fee.** The fee estimate took
  the base fee of the oldest block in the fee-history window instead of the newest, so under a
  rising base fee `max_fee_per_gas` could land below the actual block base fee and the send was
  rejected; this affects the coins the shipped coins file puts on a non-legacy tier, since those
  price swaps from these estimates. An `eth_feeHistory` response that omits `baseFeePerGas`, which a
  conforming client may do, could also panic inside a swap send path and no longer can. Detail:
  PR #48.
- **A counterparty's EIP-1559 (type 2) and EIP-2930 (type 1) transaction can now be read.** Typed
  transactions — most Polygon traffic — failed to decode with `unexpected string` and took the swap
  with them (issue #14). They are now decoded through the EIP-2718 envelope, in swaps and in the
  node-RPC paths (transaction history, spend search), where the sender was previously recovered
  wrongly and the gas price reported as zero. A decoded EIP-1559 transaction exposes no single gas
  price (`gas_price()` is `None`) and transaction history reports the fee actually paid from the
  receipt instead; EIP-4844 and EIP-7702 are rejected with an error naming the type. Legacy
  decoding, encoding and ids are unchanged. Detail: PR #24.
- **An undecodable EVM transaction now says what it is.** The error names the unsupported type byte
  and the supported types, or, for a payload that is not a transaction at all, its length and a
  short prefix — instead of the opaque `unexpected string`. Detail: PR #24.
- **`enable_erc20` now works on an HD wallet.** The one-shot call refused every HD-derived EVM
  platform with `UnexpectedDerivationMethod`, so a client that knows only this call could not add an
  EVM token after startup in HD mode. It now activates the token on the platform's enabled address
  and returns one `balances` entry per known address — the same response shape as before, and
  byte-identical on a single-address platform. A `path_to_address` selecting another address is
  still refused, as is a hardware-backed platform, and only `task::enable_erc20` reports progress.
  The `MM2.json` boolean `erc20_hd_activation` set to `false` (default `true`) restores the former
  refusal. Detail: PR #28; CRD ch.35 R35.2.5.

### Tests

- **The tests run before this beta and their results are listed in
  [`docs/release-tests/0.3.0-beta.1.md`](docs/release-tests/0.3.0-beta.1.md).**
- **The ARRR HTLC key derivation is checked against keys another KDF implementation derived.** A
  maker and a taker swap made by the reference on throwaway wallets give the expected HTLC public
  keys bit for bit, and two extra cases cover the low-S rule of the signature the derivation hashes.
  Detail: PR #152; CRD ch.39 T39.5.a.
- **The shielded in-flight note lock is covered end to end.** A real light-mode ARRR coin runs over
  a real shielded wallet database against a scripted `lightwalletd`, through broadcast, rejection,
  restart, scan, expiry and concurrent sends: a broadcast send's notes are not offered to a second
  send, a rejected broadcast leaves them reusable at once, and a restart does not re-offer them.
  Detail: PR #71; CRD ch.39 T39.8.0h.
- **An EVM token that sets its own gas limits while its platform coin sets none is covered.** The
  token's own `gas_limit` block is used for its sends, swap payments, spends and refunds and for the
  fee quotes, and a limit it leaves out takes the built-in default, never a value from the platform
  coin's entry. The test uses the limits of a coins file running in production. Detail: PR #155.

### Plans touched

Every plan this release touched in any sense since v0.2.0-beta.1, with what happened to it. Each plan
file carries the detail. "New" means the plan was created in this cycle.

- [`arrr-derived-htlc-key`](docs/plans/arrr-derived-htlc-key.md) — new and done: ARRR's per-swap
  HTLC key is derived as other KDF nodes derive it (PRs #140, #152), recovery signs with the key that
  matches the payment (PR #142), and the refund waits for the chain's median time (PR #146); the
  field recovery of the stuck September swap passed on 2026-10-29.
- [`arrr-htlc-electrum-fallback`](docs/plans/arrr-htlc-electrum-fallback.md) — new and done: ARRR
  swaps get their HTLC answers from the wallet's own send record and, as a fallback, from Electrum,
  and a taker that no source could watch is refused (PRs #120, #125, #130, #136); the field check
  was waived for lack of a suitable public server, and one naming detail is recorded as deferred
  work (PR #160).
- [`arrr-htlc-lightwalletd-txwalk`](docs/plans/arrr-htlc-lightwalletd-txwalk.md) — new, planning for
  after the beta: answering the ARRR swap questions from the blocks `lightwalletd` serves, without an
  address index, and then retiring the Electrum fallback (PR #151).
- [`arrr-ironwood-compatibility`](docs/plans/arrr-ironwood-compatibility.md) — new, in progress: ARRR
  builds the version-6 transactions of the Ironwood upgrade; the reorg wedge was fixed (PR #31), the
  lightwalletd transport moved to tonic 0.14 on hyper 1 (PRs #37, #42), endpoints that stopped
  following the chain are excluded (PR #51), activation warns about the default scan window
  (PR #61), and field testing was green-lit on 2026-10-18.
- [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md) — new, planning:
  ARRR native mode, the Ironwood pool and Pirate1 addresses for v0.4.0; native mode is refused in
  this release (PR #115).
- [`clippy-ci-gate`](docs/plans/clippy-ci-gate.md) — new, parked: the gate shipped (PR #27); the
  vendor-filter follow-up stays open.
- [`dependabot-triage-2026-09`](docs/plans/dependabot-triage-2026-09.md) — new and done: the
  September 2026 dependency alerts were triaged.
- [`electrum-measured-server-order`](docs/plans/electrum-measured-server-order.md) — new, in
  progress: the `mixed` and `measured` server orders shipped (PR #144); making `mixed` the default
  waits for a field run on RIN.
- [`evm-min-trading-volume`](docs/plans/evm-min-trading-volume.md) — new, in progress: the minimum
  trading volume matches the reference for EVM, QRC20, TRON, UTXO and Lightning coins (PR #66); an
  end-to-end preview on a real 8-decimal coin is left.
- [`evm-swap-gas-multiplier`](docs/plans/evm-swap-gas-multiplier.md) — new, blocked: per-chain gas
  limits in the coins file, not the multiplier, are the knob (PR #123), and the coins repository
  has carried a block measured on Monad for the MON tokens since 2026-10-28; the two remaining
  options wait on the reference maintainers.
- [`evm-test-infrastructure`](docs/plans/evm-test-infrastructure.md) — new, planning: two dead-network
  EVM tests ported to a scripted local node (PR #30), and tests against a local `geth` node run in
  CI (PR #53).
- [`faster-transaction-visibility`](docs/plans/faster-transaction-visibility.md) — new and done: push
  notifications and pending receipts shipped with tests; they are listed under 0.3.0-alpha.1, and the
  work predates pull requests.
- [`gossip-send-queue-bound`](docs/plans/gossip-send-queue-bound.md) — new and done: each peer's
  gossip send queue is capped (PRs #121, #122), and the cap is listed in the compatibility document.
- [`librustzcash-upgrade`](docs/plans/librustzcash-upgrade.md) — done: the upgrade with a
  schema-safe rescan finished; a note records that the stack has moved again since.
- [`lightning-activation-features`](docs/plans/lightning-activation-features.md) — new, parked: it
  points to the trading-floors work (PR #69) and records, for after the beta, that a Lightning node
  whose activation failed keeps its listening port (PR #156).
- [`lightning-ldk-upgrade`](docs/plans/lightning-ldk-upgrade.md) — new, parked: scoping only; it
  records the local LDK patch (PR #66).
- [`maker-gossip-rate-control`](docs/plans/maker-gossip-rate-control.md) — new, parked: a maker's
  order messages can leave at a bounded rate that drops when a relay falls behind (PRs #129, #143,
  #148), with defaults measured in the lab (PR #159) and the follow-ups done (PR #160). After a
  field run it became opt-in (PR #166); the plan lists what comes before it is the default again,
  and two deferred items await field data.
- [`my-orders-speed`](docs/plans/my-orders-speed.md) — new and done: `my_orders` answers about 4 to
  12 times faster with 2,000 orders (PR #154).
- [`order-posting-latency`](docs/plans/order-posting-latency.md) — new, in progress: posting orders
  makes fewer Electrum and node round trips (PRs #95, #114, #116, #128) and no longer does I/O under
  an order's lock (PR #149); the timing run of 2026-10-31 found posting faster than before, and the
  cost of spreading reads over a slow server is documented (PR #187).
- [`orderbook-queue-and-history-caps`](docs/plans/orderbook-queue-and-history-caps.md) — new, in
  progress: the order-book store applies queued updates in bulk, purges unused entries early and
  keeps at most 2,048 changes per maker and pair (PR #164); one open point on the early purge is
  left.
- [`orderbook-wipe-recovery`](docs/plans/orderbook-wipe-recovery.md) — new, in progress: a maker's
  order messages keep its book alive (PR #132), a relay holds a sync answer while it catches up
  (PR #135), and a variant of the short retry the plan did not recommend shipped as a second sync
  request after a quick failure (PR #173); the lab run found no burst of failed synchronizations, and
  one variant moves to after the beta.
- [`qtum-qrc20-stabilization`](docs/plans/qtum-qrc20-stabilization.md) — parked: given a status
  header; the analysis is unchanged.
- [`rpc-api-reference-wire-parity`](docs/plans/rpc-api-reference-wire-parity.md) — new, parked: the
  coin-activation wire contract shipped (PR #17); per-coin serialization of activations (PR #156)
  and the refusal of a future ARRR sync date (PR #22) are recorded as implemented.
- [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md) — new, parked:
  Radiant signing shipped (PRs #54, #65); its compatibility bookkeeping and one post-beta step are
  open.
- [`secp256k1-migration`](docs/plans/secp256k1-migration.md) — done: its last follow-up closed, the
  real WASM link step is back in CI (PRs #26, #27).
- [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md) — new, in
  progress: a shielded send's notes stay reserved on the swap and `withdraw` paths (PR #49), the
  shortfall is named (PR #50), and a harness covers the lock (PR #71); two daemons on one seed are
  not yet detected.
- [`shielded-sync-cache-reuse`](docs/plans/shielded-sync-cache-reuse.md) — parked: given a status
  header; not started.
- [`sia-withdraw-fee-control`](docs/plans/sia-withdraw-fee-control.md) — new and done: a Siacoin
  `withdraw` honors a custom fee (PR #77), checked live on 2026-10-26.
- [`solana-sdk-upgrade`](docs/plans/solana-sdk-upgrade.md) — new, parked: deferred; the SDK is pinned
  upstream and would need a fork.
- [`solana-support`](docs/plans/solana-support.md) — new, parked: Solana is switched off for this beta
  (PR #93).
- [`swap-transaction-path-hardening`](docs/plans/swap-transaction-path-hardening.md) — new, planning:
  a stub to elaborate before any work is scheduled (PR #90).
- [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md) — new, parked:
  Tendermint coins are wallet-only for this beta (PR #93).
- [`trading-floors-parity`](docs/plans/trading-floors-parity.md) — new, in progress: every coin
  family's trading floors are specified (PR #69) and implemented (PRs #66, #72, #73), and the lab run
  of 2026-10-31 passed, including a swap at the minimum volume for KMD, LTC, DOGE and DINGO.
- [`tron-gasfree`](docs/plans/tron-gasfree.md) — new, parked: waits on pinning the GasFree proxy's
  creation bytecode; TRON is off for this beta (PR #93).
- [`tron-support`](docs/plans/tron-support.md) — new, parked: how TRON comes back in the next beta
  (PR #138).
- [`tron-swap-support`](docs/plans/tron-swap-support.md) — new, parked: branch-only; TRON is off for
  this beta (PR #93).
- [`utxo-header-family-selection`](docs/plans/utxo-header-family-selection.md) — new, in progress:
  RIC and MEWC swaps complete through a per-coin `header_layout` and a fallback switch (PRs #62,
  #67), the layout reaches every header read (PR #74), PIVX and LBC headers are read (PR #82), and
  AuxPoW is recognized by its version flag (PR #55); the remaining family rules and step H5 are
  open.
- [`v0.2.0-dependency-hygiene`](docs/plans/v0.2.0-dependency-hygiene.md) — in progress, the standing
  advisory ledger: the rustls, webpki, ring and tungstenite rows cleared, and the ledger is green
  with one dated acceptance, RUSTSEC-2026-0387 (PRs #37–#40, #42, #44, #68); redaction inside
  `sia-rust` is recorded as deferred work (PR #160).
- [`v2-dynamic-fee-spend-estimate`](docs/plans/v2-dynamic-fee-spend-estimate.md) — new, parked:
  options recorded, none chosen; Reloaded stays at parity.
- [`v2-swap-engine-hardening`](docs/plans/v2-swap-engine-hardening.md) — new, parked: steps T1–T7 and
  T9–T10 shipped; T8 is deferred and T11–T13 are queued.
- [`walletconnect-fork-upgrade`](docs/plans/walletconnect-fork-upgrade.md) — new and superseded: a
  `[patch]` onto upstream `tokio-tungstenite-wasm` 0.4.0 replaced the fork bump (PR #40).
- [`withdraw-entry-expiry-by-block-time`](docs/plans/withdraw-entry-expiry-by-block-time.md) — new,
  planning for after the beta: a withdrawal's recorded spend would expire after a multiple of the
  chain's real block time, and the record would be kept per address for HD wallets (PR #180).

### Known limitations

What does not work yet, or works only in part, in this release. Each item says where more detail
lives.

- **Do not run two KDF instances on the same seed at the same time.** If you do, for any coin (RIN
  included), both instances can pick the same funds for two different transactions. The network
  accepts one and rejects the other, and the instance that loses gets no explanation and may try
  again. KDF guards against this inside a single instance only; shielded coins such as ARRR are the
  known case. More in [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md).
- **A withdrawal that drops out of the mempool keeps its coins set aside until the coin is enabled
  again or the node restarts.** Once a withdrawal's broadcast is accepted, the node no longer offers
  the coins it spent, so the next withdrawal does not pick them again, and it offers the change
  instead. If the transaction later leaves the mempool unconfirmed, for example because its fee was
  too low or it was replaced, those coins stay set aside and its change stays offered, so a
  withdrawal that picks that change can fail. Disabling and enabling the coin again, or restarting
  the node, clears this. In an HD wallet, change that a withdrawal sends to another address of the
  coin is offered only once the Electrum server lists it. More in
  [`withdraw-entry-expiry-by-block-time`](docs/plans/withdraw-entry-expiry-by-block-time.md).
  Detail: PR #181; CRD ch.38 R38.5B.8, D38.5B.2, D38.5B.4.
- **A withdrawal with an explicit fixed fee can cost more than on Komodo or GLEEC KDF.** When a UTXO
  `withdraw` gives its fee as `{"type":"UtxoFixed","amount":…}`, KDF Reloaded charges that amount for
  every started kilobyte of the transaction, while Komodo and GLEEC KDF charge it in proportion to
  the size. For the same request, a 245-byte KMD withdrawal paid the whole fee here and about a
  quarter of it on the reference, and a 1,404-byte DOGE withdrawal paid twice the fee here and about
  1.4 times it there. You never pay less than the fee you entered, and a withdrawal without `fee`,
  swaps and the DEX fee are not affected. Parity is planned after the beta. More in the
  [GLEEC compatibility guide](docs/GLEEC_COMPATIBILITY.md). Detail: PR #72.
- **The swap watcher does not work yet.** A watcher is another node that finishes a swap for you
  when you are offline. No swap hands its data to a watcher yet, for any coin (issue #56), so keep
  your node running until your swaps have finished. Bitcoin-family coins (UTXO coins such as RIN,
  BCH and QTUM) also no longer offer to watch other traders' swaps, because the checks a watcher
  runs are not implemented for them; only Siacoin (SC) offers watching. Detail: PR #177; CRD ch.09
  R17.
- **A maker that changes its orders many times a second can drop out of the order book of a relay
  with a slow link.** In a test, a node that kept 480 orders and changed their prices about 80 times
  a second sent its updates to a GLEEC KDF relay whose incoming link was limited to 100 kbit/s. Within
  2 to 4 minutes the orders were gone from that relay's order book, and from the nodes behind it, for
  the rest of the test; the maker itself reported no errors. Setting the `MM2.json` boolean
  `maker_update_coalescing` to `true` kept all 480 orders visible in the same test. It is off by
  default because it has a cost: price changes reached other nodes about 20 to 25 seconds later, for
  5 minutes after the relay fell behind other nodes learned changes only through the keep-alive
  synchronization, and the relay logged 6 to 18 failed order-book synchronization requests a minute.
  More in [`maker-gossip-rate-control`](docs/plans/maker-gossip-rate-control.md). Detail: CRD ch.32
  D9.
- **Other nodes can keep showing a cancelled order for a while if its cancellation is lost on the
  way.** Such an order cannot be matched or traded, and it disappears once the maker posts again on
  that pair or goes offline. Detail: PR #188; CRD ch.32 D12.
- **ARRR (Pirate) atomic swaps work only between two KDF Reloaded nodes.** ARRR now uses a newer
  transaction format (v6, from the Ironwood upgrade) that no other KDF implementation can handle, so a
  swap with a Komodo or GLEEC counterparty cannot complete. A swap between two KDF Reloaded nodes has
  completed on the live chain, including the spend of the locked ARRR (October 2026). More in
  [`arrr-ironwood-compatibility`](docs/plans/arrr-ironwood-compatibility.md).
- **Shielded coins such as ARRR cannot be activated in a browser (WASM) build.** Activation ends with
  an error saying shielded activation is not available in the WebAssembly build: the browser build
  has no shielded wallet database, and building a shielded transaction would also need the Sapling
  prover, which only the native build has. Use a native build for ARRR. Detail: CRD ch.39 R39.10.1,
  §39.6.1.
- **Siacoin (SC) swaps are not available.** Sending, receiving, the balance and the history work.
  Atomic swaps of SC are postponed until after the beta; the design is still a draft. More in the
  [Siacoin swap draft](docs/reloaded-rewrite/54-sia-swap-v2-path.md).
- **Radiant (RXD) is only partly usable.** RXD activates and signs, but swaps with it are not
  established. More in
  [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md).
- **TRON (TRX, TRC20) and Solana (SOL, SPL) cannot be activated in this beta.** Their support is not
  finished. TRON activation answers "TRON support is not available in this build";
  `enable_solana_with_tokens` and `enable_spl` answer "Solana support is not available in this
  build"; the rest of the node is unaffected. Both are planned to return in the next beta. More in
  [`tron-support`](docs/plans/tron-support.md) and [`solana-support`](docs/plans/solana-support.md).
  Detail: PR #93.
- **Tendermint (Cosmos) coins are wallet-only in this beta.** Activation, balances and withdrawals
  work; orders and swaps are planned to return in a later release. A swap already in progress still
  completes, and an order with a Tendermint coin saved by an earlier version is cancelled at startup.
  More in [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md). Detail:
  PR #93.
- **Swaps of Monad tokens need a coins file from October 28, 2026 or later.** On Monad, a token's
  swap payment needs more gas than the default limit of 150000 (about 158000 for USDC-MON). Coins
  files from that date on give each Monad token, such as USDC-MON, its own `gas_limit` block, which
  KDF Reloaded uses. With an older file the payment runs out of gas and fails on-chain: its gas fee
  is spent, the tokens stay in the wallet and the swap stops. A counterparty whose payment is already
  locked gets it back by refund once the swap's lock time passes. More in
  [`evm-swap-gas-multiplier`](docs/plans/evm-swap-gas-multiplier.md). Detail: PR #105; CRD ch.57
  R57.5.2, R57.5.3.

## [0.3.0-alpha.3] — development milestone, not released

### Fixed

- **A shielded wallet no longer binds to a lightwalletd endpoint that never followed the
  Ironwood upgrade.** Once the Ironwood activation time has passed, an endpoint still
  reporting the pre-Ironwood consensus branch is excluded outright, however many endpoints
  share that view; if no current endpoint is reachable the wallet fails closed instead of
  building against a frozen tip and broadcasting a v6 transaction to a node that cannot
  decode it (`TX decode failed`, code -22). A wallet whose recorded branch is the
  pre-Ironwood one (every wallet that synced before the upgrade) is released from that
  binding after activation, which previously excluded every current endpoint and left only
  the stale ones (CRD ch.39 R39.11.7a(h)).
- **Endpoint admission is now logged.** Each evaluation logs the endpoints probed with
  their reported chain, branch and height, every exclusion with its reason, unreachable
  endpoints and the admitted set (R39.11.7a(i)).
- **A shielded-coin sync date in the future is rejected instead of clamped.** A
  `sync_params` `date` later than **both** the wall clock and the chain tip's block time
  now ends activation with `CoinCreationError`, matching the reference. The two
  reference points are both needed: a date minutes after the tip is ordinary on a chain
  that has not mined since, so the tip's time alone would reject it, while block times
  can run ahead of the clock, so the clock alone would reject a date a real block
  already covers. A date between the two resolves to the tip. A start given as a
  **height** beyond the tip is unaffected and still clamps to the tip (CRD ch.39
  R39.10.11, R39.8.0h).
- **A shielded-coin sync date before the genesis block no longer hangs activation.** The
  binary search that resolves a date to a height could not terminate when the target
  preceded block 0's timestamp: the window stayed pinned at `0..=0` and the search
  re-fetched block 0 over the network indefinitely. Such a date now resolves to height 0.
- **Custom ERC20 tokens are wallet-only.** A token activated from an inline
  `protocol` (`enable_eth_with_tokens` / `enable_erc20`, a token absent from the
  coins file) now reports itself wallet-only, so `buy`, `sell` and `setprice`
  reject it. Previously the wallet-only check read the coins file, which has no
  entry for such a token, so an unvetted token could be offered in swaps. This
  matches the reference behavior bound in CRD ch.35 R35.2.7 (issue #16).

## [0.3.0-alpha.2] — development milestone, not released

### Upgrade note: EVM address under an HD wallet

**If you use an HD wallet (`enable_hd: true`) and activated ETH, another EVM
coin or ERC20 tokens with `enable_eth_with_tokens` or `task::enable_eth` on an
earlier KDF Reloaded build, the activated address changes with this release.**

Earlier builds activated EVM coins under an HD wallet at the address of the
daemon's internal identity key instead of the address at the coin's
`derivation_path` (`m/44'/60'/...`) plus `path_to_address`. GLEEC KDF and every
BIP-44 wallet use the derivation-path address, so the same seed phrase showed a
different EVM address in Reloaded than elsewhere. This release activates the
derivation-path address (CRD ch.35).

Funds received at the old address are not lost -- its key is still derived from
your seed phrase -- but the wallet no longer shows them. To move them:

1. Start KDF Reloaded in single-address mode: `enable_hd: false` in `MM2.json`,
   with the **same seed phrase** as `passphrase`.
2. Activate the EVM coin. The address it reports is the old address; check its
   balance.
3. Withdraw the funds to your HD address (the address the HD wallet now shows
   for that coin), then restart in HD mode.

Legacy `enable` for EVM coins under an HD wallet is not changed by this release.

### Changed

- **A Light-mode ZHTLC (shielded) coin is served by `lightwalletd` alone.** Electrum
  support for the coin is removed; there is no switch to restore it (see
  `RELOADED_VS_GLEEC.md` and `docs/GLEEC_COMPATIBILITY.md`). `light_wallet_d_servers` is
  now mandatory and non-empty; `electrum_servers` is accepted and ignored. Configured
  endpoints are admitted by `GetLightdInfo` (network, Sapling height, consensus branch)
  before use, so an endpoint on another fork can no longer pull the wallet onto its
  chain. Every shielded construction path -- including HTLC spend and refund, which
  previously took their height from the Electrum client -- now takes its builder target
  height from the one lightwalletd tip and refuses to build when it cannot be
  corroborated. **A fresh wallet whose configuration cannot name the active network
  upgrade needs two agreeing reachable endpoints**: the branch is decided by strict
  majority and recorded with the sync state, so a single reachable endpoint cannot
  establish it. A wallet that has already scanned blocks needs only one, and stays on
  the branch recorded with them. Swap-path HTLC spend detection over lightwalletd is
  implemented: the transparent index is queried under both address encodings, because
  Pirate files a pay-to-script-hash row under the pubkey-hash key type, and a negative
  answer requires a demonstrably healthy endpoint -- "not spent" is never reported when
  the truth is "could not tell" (CRD ch.39 §39.11, ARRR Ironwood plan Step C).
- **ARRR transaction parsing is routed to the Zcash reader.** The shared UTXO parser
  applies the version-4 layout with double-SHA256 identifiers and **refuses** a
  version-6 transaction outright, so the swap step failed rather than proceeding on a
  wrong identifier. Every ZHTLC swap-path parse now goes through
  `zcash_primitives::Transaction::read`, which keeps the version-4 history an old wallet
  still needs when restoring from a pre-fork birthday (CRD ch.39 §39.11 R39.11.16,
  ARRR Ironwood plan Step D).
- **The Zcash crate stack is re-vendored on `zcash_primitives` 0.30.1** (with
  `zcash_transparent` 0.10.0, `zcash_client_backend` 0.24.0, `zcash_client_sqlite`
  0.22.0 and `zcash_protocol` 0.10.6), because version-6 reading was behind the
  `zcash_unstable = "nu7"` rustc cfg in 0.28.0 and is unconditional in 0.30.1. The
  `zcash_client_backend` vendor copy is dropped: its only patch was an exact `time-core`
  pin that upstream has since removed. `orchard` is a default feature of the new
  `zcash_client_backend` and `zcash_client_sqlite`, so both are taken with
  `default-features = false` -- no Orchard or Ironwood bundle is built
  (ARRR Ironwood plan Step B).

### Added

- **Coin-activation wire contract, wave 1 (CRD chapter 55).** The task
  activation families now answer with the bound wire shapes:
  - `cancel` of every task family answers `NoSuchTask` (integer task id,
    HTTP 404) for an unknown, forgotten or already cancelled task,
    `TaskFinished` (integer task id, HTTP 409) for a finished task whose
    result has not been forgotten, and `Internal` (string, HTTP 500)
    otherwise; the Lightning task no longer maps every failure to `Internal`.
    A cancelled activation deregisters a coin it had already registered, and
    a cancelled task is no longer observable by `status`. Every `init`
    accepts the optional `client_id` (accepted and ignored; streaming of task
    events is deferred).
  - `task::init_trezor::init` accepts no parameters, `null`, `{}`, a
    `device_pubkey` and a `client_id`; its success details are the device
    object (`type`, `model`, `device_name`, `device_id`, `device_pubkey`); its
    errors are `HwContextInitializingAlready`, `HwError` (string vocabulary,
    HTTP 410), `UnexpectedUserAction`, `Timeout` and `Internal`; and the new
    `task::init_trezor::cancel` returns the hardware-wallet context to the
    not-initialised state.
  - `task::enable_utxo`, `task::enable_qtum` and the new
    `task::enable_bch` share the bound standalone-coin error set (including
    `HwError` and `Transport`, without the private-key and
    derivation-method discriminants), report a lost registration race as
    `CoinIsAlreadyActivated`, put `ticker` and ticker-keyed balances in the
    success object, report `FollowHwDeviceInstructions`, accept
    `priv_key_policy` as a string or as `{"type": ...}`, honor
    `path_to_address` for a software HD account, accept a per-server
    `timeout_sec` and default the WebAssembly Electrum protocol to `WSS`.
    `task::enable_bch` activates the BCH coin only, for a single address.
- **Tendermint and Lightning activation wire contract (CRD chapters 36, 41).**
  Tendermint platform activation reports an empty node list, an unusable node
  URL, `decimals` above 18 and unreachable nodes as `PlatformCoinCreationError`
  (HTTP 500) instead of `AtLeastOneNodeRequired` / `Transport` (502);
  `tokens_params` is required; a token naming another platform is
  `PlatformCoinMismatch`; `enable_tendermint_token` answers
  `CouldNotFetchBalance` for a balance failure. A request carrying
  `activation_params` is now rejected with `PlatformCoinCreationError` instead
  of silently activating with the local key, until public-key and WalletConnect
  activation are implemented. The Lightning task uses the documented error names
  (`L2IsAlreadyActivated`, `L2ConfigIsNotFound`, `UnexpectedL2Protocol` with its
  `protocol`, `InvalidPlatformConfiguration`, ...), checks the platform's
  `avg_blocktime` at `init`, and its default node colour is the documented
  `2b6680`. A protocol without parameters is reported in `error_data` without a
  `protocol_data` key.
- **Z-coin, Sia, legacy and BCH/SLP activation wire contract (CRD chapters 39,
  46, 55).** `task::enable_z_coin` accepts an optional `mode`, the `"Electrum"`
  and `servers` synonyms and `min_connected` / `max_connected`, reports a
  lightwalletd outage after the sync has started as the in-progress value
  `{"TemporaryError": ...}` and keeps retrying, and reports post-build height
  and balance failures as `Transport`; it is routed on WebAssembly but answers
  `CoinCreationError` there (no shielded wallet database). Sia reports an
  unsupported key policy as `CoinCreationError` and post-build failures as
  `Transport`, and legacy `enable` activates Sia coins. Legacy `electrum` bounds
  its balance read to 20 s, and both legacy methods deregister a coin whose
  balance read failed. `enable_bch_with_tokens` honours `get_balances`, checks
  the token platform and rejects a hardware key policy; `enable_slp` records the
  token on its platform coin. The mmrpc 2.0 EVM activation defaults
  `required_confirmations` to 3 when neither the request nor the coin config
  sets it.
- **Two compatibility switches** (see `docs/GLEEC_COMPATIBILITY.md`):
  `sia_activation_single_address_balance` (Sia activation result under an HD
  wallet) and `legacy_enable_keeps_coin_on_balance_failure` (legacy `enable`
  after a failed balance read).
- **EVM activation wire contract, wave 2 (CRD chapter 35).**
  `enable_eth_with_tokens` and `task::enable_eth`:
  - every request field of the chapter is honored: both forms of `nft_req`
    (inline owned-inventory fetch, reported in the always-present
    `nfts_infos`), `get_balances` (with `tickers` instead of balances),
    `swap_v2_contracts`, `swap_gas_fee_policy`, the gas-station fields,
    `rpc_mode` (WebAssembly only, paired with the MetaMask policy),
    `priv_key_policy` as an object (`ContextPrivKey`, `Trezor`, `WalletConnect`
    with `params.session_topic`, `Metamask` on WebAssembly), inline `protocol`
    custom tokens, and a missing `swap_contract_address` for a wallet-only
    coin;
  - an HD daemon now activates the address at the coin's `derivation_path`
    plus `path_to_address` (it used to bind the daemon's internal identity
    key) and answers the HD shape (`ticker`, `wallet_balance`, `nfts_infos`);
    the Trezor task answers the HD shape too; `pubkey` is the `04`-prefixed
    uncompressed key without `0x`;
  - every node unresponsive, or a failing chain-height, decimals or balance
    read, is `Transport` (string, HTTP 502) instead of
    `PlatformCoinCreationError`/`Internal`; invalid node URLs and swap
    contracts are `InvalidPayload`; absent `nodes` is `AtLeastOneNodeRequired`
    (string); the hardware policy on the one-shot call is
    `UnexpectedDeviceActivationPolicy`; `PrivKeyNotAllowed` is now
    `PrivKeyPolicyNotAllowed`; tokens are enabled in three passes
    (resolution, platform consistency, construction), so the first failing
    token of the earliest failing pass decides;
  - `task::enable_eth::init` answers an already active ticker at mmrpc level;
    the device confirmation is reported as `FollowHwDeviceInstructions`.

  `enable_erc20` and `enable_nft` share one single-token handler with the
  bound discriminants (`Transport` is HTTP 500 there), on-chain decimals,
  EIP-55 contract validation, lower-case `token_contract_address`, token
  confirmations from the token config, and custom tokens (`CustomTokenError`).
  The new `task::enable_erc20::{init,status,user_action,cancel}` family runs on
  a token task substrate with its own registry, answers the HD wallet report on
  an HD platform, and deregisters a token on cancel.
- **The Ironwood swap freeze and build refusal now actually engage — CRD ch.39 R39.6.4d.** Both gates read `ironwood_activation_time` from the coin configuration, and **no published coin configuration declares it** — the field is an extension introduced here, and in `GLEECBTC/coins` no coin mentions Ironwood at all. So as previously shipped the entire 3 Oct safety net was inert on every deployment but a developer's own, which the last release documented but did not fix. Pirate Chain's maintainer confirmed on 2026-09-28 that there will be **no standardness grace period** and recommended disabling ARRR swaps from 2 Oct, so the gates could not keep waiting on a third party's configuration change. This build therefore carries the activation time for ARRR (1 791 054 000 — 3 Oct 2026 19:00 UTC, read from the chain's own `KOMODO_IRONWOOD_ACTIVATION`) and applies it when the configuration supplies none. Consequently **ARRR stops accepting new swaps from 1 Oct 2026 22:28 UTC and stops building transactions from 3 Oct 19:00 UTC on a default installation**; receiving, balance, address derivation and history are unaffected, and swaps already under way are never interrupted. The earlier objection to a compiled-in date — that it could not follow a moved activation — is answered by precedence rather than abstention: a configured `ironwood_activation_time` still overrides the built-in value, so a moved date stays a data change and never needs a release; the built-in value is withheld from test and regtest parameters, which activate Ironwood at a height with no timestamp; and a coin whose ticker this build does not know is unaffected. **Compatibility note:** omitting `ironwood_activation_time` is no longer an opt-out — set `ironwood_gate_disabled: true` in `protocol.protocol_data.consensus_params` for GLEEC-equivalent behaviour. Four tests pin the resolution rules, each verified to fail against a deliberately broken implementation. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins/z_coin/z_coin_ops.rs`.
- **ARRR version-6 (Ironwood) transaction construction.** `NetworkUpgrade::Nu6_3` maps
  to the Ironwood activation height, taken from the coin configuration or discovered at
  runtime, and the builder then selects version 6 and consensus branch `0x37A5165B`
  itself. Runtime discovery is required rather than convenient, because the Pirate team
  publishes no activation height: `GetLightdInfo` supplies the branch and tip and, where
  an activation time is known, the first block past it is found by bisection, with a
  30-block depth requirement before an upcoming transition is trusted. A disagreement
  between the server and the timestamp rule refuses the build rather than defaulting.
  `IRONWOOD_V6_SUPPORTED` is now true, which makes the R39.6.4b and R39.6.4c gates inert
  by their own terms (CRD ch.39 R39.6.4e, ARRR Ironwood plan Step E).

### Fixed

- **An existing ARRR shielded wallet is migrated instead of being refused.**
  `zcash_client_sqlite` 0.22.0 adds 23 unconditional migrations, so a wallet created by
  an earlier build no longer matched the current schema and activation failed with
  "unrecognized shielded wallet schema". The wallet is now copied to
  `<wallet db>.pre-upgrade.<ms>.bak`, migrated and verified **on a working copy**, and
  only then renamed over the original, so no half-migrated wallet can exist at the real
  path; any failure after the backup falls back to preserving the file and rebuilding
  fresh. Note that an earlier build cannot read the migrated schema afterwards -- the
  backup is the way back (CRD ch.39 R39.8.0ba).
- **`sync_params: "earliest"` could not activate a shielded coin.** A scan from the
  Sapling activation height asks the server for the commitment tree one block below it,
  and there `lightwalletd` substitutes a bare 32-byte commitment root for the serialized
  tree. The wallet refused to anchor on it, which was right in itself but made an
  ordinary request fail. The substitution is now recognised by its empty
  `saplingFrontier` -- not by the tree's length, since a legitimately empty tree is also
  short -- and below the activation height it is read as the empty tree, which is correct
  by consensus rule, since no Sapling commitment can exist there. At or above activation
  it is still refused, and the message now names the height to use
  (CRD ch.39 R39.8.0ak).

## [0.3.0-alpha.1] — development milestone, not released

### Added

- **Version-two (V2) UTXO swap wire contract corrected to match deployed
  peers (issue #11 batch 2).** The version-two taker-payment-spend preimage
  now matches the reference `v2.6.0-beta`/v3-lineage contract instead of an
  earlier, incompatible one:
  - the `Standard` preimage carries the fee-collection output alone (the
    maker appends its own payout afterward), not the maker's payout with the
    fee appended;
  - the `WithBurn` preimage orders its outputs fee, then burn, then maker
    payout, not maker-first;
  - the `OP_RETURN` burn output carries the burned value in the output's own
    value with a bare `OP_RETURN` script, not a zero-value output with an
    8-byte little-endian payload;
  - the maker's validation now rebuilds the expected preimage and requires
    an exact match (no tolerance), instead of a ±10% value tolerance;
  - the version-two funding-spend and taker-payment-spend fee estimate is
    now proportional to a 496-byte reference size (matching the reference
    nodes), not the previous whole-kilobyte-rounded, 305-byte estimate;
  - the fee-collection output now resolves the active network's DEX-fee
    address via `NetConfig`, not a deprecated constant fixed to netid 8762
    (previously wire-incorrect on netid 6133);
  - each cooperative-branch signature now carries its own signature-hash
    flag byte (maker always `ALL`, taker `SINGLE` for `Standard`) — a single
    shared byte could not represent the corrected `Standard` case.
  Every version-two swap with a UTXO taker coin between this project and a
  reference node previously failed at preimage validation, on both netids,
  for any ticker. Code: `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`.
- **Version-two no-fee ticker exemption (chapter 16 R12B) is now wired into
  the version-two swap machinery (issue #11 batch 2).** A version-two swap
  with `KMD` on either side of the pair now carries `NoFee` end to end
  (both roles, before and after negotiation) on netid 8762; the accessor
  existed since the previous release but nothing in the version-two path
  consulted it. The legacy protocol is unaffected — it never applies this
  exemption. Code: `mm2src/mm2_main/src/lp_swap/dex_fee.rs`,
  `mm2src/mm2_main/src/lp_swap/maker_swap_v2.rs`,
  `mm2src/mm2_main/src/lp_swap/taker_swap_v2.rs`.
- **Version-two UTXO funding amount no longer omits the burn leg (issue #11
  batch 2).** The taker's version-two funding output value is now trading
  amount + premium + the dex-fee *total* spend amount (fee plus burn for a
  `WithBurn` descriptor), matching chapter 15 R16/R17. It previously used
  the fee component alone, which would have underfunded a `WithBurn`
  version-two swap (not currently reachable in production, since neither
  production network negotiates a version-two `WithBurn` descriptor today).
  Code: `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`.

- **Siacoin swap payment validation is now covered by tests — CRD ch.20 R-H1.** Nine cases around the check `validate_maker_payment`/`validate_taker_payment` make before a swap commits to a counterparty's on-chain payment: eight of them rejection properties (roles swapped, wrong amount, different secret hash, different counterparty key, different timelock, no outputs, HTLC funded at a decoy index). **Known limitation:** SC still has no automated end-to-end swap test — one that boots a containerised Sia node and settles a real swap on both legs — so SC swaps rest on these logic tests plus manual testing. Deferred to the next release; tracked with the rest of the Siacoin deferred work in CRD ch.20 §20.10. Code: `mm2src/coins/siacoin/siacoin_swap_ops.rs`.
- **A shielded coin now refuses to build transactions once a network upgrade it cannot transact on has activated — CRD ch.39 R39.6.4c.** The swap freeze added alongside it stops *new swaps* ahead of Pirate Chain's Ironwood upgrade, but a withdrawal passes none of the swap gates — so after activation it would still have built a version-4 transaction and failed at broadcast, surfacing as an opaque network rejection. Every ARRR transaction is now refused from the activation time onwards, at the single point they are all constructed, with an error naming the coin, the activation time and the need to upgrade. The check runs before the path's blocking wait, so a caller is refused promptly rather than made to wait first. The two cut-offs are deliberately staggered and are not merged: the freeze starts ~44.5 h earlier and blocks only *entering* swaps, because between the two a swap begun before the freeze must still be able to spend or refund itself — a build refusal starting at the freeze would strand exactly the swaps the freeze exists to protect. A test pins that ordering. Receiving, balance, address derivation and history are unaffected throughout. The refusal reaches callers as `UnsupportedAfterNetworkUpgrade` with HTTP **400**, not as an internal error with HTTP 500 as it first did: nothing has gone wrong inside the process, the condition is foreseeable and permanent until the user upgrades, and after 3 Oct *every* ARRR withdrawal takes this path — a GUI rendering “Internal error” there would read as a broken wallet rather than an actionable prompt. This follows the existing `UnsupportedUnderTrezor`/`UnsupportedUnderMetamask` precedent, which map unsupported operations to 400 (CRD R47.6.7/R50.20). Verified end to end against a running node: before activation a withdrawal reaches the balance check downstream of the guard, and with an activation time in the past it is refused by the guard with the coin and time named. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins/z_coin/z_coin_errors.rs`, `mm2src/coins/lp_coins_errors.rs`.
- **A shielded coin now stops accepting new swaps ahead of a network upgrade it cannot yet transact on — CRD ch.39 R39.6.4b.** Pirate Chain's Ironwood upgrade (3 Oct 2026 19:00 UTC) makes only version-6 transactions standard; a build that cannot construct them would be unable to spend *or refund* an HTLC funded shortly before activation, stranding one side of any swap that straddles it. A coin declaring `ironwood_activation_time` therefore reports itself wallet-only from a cut-off ahead of activation, which `buy`, `sell` and `setprice` already gate on, and additionally declines incoming peer matches — the wallet-only report is never consulted on that path, so a remote taker could otherwise still match an order already posted. The cut-off is 160 300 s (44 h 32 m): the longest maker payment lock this framework can produce (156 000 s — `PAYMENT_LOCKTIME` x 10 for the legacy slow-coin rule, x 2 for the maker leg), plus the 3 700 s refund grace the swap machines wait before acting, plus ~10 blocks for the refund to be mined. For ARRR that means trading pauses at **1 Oct 2026 22:28 UTC**. Receiving, balance, address, `withdraw`, history and every swap already in flight are untouched, and a coin declaring no Ironwood activation is never affected. Because the locktime rules live in the swap layer, which depends on the coin layer rather than the reverse, the margin is a constant in `coins` guarded by a test in `mm2_main` that fails if the two drift — that guard is what caught the first draft, which covered the lock but not the refund grace. Code: `mm2src/coins/z_coin.rs`, `mm2src/mm2_main/src/lp_swap.rs`.
- **Shielded coin configurations may now declare the Ironwood network upgrade — CRD ch.39 R39.6.4a.** `protocol.protocol_data.consensus_params` accepts two optional members, `ironwood_activation_time` (the wall-clock timestamp the upgrade activates from) and `ironwood_activation_height` (the height, once the network has derived and published it). Two members rather than one because Pirate does not fix the height in advance: every node derives it at runtime from the first block whose time exceeds the timestamp, so only the timestamp can be published ahead of the upgrade while only the height is usable as a consensus-parameter lookup. Both are optional in both directions — an existing coin file parses on a build that understands them, and a file carrying them parses on a build that does not. This release only carries the values: no network-upgrade lookup changes, so the transaction version and consensus branch ID this build signs with are untouched, and a test pins that. **Both members are an extension introduced here, and no published coin configuration declares either of them** — in `GLEECBTC/coins` no coin mentions Ironwood (ARRR is the only coin carrying `consensus_params` at all). **So in this release the two gates below ship dormant: on a default installation ARRR is expected to stop transacting when Ironwood activates on 3 Oct 2026 19:00 UTC, building version-4 transactions that the post-upgrade mempool rejects, and ARRR swaps should be avoided from about 1 Oct onwards** — a swap funded shortly before activation could be left unable to spend *or* refund its HTLC, which is the one case here that risks value rather than merely failing. Supplying `ironwood_activation_time` in any coin configuration the build loads arms both gates with no code change. The values are left to configuration rather than compiled in deliberately: Pirate derives the activation height at runtime and the published timestamp could still move, and a compiled-in date that fired early would refuse every ARRR transaction until a new binary shipped — a worse failure than the one the gates prevent. Code: `mm2src/coins/z_coin.rs`.

### Security

- **The P2P stream multiplexer could be crashed by one malformed frame — GHSA-vxx9-2994-q338.** `yamux` 0.12.1 panics on a crafted inbound Data frame that sets SYN with a body larger than `DEFAULT_CREDIT`: on the first packet of a new inbound stream the stream state is created and a receiver queued *before* the oversized-body check completes, and the cleanup that follows the failed check calls `remove(...).expect("stream not found")`. Remotely reachable over a normal Yamux session with no authentication, and there is no fix on the 0.12 line. We were not exposed, but only by accident: `libp2p-yamux` 0.44 depends on *both* yamux lines and `Config::default()` picks the patched 0.13.10 — while **any** `Config` setter switches the connection back to 0.12, including `set_max_num_streams`, which is not deprecated and looks entirely harmless. Upstream asserted that switch as intended behaviour in its own test. `libp2p-yamux` is now vendored at `vendor-patches/libp2p-yamux-0.44.0/` with the `yamux012` dependency removed outright, so the 0.12 crate is no longer in the dependency graph and the fallback cannot be reached. The 0.12-only knobs (`Config::client`/`server`, `WindowUpdateMode`, `set_window_update_mode`, `set_receive_window_size`, `set_max_buffer_size`) are deleted rather than stubbed, so using one is now a compile error instead of a silent downgrade; `set_max_num_streams` is kept and maps to yamux 0.13 natively. Not a wire change — both speak `/yamux/1.0.0`, and since this workspace never configured the muxer, effective flow-control behaviour is unchanged. Code: `vendor-patches/libp2p-yamux-0.44.0/src/lib.rs`, `Cargo.toml`.

- **WalletConnect JWT verification no longer goes through a vulnerable JWT library — GHSA-h395-gr6q-cpjc.** `relay_rpc` depended on `jsonwebtoken` 8.3.0, which has a type confusion in claim validation: a standard claim (`nbf`, `exp`) supplied with the wrong JSON type is marked `FailedToParse` and then treated identically to `NotPresent`, so an enabled check is silently skipped unless the claim is also in `required_spec_claims`. The advisory has no RustSec id, so `cargo deny check advisories` never saw it. It was not exploitable here twice over — `relay_rpc` used that crate only for `crypto::verify` (a signature check; its claim validation is hand-rolled in the same file), and KDF only ever mints and sends relay tokens, never verifies one — but removing it was cheaper than continuing to depend on that argument holding. The single call site now verifies with `ed25519_dalek::VerifyingKey::verify_strict`, using the public key from `iss` that the signing side already derives and the same base64url-nopad decoder that produced the signature, so signing and verification are symmetric. `ed25519-dalek` was already a direct dependency of `relay_rpc`, so nothing was added, and both `jsonwebtoken` and its `ring` 0.16.20 edge leave the graph. JWT wire format is untouched. Code: `vendor-patches/relay_rpc-0.1.0/src/jwt.rs`, `Cargo.toml`.

- **A single crafted P2P message could crash any node — GHSA-gc42-3jg7-rxr2, GHSA-xqmp-fxgv-xvq5.** The gossipsub implementation in the pinned `KomodoPlatform/rust-libp2p` fork (`k-0.52.12`, gossipsub 0.45.0) took the `backoff` field of an incoming PRUNE control message — an attacker-controlled `u64` — and fed it straight into `Instant` arithmetic without a bound. A PRUNE carrying a near-maximum backoff panicked the swarm state machine, either on receipt (`Instant::now() + time`) or on a later heartbeat (`backoff_time + slack`, surfacing as `overflow when adding duration to instant`). The sender needed no authentication beyond being a mesh peer, so any node that would peer with an attacker could be crashed on demand — seed nodes included. A third overflow in the same path, covered by neither advisory, was found while writing the regression test: the `heartbeat_index + heartbeats(time) + backoff_slack` sum overflows `usize`, which panics in debug builds and on 32-bit targets (armv7 and wasm32 are both shipped). All three are fixed — the peer-supplied value is clamped to 24 hours before it becomes a `Duration` (the protocol default backoff is 60 s), and the additions are checked or saturating. Upstream fixed the first two in gossipsub 0.49.3/0.49.4, which belong to libp2p 0.56; `k-0.52.12` is the newest tag the fork has, so the crate is vendored and patched at `vendor-patches/libp2p-gossipsub-0.45.0/` rather than bumped — the fork's relay-mesh extension is untouched and the 19 `mm2_p2p` mesh tests still pass. Drop the patch when a fork tag carrying the fix appears. Code: `vendor-patches/libp2p-gossipsub-0.45.0/src/backoff.rs`, `.../src/behaviour.rs`, `Cargo.toml`.

### Fixed

- **A shielded wallet that could no longer sync looked like one still catching up — CRD ch.39 R39.8.0au.** ZCoin derived its history-sync state from whether the wallet-database scan had completed, and a failed background pass leaves that flag exactly where an unfinished one does. So a wallet whose sync was broken — unreachable chain tip, every configured lightwalletd failed, or a scan error — reported the same in-progress state as one that was merely slow, and kept reporting it indefinitely. The two call for opposite responses from the operator: wait, or investigate. The failure is now recorded with its message and the height reached, reported through the existing error state that other coins already use, and cleared by the next successful pass so the state tracks the current condition instead of latching on the first failure. It is reported whether or not the initial scan had completed, because a background pass can fail long after activation succeeded. `z_coin_tx_history`'s `sync_status` can therefore now carry the error state; the chapter previously described it as always terminal, which was already inaccurate for a failed pass. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins/z_coin/z_coin_ops.rs`.
- **`kdf --version` exited with status 1.** It printed the version and then went on to parse the flag as a JSON configuration. `kdf --version` (and `kdf version`) now prints the version and exits with status 0. Code: `mm2src/mm2_main/src/mm2.rs`.
- **Swaps on a UTXO coin whose blocks carry version `0x30000000` could not be spent or refunded — CRD ch.37 R37.5.4a, R37.5.6.** That version value is what a chain signalling only BIP9 version bit 28 produces (Bitcoin Core regtest does so while its test deployment is started or locked in), and it is also the KawPoW header family's version value. The header reader chose the 120-byte KawPoW layout from the version alone, so a list of standard 80-byte headers read misaligned and ended early. Median-time-past failed with it, and so did every spend or refund that sets its lock-time from that value: the maker stopped at the taker-payment spend, and both sides fell back to waiting for the refund locktime. Header reads now require the served bytes to be consumed exactly, and fall back to the standard 80-byte layout when the served bytes are exactly 80 per header. A header list that leaves trailing bytes is now rejected rather than silently accepted. Standard headers with this version and a zero nonce also re-encode and hash correctly. KawPoW coins keep their current behaviour. Selecting the header family from the coin configuration (`protocol.chain_variant`) remains deferred work (CRD ch.37 §37.9 D1). Code: `mm2src/kdf_chain/src/header.rs` and the UTXO header read paths.
- **Back-to-back ARRR sends could be rejected for reusing a note — CRD ch.39 R39.8.0ao–R39.8.0at.** In Light mode nothing told the wallet that a note had already been committed to a transaction it had just broadcast. The shielded wallet database only learns a note was spent when the block containing the spend is scanned, and that scan runs on a 30-second timer, so between *mined* and *scanned* the spent note still satisfied every condition of the spendable-note query. The next send re-selected it, and the resulting transaction re-presented the same nullifier and was refused by the network. Nothing in the request explained why. The 1-confirmation wait the send path already performed did not help, because selection depends on the scan, not on the confirmation. A taker swap using ARRR was the likeliest way to hit it — the DEX fee and the taker payment are two shielded sends in quick succession — but any two sends inside the window could collide, including a withdrawal issued during a swap. The send path now records each broadcast transaction in the shielded wallet database immediately after the backend accepts it, which marks the spent nullifiers and takes the notes out of selection, and performs selection, construction, broadcast and recording under a single per-wallet exclusive section so two concurrent sends cannot choose the same notes. A send that is short only because of an in-flight spend releases the section, waits under a bounded budget and re-selects; one that could not be funded even with every note available fails immediately instead. Recording happens only after a successful broadcast, so a refused broadcast leaves every note immediately reusable. The same change corrects a balance inconsistency in that window, where the spent input was still counted as spendable while its change was also reported as pending. Light mode only; in Native mode the full node already accounts for its own mempool spends. **Still to do, deferred:** cover a withdrawal broadcast outside the daemon, which the send path never observes and therefore cannot record (CRD ch.39 D39.8.0e); complete the specified regression coverage — a second send declining the excluded notes, a rejected broadcast leaving them reusable, survival across a restart, the mined-and-scanned transition, expiry lapse, and two concurrent sends — which needs a harness able to drive a shielded wallet database through broadcast, scan and expiry transitions, so only the scanned-transaction predicate is covered today (CRD ch.39 D39.8.0f); and test and fine-tune how long a short send waits for an in-flight spend before giving up, currently 300 seconds, chosen to sit under the swap timeouts above it but not yet measured against real ARRR sync timings. Code: `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins/z_coin/z_coin_wallet_db.rs`, `mm2src/coins/z_coin.rs`.
- **A shielded transaction that was never mined blocked the send path indefinitely.** The send path's wait for its own transaction to confirm passed a millisecond timestamp into a deadline compared in seconds, putting the deadline roughly fifty thousand years out, so the intended ~66-minute cap never fired. A transaction that stayed in the mempool without ever being mined or dropped left the swap or withdrawal waiting forever rather than failing. Code: `mm2src/coins/z_coin/z_coin_ops.rs`.
- **A requested shielded rescan was silently discarded by the next activation — CRD ch.39 R39.8.0an.** Activation skips forward to the recent scan window rather than replay long-dead history, which is right for a new wallet. But a wallet part-way through a caller-requested deep rescan is indistinguishable from a fresh one — empty, anchored far behind the window — until the rescan reaches its first transaction, so the skip fired on the next activation that carried no `sync_params` (which is what a GUI sends on restart) and threw the rescan away. Observed in the wild: a rescan requested at 02:02:11 was wiped 53 seconds later and jumped forward ~156 000 blocks. The case it damages most is restoring a seed whose funds predate the recent window — the rescan that would have found them is discarded and the balance stays at zero. The skip is now confined to a wallet still anchored where it was born (the coin configuration's checkpoint, or Sapling activation when none is declared); a deliberately re-anchored wallet keeps its anchor. Code: `mm2src/coins/z_coin/z_coin_wallet_db.rs`.
- **Incoming ARRR payments from current Pirate wallets were invisible — CRD ch.39 R39.8.0am.** Pirate accepts both the pre- and post-ZIP-212 note plaintext versions at every height, and says so explicitly in its own `plaintext_version_is_valid`. But Pirate has no Canopy upgrade, and librustzcash derives ZIP-212 enforcement solely from Canopy — so with `canopy_activation_height: null` (which is correct for Pirate) enforcement resolved to `Off`, which accepts **only** the `0x01` lead byte. Every note sent by a current Pirate wallet carries `0x02` and was discarded. Nothing errored: a failed trial decryption is indistinguishable from a note that is not ours, so the payment simply never appeared — no balance, no history entry, no warning. Payments sent *from* KDF were unaffected, because KDF built them with the same `Off` policy and so produced `0x01` notes that both sides could read; this is why sending appeared to work perfectly in both directions while receiving from Treasure Chest silently failed. Note decryption now uses parameters derived separately from the coin's consensus parameters, reporting the enforcement state that accepts both versions, applied at every trial-decryption site: the compact-block scanner, mempool detection, full-transaction decryption and outgoing-output recovery. Those parameters are deliberately confined to decryption and never reach transaction construction — the consensus branch ID is the branch of the last active upgrade, so parameters reporting Canopy active would move every signed transaction off the Sapling branch; a test pins that the real parameters still resolve to Sapling and the decryption parameters do not. Upgrading the crates does not help: `sapling-crypto 0.7.0` is the newest release and applies the identical rule, as does `zcash_primitives 0.30.1` — the rule is a correct implementation of *Zcash's* consensus, which Pirate deliberately departs from. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins/z_coin/z_coin_wallet_db.rs`, `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins/z_coin/z_swap_ops.rs`.
- **A shielded (ARRR/ZHTLC) wallet that met an ordinary chain reorg could never sync again — CRD ch.39 R39.8.0al.** A light wallet that syncs to the chain tip records the tip block; a routine one-block reorg then leaves it holding a block the network has dropped. On the next activation the tree state the servers report for that height no longer matched what the wallet stored, and the wallet refused to continue — with no rewind and no rebuild, so every retry repeated the identical comparison and the wallet stayed stuck permanently. It surfaced as `All lightwalletd servers failed: … tree-state hash at height N does not match the shielded wallet DB`, with every reachable server named, which reads like a server outage but is the opposite: the servers agreed with each other and the wallet was the stale party. Confirmed against mainnet — a wallet's stored tip hash was found on the chain as a real block at that height with `isMainChain: false`. The wallet now rewinds past the divergence and re-anchors, rebuilding and rescanning only if that fails or the backend refuses the rewind (which it does for heights it holds no commitment-tree checkpoint for); fetching resumes from the height actually anchored at, so the rewound range is rescanned rather than skipped. No shielded value is at risk in any path — every note is recoverable from the chain with the wallet's viewing key. The divergence is now logged with both block hashes in explorer display order, where before it named neither. Code: `mm2src/coins/z_coin/z_coin_wallet_db.rs`.
- **A shielded (ARRR/ZHTLC) light wallet could anchor itself on a commitment tree of size zero if a lightwalletd server answered with a commitment root instead of a tree — CRD ch.39 R39.8.0ak.** Pirate's lightwalletd fills the `TreeState` tree field via `preferredTreeState(finalState, finalRoot)`, substituting the bare 32-byte commitment **root** whenever the node cannot supply a frontier — and it does so identically in `GetTreeState` and `GetBridgeTreeState`, so there is no alternative RPC to ask instead. The wallet passed that field straight to the tree reader, which reads its three members and returns *without* checking that the buffer is exhausted: a root whose leading bytes happen to be zero parses **successfully** as the empty tree, silently discarding the other 29 bytes, and the wallet anchors on a tree of size 0 — a wrong sync anchor rather than a visible failure. The checkpoint payload is now rejected if it is exactly a root's length, if bytes remain after a complete tree, or if it is empty above Sapling activation (empty at or below activation is the empty tree and stays valid), in every case without touching persisted or in-memory chain state. The `saplingFrontier` and `ironwoodTree` fields that Pirate lightwalletd v1.0.0.0 added are now decoded, and a frontier disagreeing with the tree field is logged while the tree field stays authoritative. Not observed in production — every reachable server returned a well-formed tree throughout the 2026-09-18 live verification — this closes the silent-acceptance path ahead of the Ironwood upgrade. Code: `mm2src/coins/z_coin/z_coin_wallet_db.rs`, `mm2src/coins/z_coin/service.proto`.
- **`z_coin_tx_history` reported ARRR transaction IDs byte-reversed, so they matched no block explorer and disagreed with the node's own `send_raw_transaction` answer — CRD ch.39 R39.8.7a.** The handler hex-encoded the shielded wallet database's `transactions.txid` column directly, but `zcash_client_sqlite` stores that column in internal little-endian order; every other KDF coin's history reverses it to big-endian display order first, and so does this coin's own `withdraw` response. The two orders are byte reversals of each other, so the wrong one is not malformed — it is a valid-looking 64-character hash that resolves nowhere. Reproduced end-to-end on ARRR mainnet: a received payment that the sending wallet and both Pirate explorers know as `339740530aba…` was reported by history as `a4d43b3b…`, and the sweep that spent it came back from `send_raw_transaction` as `23bb6cda…` (which the explorer resolves) while history rendered the same transaction as `0e377444…` (which it does not) — so a caller could neither look a transaction up nor correlate a send with its own history entry. Now reversed at the single call site. The existing page-ordering test could not have caught this: its fixture transaction IDs were repeated single bytes, which read identically in both orders. The fixture now uses the two real mainnet IDs above and the byte order is asserted in both directions. Code: `mm2src/coins/z_coin/z_coin_wallet_db.rs`.
- **A Siacoin swap waiting for its HTLC spend no longer hides an unreachable node.** `wait_for_tx_spend` polls the mempool and then the chain for the transaction spending a swap's HTLC output. The chain-side lookup logged a failed query and carried on; the mempool side discarded the error and substituted an empty transaction list, which is indistinguishable from a genuinely empty mempool. A node that was persistently unreachable therefore presented exactly like an HTLC nobody had spent — the swap ran to its timeout with nothing in the log to explain why. The query is still retried either way (a failure means "we do not know yet", not "not spent", and aborting a wait on one blip would be worse); it is now logged at the same level as the chain-side lookup beside it. Code: `mm2src/coins/siacoin/siacoin_market_ops.rs`.
- **A Siacoin withdrawal reported the wrong record type and no record id, and could have reported success with an unusable transaction — CRD ch.20 R-W6/R-W9/R-W10.** Three defects on the same response, all in `sia_withdraw.rs`. `transaction_type` was left at the shared enumeration's default standard-transfer member, disagreeing with the Sia v2-transaction value this project's own history path already reports (ch.53 R53.5.10) for the very same transaction — one transaction described two ways depending on which subsystem a caller asked. `internal_id` was left empty, so a withdrawal record carried no primary key and could not be joined to the history record that later appears for it; it now carries the raw id bytes whose hex form the same record reports as `tx_hash`, matching how the history path keys its own records (ch.53 R53.5.2). And the failure branch of serialising the signed transaction substituted an empty default, which would have returned a *successful* withdrawal carrying a `tx_hex` no broadcast path can use — a placeholder value in a funds-moving path; it now aborts the withdrawal with a structured error. Code: `mm2src/coins/siacoin/sia_withdraw.rs`.
- **A V2 taker swap's confirmation-timeout abort could never actually refund — CRD ch.52 D8.** If the maker's spend of the taker's own successful claim on the maker's payment failed to confirm within its wait window, `MakerPaymentSpent::on_changed` transitioned to `TakerPaymentRefundRequired` carrying empty bytes in place of the taker's own payment transaction, which the resulting refund attempt could not construct anything from — the swap was left stuck rather than refunded. Root cause was state-field scope, not a missing lookup: the bytes existed two states earlier (`TakerPaymentSent`) but were dropped by the very next state (`TakerPaymentSpent`) and never carried further. `taker_payment` is now threaded through both states as an additive, defaulted field, and the abort path passes the real bytes. Reachable on native builds by any delayed, dropped, or reorganized confirmation of the maker's own payment spend — not exotic. Code: `mm2src/mm2_main/src/lp_swap/taker_swap_v2.rs`.
- **A V2 swap's `net_config_or_panic` calls could crash a swap's kickstart thread on an unrecognised netid instead of aborting cleanly.** Seven call sites across both V2 swap machines used the panicking netid lookup from inside an already-fallible `on_changed`; all seven now use the existing non-panicking lookup and abort the swap with a typed error on `None`, matching the sibling error-handling already present at each site. The panic was contained to the one swap's dedicated kickstart thread, not the whole process, but a clean abort is strictly better. Code: `mm2src/mm2_main/src/lp_swap/{taker_swap_v2,maker_swap_v2}.rs`.
- **A V2 atomic swap now reserves the fee it needs to collect the payment it is owed — CRD ch.52 R64, closing ch.52 D2.** A V1 swap holds back the network fee for spending the counterparty's payment, so that a trade started a second later cannot spend it; a V2 swap held back nothing. On a coin that pays that fee from the account balance the shortfall is real: the concurrent trade wins, and the V2 swap is left with a payment on chain it cannot claim until the balance is topped up. Both V2 roles now reserve it — the maker on the taker coin, the taker on the maker coin — for as long as the incoming payment is unspent, and a swap resumed after a restart recovers the reservation from its persisted log rather than losing it. The amount is derived to equal what V1 already reserves, because a node runs both protocols and computes one answer over the union: it is the spend fee when the coin pays it from the account balance (EVM coins — the spend gas, billed to the platform coin for an ERC-20 leg) and **zero** when the coin pays it out of the payment being claimed (UTXO coins, where the miner is paid from the HTLC output itself). **What changes for a user:** `max_taker_vol` and `get_locked_amount` return a smaller number for an EVM coin while a V2 swap against it is live, by that swap's spend gas — a trade sized at the old maximum is now refused, and the balance it would have spent is the balance the live swap needs. No V1 answer changes, and no answer changes for a UTXO coin. Each role's initialisation event gains one additional persisted field carrying the resolved amount; it is defaulted, so a swap persisted before this change resumes without a headroom rather than failing to parse. Code: `mm2src/mm2_main/src/lp_swap.rs`, `mm2src/mm2_main/src/lp_swap/maker_swap_v2.rs`, `mm2src/mm2_main/src/lp_swap/taker_swap_v2.rs`.
- **A V2 swap's own payment now reserves its sending fee in the right coin — CRD ch.52 R58, closing ch.52 V5.** The reservation for a role's own outgoing payment nested its sending fee inside the volume entry as a `TradeFee` naming the trading coin, unconditionally. That is correct when the fee is actually paid in the trading coin, but a token coin's sender fee is gas billed to its *platform* coin, and the per-coin-ticker ledger checks a nested fee's coin only against the bucket already being queried — so for a token trading coin, the fee was invisible in every bucket: not counted against the token (correctly, since the fee isn't paid in the token), and not counted against the platform coin either (incorrectly — that balance is what the fee actually costs, and nothing was held back for it). A token maker or taker could start a trade with adequate token balance and no reserved gas, and a concurrent trade could spend that gas out from under it. The send fee is now its own ledger entry, filed under whichever coin the trade-fee estimate actually names, alongside — not nested inside — the volume entry; the two share a bucket when the fee's coin agrees with the trading coin and land in separate buckets when it does not, and both are created and released together. **What changes for a user:** `max_taker_vol` and `get_locked_amount` now correctly reserve a token V2 swap's send-side gas against the platform coin instead of silently reserving nothing for it; no change for a coin whose own asset pays its own fees (UTXO, ETH itself). Code: `mm2src/mm2_main/src/lp_swap.rs`, `mm2src/mm2_main/src/lp_swap/maker_swap_v2.rs`, `mm2src/mm2_main/src/lp_swap/taker_swap_v2.rs`.
- **Sia (SC) can now take part in V1 atomic swaps — the swap negotiated a key Sia could never sign with. CRD ch.51 R62–R64, ch.20 R-S9.** The legacy swap machines initialised both per-coin HTLC keypairs to the node's secp256k1 key and put that key on the negotiation wire for *both* coins, without ever asking either coin what key it uses. Sia signs with ed25519 and derives its swap keys internally, so a swap with SC on either side negotiated one key and built its spend policies from another; the two could never agree, and no SC swap could complete. The coin layer now answers two operations for every chain — deriving this node's HTLC public key, and validating the counterparty's — both fixed at the 33 bytes the deployed wire format has always required (R63). A secp256k1 chain is unaffected: it returns the same key the machines resolved before, and its validator additionally rejects anything that is not a well-formed secp256k1 point. Sia returns its own ed25519 key in the dictated 33-byte form (R64): the 32 native bytes in the leading positions, final byte zero, read back from the front — the form Sia's own swap operations already consume. A swap persisted before the per-coin keys were recorded still negotiates the key it started with. The V2 swap path is untouched (ch.52 R69). Code: `mm2src/coins/lp_coins_traits.rs`, `mm2src/coins/lp_coins_types.rs`, `mm2src/coins/siacoin/siacoin_swap_ops.rs`, `mm2src/mm2_main/src/lp_swap.rs`, `mm2src/mm2_main/src/lp_swap/maker_swap.rs`, `mm2src/mm2_main/src/lp_swap/taker_swap.rs`.
- **A Sia-, Lightning-, or Tendermint-paired legacy swap now negotiates a secret hash the payment's own HTLC can actually be satisfied with — CRD ch.51 R71/R72/R72A.** The legacy maker and taker state machines computed `secret_hash` with the 20-byte `RIPEMD160(SHA-256(secret))` default unconditionally, for every coin pair — even though a swap against Sia's native spend-policy HTLC, Lightning, or a Tendermint-family IBC-HTLC can only ever be satisfied by the 32-byte `SHA-256(secret)` alternate (§20.6, ch.18, ch.41). A coin-pair-selection primitive matching this already existed elsewhere in the crypto surface; the swap machines never called it. Two more gaps sat downstream: `secret_hash` was stored as a fixed-20-byte type on both sides, so even a correctly-selected 32-byte value would have been truncated on receipt; and the UTXO coin layer's own HTLC construction, validation, and spend-secret extraction all embedded `secret_hash`'s raw bytes directly into UTXO's fixed-20-byte `OP_HASH160` comparison, which a 32-byte value can never satisfy on-chain — invisible at negotiation and validation time (both only reconstruct and structurally compare), surfacing only when an actual spend was attempted, at which point the payment remained spendable only via the refund timelock. All three now close together: the maker machine selects the algorithm once per swap and persists the result as a variable-length `BytesJson` (the taker adopts whatever the maker negotiated, so fixing the maker side fixes both roles), and UTXO's `htlc_hash160_comparator` performs the width-aware derivation R72A dictates — a 20-byte hash used unchanged, a 32-byte one with `RIPEMD160` applied to it once — at every UTXO construction, validation-reconstruction, and extraction call site. **What changes for a user:** a legacy V1 swap pairing SC (or a Tendermint-family coin) against a UTXO coin can now actually complete on the honest-spend path; before this fix it negotiated cleanly and failed silently at spend time. A swap already persisted with the pre-fix 20-byte value is not retroactively repaired — see the `recover_funds_of_swap` entry below for what that now looks like. Code: `mm2src/mm2_main/src/lp_swap.rs`, `mm2src/mm2_main/src/lp_swap/maker_swap.rs`, `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`.
- **Siacoin HTLC payment validation now checks the real counterparty's key, not sometimes its own — CRD ch.51 R63.** `ValidatePaymentInput.taker_pub`/`.maker_pub` always carry the real taker's and real maker's keys, never "mine" vs "other" — both `validate_maker_payment` and `validate_taker_payment` share the struct and populate it that way, and the caller alone knows which. Sia's validator read a fixed field regardless of which operation called it, so one of the two roles validated an observed payment's spend policy against its *own* key instead of the counterparty's — a spend-policy address that can never match what the real counterparty actually paid to, silently rejecting an honest payment as invalid. The caller now says explicitly which side is being validated. Code: `mm2src/coins/siacoin/siacoin_types.rs`, `mm2src/coins/siacoin/siacoin_swap_ops.rs`.
- **A Siacoin swap spend no longer fails on a walletd indexing blip.** `send_maker_spends_taker_payment`/`send_taker_spends_maker_payment` look a just-confirmed payment's own event up by txid immediately after `wait_for_confirmations` polled that same event as sufficiently confirmed through the same endpoint — but the wait step tolerates a transient walletd error by retrying, and this lookup did not, so a walletd blip invisible to the wait step failed the whole swap outright (observed live: a 404 "event not found" roughly 200 ms after the wait step's last successful poll of the same txid). The lookup now retries with the same tolerance as the wait step. Code: `mm2src/coins/siacoin/siacoin_swap_ops.rs`.
- **An HD-wallet-activated UTXO account can now spend/refund its own swap HTLC and see its own transaction history — CRD ch.51.** `send_maker_spends_taker_payment`, `send_taker_spends_maker_payment`, `send_taker_refunds_payment`, `send_maker_refunds_payment`, and `tx_details_by_hash` all resolved "my address" via `derivation_method.iguana_or_err()`, which only ever succeeds for a single-address (Iguana) account and returns `IguanaPrivKeyUnavailable` for an HD-wallet one. Any of these operations, executed by the HD-activated party, therefore failed outright: a live swap stalled waiting for a taker-payment spend that could never be sent, and an HD wallet's own UTXO transaction history stayed empty regardless of whether the coin otherwise had correct data. Fixed by deriving the address from the swap's own already-correct signing key (`key_pair.public()`, resolved via `get_htlc_key_pair` at negotiation time) instead of asking `derivation_method` at all — one fix that is correct for both Iguana and HD-wallet accounts uniformly, with no branch needed. Confirmed against real swaps: SC/RIN and RIN/LTC pairs, both maker/taker roles, both an Iguana-activated and an HD-wallet-activated counterparty. Code: `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`, `mm2src/coins/utxo/utxo_common/utxo_common_history.rs`.
- **`recover_funds_of_swap` and `import_swaps` no longer risk crashing the whole node.** Both RPCs ran on `futures-cpupool` (unmaintained since 2018), whose own task-teardown path was found — via a Windows Application Verifier page-heap catch, later reproduced as a clean access violation with a full-memory crash dump and matching debug symbols — to corrupt heap memory while dropping a completed task's internal `MySender` wrapper, taking the entire process down with it (surfacing beforehand only as an opaque `STATUS_HEAP_CORRUPTION`, on whichever thread happened to touch the corrupted region next — not necessarily the one that ran either RPC). Neither RPC's own logic was at fault. Both now run via `tokio::task::spawn_blocking` (already used elsewhere in this codebase for exactly this "off the async reactor" need), and the crate is no longer a dependency of this workspace. Code: `mm2src/mm2_main/src/rpc/dispatcher/dispatcher_legacy.rs`, `mm2src/common/wio.rs`.
- **`recover_funds_of_swap` no longer risks a blind refund for Siacoin, and its hard-stop failures now say why — CRD ch.20 D3/R-S8.** Sia's `search_for_swap_tx_spend_my`/`_other` were stubs returning `Ok(None)` unconditionally — indistinguishable, to `recover_funds`, from "checked the chain, confirmed not spent" — and `recover_funds` treats that answer as license to attempt a refund. A Siacoin payment the counterparty had, in fact, already spent would have been silently skipped straight to a refund attempt of it, never actually checked. Both now return an explicit "not yet implemented" error instead, surfaced as a clean RPC failure rather than a wrong branch taken silently. Separately, two of `recover_funds`' correct, structural refusals had messages that said nothing a caller could act on: a UTXO script-pubkey mismatch printed a full Rust `Debug` dump of the whole deserialized transaction, and a Siacoin secret-hash-width mismatch surfaced sia-rust's generic "invalid slice length" with no indication that the swap had simply negotiated the legacy 20-byte hash before Sia pairs required 32 (ch.51 R71/R72, closed above). Both now state plainly what went wrong and why the swap cannot be recovered automatically. Code: `mm2src/coins/siacoin/siacoin_swap_ops.rs`, `mm2src/coins/siacoin/error.rs`, `mm2src/coins/siacoin/siacoin_types.rs`, `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`.
- **Paying with ARRR/ZHTLC in light mode now works — the swap payment transaction could never be built.** A ZHTLC swap payment is a transparent P2SH output plus an `OP_RETURN` carrying the redeem script, funded from shielded notes. The light-mode transaction builder was written for withdrawals, which only ever produce shielded outputs, and rejected any transparent output outright. Every other caller was left to hit that guard, and the only one that passes transparent outputs is the HTLC builder used by both `send_maker_payment` and `send_taker_payment` — so a ZHTLC coin could never be the coin you pay, as maker or as taker. The dex fee is a shielded output and succeeded, which is why an affected swap got as far as `TakerFeeSent` and then died at `TakerPaymentTransactionFailed` with `UnsupportedLightWalletOutput`, after the taker had already paid the fee. Receiving was never affected: validating and spending an HTLC builds its own transaction and does not go through this path. The light-mode builder now emits transparent outputs exactly as the native path does — counted into the note-selection target so enough notes are selected to cover them, and appended after the change output so the transparent output ordering the spend path relies on (`z_p2sh_spend` spends transparent output 0) is preserved. Code: `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins/z_coin/z_coin_errors.rs`.
- **Incoming ARRR/ZHTLC payments are now visible before they are mined — CRD ch.39 §39.8.0.6, R39.8.0af–aj.** Light mode reported a zero non-spendable balance unconditionally, so a shielded payment sent to the wallet did not exist as far as the UI was concerned until its block was mined *and* locally scanned. The background sync task now polls lightwalletd's `GetMempoolTx` stream after each wallet scan, trial-decrypts every compact output with the wallet's incoming viewing key, and reports the recovered value as pending. Ordering is deliberate: polling the mempool after the scan means a transaction mined between the two reads is already excluded, so the same value can never be counted from both the wallet database and the pending set. The set is rebuilt wholesale each poll, so mined, dropped, replaced, and expired transactions all fall out without bespoke invalidation, and repeated observation contributes a value at most once (keyed by transaction id and output index). Pending amounts appear **only** in the `unspendable` balance field — every swap path reads spendable balance, so an unmined note can never size a trade, fund a swap, or be selected as a transaction input. A backend that is unreachable or returns an unusable response degrades the pending view to zero rather than failing the balance query. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins/z_coin/z_coin_wallet_db.rs`.
- **NFT activation now speaks the deployed indexer's wire contract — CRD ch.19 §19.5.1, R6a/R9a.** The subsystem previously used a REST shape of its own devising (`{base}/<chain>/<owner>/transfers?from_block=<n>`) against the base URL SDK/GUI callers supply. That URL addresses a deployed EVM NFT indexer, which serves a different contract, so every crawl request resolved to a path the service does not route and returned `404` — surfaced to the user as `CrawlFailed` and an activation failure. The bundled provider now speaks the dictated contract: `{base}/api/v2/<owner>/nft` and `.../nft/transfers` with `chain` and `format=decimal` query parameters, cursor pagination followed to completion, a `result`/`cursor` envelope, and `from_block` floored at `1` because the API treats `0` as absent. Block heights, timestamps and token ids are accepted as the decimal **strings** the contract sends (JSON numbers still decode, so persisted payloads and GUI clients are unaffected), and `confirmations` — which the contract does not carry — now defaults instead of failing the page. The contract has no latest-block operation, so a chain yielding no transfers leaves its scan bookmark unadvanced rather than moving it to a provider-reported head, which could otherwise skip transfers permanently. Code: `mm2src/coins/nft/providers/crawler.rs`, `mm2src/coins/nft/model/`, `mm2src/coins/nft/serde_helpers.rs`.
- **NFT activation no longer crawls transfer history — CRD ch.19 R9a.** Activation ran a full transfer-history crawl, which multiplied provider requests by history size and made any single one of them activation-fatal, while contributing nothing the `nfts` response needs. It now performs exactly one operation — the owned-inventory fetch, paged to completion — and persists that snapshot before returning. The transfer walk, per-token detail lookups, metadata back-fill, and bookmark advance all remain behind `update_nft`. Code: `mm2src/coins/nft/activation.rs`.

- **Light-mode shielded (ARRR/ZHTLC) balances now update while the wallet runs — CRD ch.39 §39.8.0.5.** The modern compact-block downloader and wallet scanner previously ran *only* during activation, so a Light-mode wallet's shielded balance stayed frozen at the activation height for the rest of the session: an incoming ARRR payment became visible only after activating the coin again (in practice, restarting the wallet). A coin-lifetime background task is now spawned at the end of a Light-mode activation and tops the wallet database up on a bounded poll period (R39.8.0ab), resuming from local sync state (`requested_start_height: None` + `skip_sync_params: true`) so it never re-anchors the wallet (R39.8.0aa). It holds weak references only, so it stops when the coin is disabled (R39.8.0ac). Native mode was unaffected. Expected latency for an incoming shielded payment is now one block plus one poll period. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins_activation/src/z_coin_activation.rs`.
- **Shielded wallet-database rebuilds no longer race an in-flight scan — CRD ch.39 R39.8.0ad.** Disabling a shielded coin removes it from the registry, but a background sync pass already running holds its own reference and continues to the end of that pass; a re-activation started in that window could rename the wallet and compact-block databases out from under the running scan. SQLite's locking prevents corruption between concurrent statements but not against a rename, so the scan and rebuild entry points now take a shared lock scoped to the wallet database's on-disk identity rather than the ticker. Only synchronous sections are guarded, so it is never held across an await. Code: `mm2src/coins/z_coin/z_coin_wallet_db.rs`.
- **A coin with no usable Electrum server now says so immediately.** When every configured Electrum server was unusable — none configured at all, or all of them dropped because a native build cannot use `ws`/`wss` servers — activation spent five seconds attempting connections and then reported failing to connect to an empty candidate list, which gave no hint that the list was empty or why. That case is now detected before any connection attempt and reported as `NoUsableElectrumServers`, naming the ticker and both possible causes. Behaviour is unchanged whenever at least one usable server exists. Code: `mm2src/coins/utxo/utxo_builder/utxo_coin_builder.rs`.
- **The `ws`/`wss` Electrum rejection message no longer implies missing work.** Native builds cannot use `ws`/`wss` servers, and WASM builds cannot use `TCP`/`SSL` ones — a deliberate platform split, not a pending feature. The native-side message said the protocols were "not supported yet", which read as a TODO and prompted repeated investigation; it now states the platform boundary directly. Message wording only, no behaviour change. Code: `mm2src/coins/utxo/rpc_clients/electrum_rpc_client.rs`.

- **EVM swaps now explain a missing gas balance instead of leaking the provider error.** Attempting to trade an ERC-20/BEP-20/PLG-20 token with no platform coin (BNB/ETH/MATIC/…) to pay gas made `estimate_gas` revert, and the raw node error surfaced verbatim — e.g. BSC's `error code -32000: gas required exceeds allowance (0)` — with no indication that the actual problem was an empty gas balance. `get_fee_to_send_taker_fee` now checks the platform-coin balance when gas estimation fails and reports the shortfall against the *platform* ticker, which existing callers map onto `NotSufficientBaseCoinBalance` ("Not enough base coin BNB balance for swap: available …, required at least …"). Because that variant is classified as an insufficient-balance condition, `max_taker_vol` also degrades to a tradable volume of `0` instead of failing the whole RPC. The existing sent-coin check runs first and is unchanged, and a failure to read the platform balance never masks the original error. Code: `mm2src/coins/eth/eth_mm_coin.rs`, `mm2src/coins/eth/eth_types.rs`.
- **Superseded (issue #11): KMD direct-burn DEX-fee split corrected; the
  small-KMD dust exemption below is withdrawn.** The entry below ("Small KMD
  direct-burn DEX fees retain the legacy wire shape") described a flat 75/25
  split above dust plus a builder dust exemption for the resulting
  under-dust fee output. Public-network observation of a `v2.6.0-beta` node
  contradicts that: for a 0.01 KMD taker it emits 1,000 base units to the
  fee address and 158 to `OP_RETURN`, not 868 + 289. The DEX-fee split is now
  computed from three exact-rational ranges on the dust-floored total: at or
  below dust, a single fee output for exactly dust; above dust with the 75%
  share at or above dust, the 75/25 split; otherwise a fee output of exactly
  dust and an OP_RETURN burn of the remainder. Because the fee-collection
  leg is now never below dust, the generic UTXO builder needs no per-output
  or per-descriptor dust exemption for this descriptor any more — the
  under-dust exemption plumbing (builder option, taker-fee send variant, and
  selector) has been removed entirely. Code: `mm2src/coins/lp_coins_types.rs`,
  `mm2src/coins/utxo.rs`, `mm2src/coins/utxo/utxo_common/`.
- **KMD taker no longer over-claims Active User Reward (interest) on
  post-KIP-0001 UTXOs (issue #11).** The reward computation was missing the
  KIP-0001 500x reduction that took effect at the dPoW Season-7 hard fork
  (height 3,484,958). A KMD spend whose inputs include an eligible UTXO
  confirmed at or after that height now claims the correct, reduced reward
  instead of a figure 500x too large, which the network previously rejected
  with `bad-txns-in-belowout`. Reported and validated on the same
  transaction/history/`kmd_rewards_info` surfaces. Code: `mm2src/coins/utxo.rs`.
- **No-fee waiver for the network burn/waiver-key taker (issue #11).** A
  taker whose taker-coin swap public key equals the active network's
  burn/waiver key now pays no DEX fee, on both netid 8762 and netid 6133 and
  on the legacy swap protocol, independently of whether the network's burn
  split is enabled. Previously the waiver only applied while an (inactive,
  on both production networks) general burn-account path was active, so it
  never took effect in production. Netid 8762's burn key
  (`0369aa…3153`) is no longer empty. Code: `mm2src/coins/lp_coins_types.rs`,
  `mm2src/mm2_net_config/`, `mm2src/mm2_main/src/lp_swap/maker_swap.rs`,
  `mm2src/mm2_main/src/lp_swap/taker_swap.rs`.
- **Background retry and diagnostic noise hardening.** Persistent QRC20 history
  failures now back off from 10 seconds to a bounded five-minute retry interval
  and reset after recovery. Native Electrum reconnect loops retain the first or
  changed endpoint error at `ERROR` while identical retries move to `DEBUG`;
  redundant rustls handshake/alert internals are omitted from the application
  log while the endpoint-specific KDF error remains. Optional
  `eth_feeHistory`, late request-response completions, routine peer exchange,
  and duplicate peer-exchange dial diagnostics no longer appear as operator
  errors. Code: `mm2src/coins/`, `mm2src/common/`, `mm2src/mm2_p2p/`.

### Added

- **Siacoin (SC) multi-account HD wallets — CRD ch.20 D1.** Sia previously supported only a single, fixed activation address; `SiaHDWallet`/`SiaHDAccount` now implement the workspace's standard HD-wallet trait surface, deriving under SLIP-44 coin type 1991 via SLIP-10 ed25519 (the derivation SLIP-44 itself dictates for ed25519 coins — architecturally different from BIP32 secp256k1's extended-public-key derivation, so each account carries its own signing key rather than an xpub). `SiaCoinGeneric` now carries a `derivation_method` field, `SiaCoinBuilder` constructs the HD-wallet variant for a `GlobalHDAccount` key policy, and the standard HD RPCs (`get_new_address`, `account_balance`, `task::account_balance`, `task::create_new_account`) are wired for Sia the same way they are for UTXO/QTUM/ETH. Account 0's external address 0 is proven identical to the pre-existing single-address activation path for the same seed, so the two mechanisms cannot silently disagree. Activation itself still only ever produces account 0 (per ch.46 R46.1.3); growing further accounts happens through the RPC surface, not activation. `task::create_new_account`'s xpub-extractor requirement was routed around with a Sia-specific software-only path, since SLIP-10 ed25519 has no hardware-wallet public-key-only derivation to offer it. Code: `mm2src/coins/siacoin/{sia_hd_wallet,siacoin_helpers,mod}.rs`, `mm2src/coins_activation/src/sia_activation.rs`.
- **Siacoin (SC) message signing and verification — CRD ch.20 D4.** `sign_message`/`verify_message` were unimplemented for Sia; both now use the coin's ed25519 keypair. `verify_message`'s third parameter is a Sia public key, not a wallet address — a plain ed25519 signature cannot be verified against an address, since an address is a one-way hash and not invertible to a pubkey — matching how this crate's other ed25519 coin (Solana) already resolves the identical constraint. Code: `mm2src/coins/siacoin/siacoin_market_ops.rs`.
- **Siacoin (SC) withdraw fees are now size-aware instead of a flat 10 SC — CRD ch.20 D6.** The fee is now estimated from a probe transaction's real serialized weight (`V2TransactionBuilder::weight()`) times walletd's live `/api/txpool/fee` rate, recomputed as each candidate input is added — a single-input withdrawal and one needing several inputs no longer pay the same fee. Code: `mm2src/coins/siacoin/sia_withdraw.rs`.
- **Siacoin (SC) is now watcher-eligible for V1 swaps — CRD ch.20 D7.** Four of `WatcherOps`'s five non-trivial methods are implemented for real: fee validation (partial — the trait's input shape carries no swap uuid or amount, so only existence/destination/height can be checked), taker-payment validation, taker-payment-refund preimage construction, and swap-spend search. The fifth, `create_maker_payment_spend_preimage`, is deliberately left on the trait's own graceful default rather than a stub: the trait never supplies the secret this method would need, and a hard stub would panic a live swap path instead of degrading gracefully like every other coin in this crate already does for the same method. Code: `mm2src/coins/siacoin/siacoin_mm_coin.rs`.
- **Siacoin (SC) raw-transaction fetch now works — CRD ch.20 D8.** `get_raw_transaction` returned `NotImplemented` unconditionally; it now fetches a transaction by id, confirmed or in the mempool, the same two-path lookup D7's fee validation already established for this coin. Code: `mm2src/coins/siacoin/siacoin_mm_coin.rs`.
- **Siacoin (SC) withdraw responses now carry the transaction in both encodings, and `send_raw_transaction` accepts either — CRD ch.20 R-W6–R-W8.** Sia is the coin family whose native serialisation of a signed transaction is JSON text rather than a binary encoding. A completed SC withdrawal keeps `tx_hex` — mandatory and authoritative per ch.49 R49.25, and the sole carrier the platform's published API reference documents — now carrying the hex of that JSON, and additionally emits the same transaction unencoded in a new top-level `tx_json` object. `send_raw_transaction` accepts a transaction in either carrier, with `tx_hex` winning when both are present, so a caller can feed back whichever half it kept (or echo a whole transaction-details object) and always broadcast the same transaction. A present-but-malformed carrier is an error rather than a reason to fall through to the other one. `tx_json` is absent for every other coin family and its absence is not an error, so no existing integration observes a change. Code: `mm2src/coins/lp_coins_types.rs`, `mm2src/coins/lp_coins_ops.rs`, `mm2src/coins/siacoin/sia_withdraw.rs`.
- **Siacoin (SC) `get_raw_transaction` now carries both encodings too — CRD ch.20 R-W12.** The method returned only `tx_hex`, leaving this project carrying one method that offers the JSON carrier and another that does not, for the same coin and the same transaction. It now returns `tx_json` alongside `tx_hex` on exactly the terms the withdraw path uses, derived from the same bytes so the two can never describe different transactions. It also stops routing through the coin-generic transaction trait's hex-serialisation method, which has no error channel and substitutes an empty vector on failure — a serialisation failure is now a structured error instead of a success response carrying no transaction (the remaining instance of that pattern is recorded as ch.20 §20.10 D9). `tx_json` is absent for every other coin family, so no existing integration observes a change. Code: `mm2src/coins/lp_coins_types.rs`, `mm2src/coins/siacoin/siacoin_mm_coin.rs`.
- **Siacoin (SC) transaction history — CRD ch.53, closing ch.20 §20.10 D2.** The wallet could not show a single SC transaction. Sia history was never implemented: the coin's history loop was inert, nothing was ever stored, and the mmrpc-2.0 `my_tx_history` rejected SC outright with `NotSupportedFor`. SC history is now a projection of walletd's per-address event log. A background tracking pass retrieves the activated address's complete event set (paging the walletd address-events endpoint until it returns a short page), maps each represented event onto the shared transaction-details record, and caches the result in the coin-generic runtime history store — file-backed natively, IndexedDB under wasm — which both `my_tx_history` tiers already read. Four event kinds are represented: v1 and v2 transactions, and miner and foundation payouts; siafund claims and both contract-resolution kinds are skipped silently. Amounts are reported in whole SC at the dictated `1 SC = 10^24 hastings` ratio. A v1 event may create more value than it consumes (consensus-funded creation), so the fee saturates at zero rather than underflowing. Payouts carry an empty `from`, a zero `spent_by_me`, and no fee. Records are keyed by the walletd event identifier and the pass is insert-only, so repeated passes over an unchanged event set neither duplicate nor rewrite a record; an event that disappears from the fetched set is pruned. The confirmation count is derived from the chain tip at request time rather than stored, so a record never carries a stale count. Tracking is enabled by the existing `tx_history` activation flag (default `false`, ch.46 §46.1.2); a coin activated without it answers history requests successfully with an empty list and the not-enabled sync status. SC is deliberately **not** classified into the SQL-indexed history storage, so no per-coin history tables are created for it and no migration is triggered (ch.53 D53.1). Three Sia values are added to the shared `transaction_type` wire enumeration (`SiaV1Transaction`, `SiaV2Transaction`, `SiaMinerPayout`, the last shared by both payout kinds); no existing member changes wire form, default, or meaning. Two defects kept this from working end to end after it landed: the coin-generic history file cache's hand-rolled `TxFeeDetails` deserializer had no case for `Sia` (or `Tendermint`), so a single SC entry failed to deserialize the whole per-coin history file and was treated as corruption — wiped on *every* read, including the read the history RPC does directly against the file, so SC history could never survive being read back even once correctly fetched; and the generic history-loop spawn point (`lp_spawn_tx_history`) ran the loop on a raw OS thread with no Tokio runtime context, so Sia's own HTTP-based history loop never actually made progress. Both are fixed. Code: `mm2src/coins/siacoin/siacoin_history.rs`, `mm2src/coins/siacoin/siacoin_mm_coin.rs`, `mm2src/coins/siacoin/error.rs`, `mm2src/coins/siacoin/mod.rs`, `mm2src/coins/my_tx_history_v2.rs`, `mm2src/coins/lp_coins_types.rs`, `mm2src/coins/lp_coins_ops.rs`.
- **HD wallets now get push notifications too.** The subscription target was resolved through the single-address accessor, which is unavailable under HD derivation — so an HD wallet subscribed to nothing and fell back to polling for its entire session, silently. Every address the wallet currently knows about, across all activated accounts and both BIP44 chains, is now subscribed; the set is bounded by the accounts' own known-address counts, i.e. exactly the addresses the balance already sums, so nothing is derived beyond what the wallet already tracks. A partial failure is non-fatal: whatever subscribes is an improvement and the poll still covers the rest. Applies to UTXO and QTUM; BCH does not implement the HD wallet traits and keeps the single-address path. Code: `mm2src/coins/utxo/utxo_common/utxo_common_hd.rs`, `mm2src/mm2_main/src/rpc/streaming_activations/balance.rs`.
- **QRC20 token balances now update on notification, via per-token contract-event subscriptions.** A QRC20 balance lives in contract storage rather than in the holder's UTXO set, so a token transfer need not change the QTUM address's script hash and cannot be observed through a script-hash subscription — tokens would have kept polling while their platform coin got push. Tokens now subscribe `blockchain.contract.event.subscribe` per (address, contract, Transfer topic), and notifications are routed on a composite key so a token and its platform coin cannot wake or displace each other. Code: `mm2src/coins/qrc20/rpc_clients.rs`, `mm2src/coins/utxo/rpc_clients/electrum_rpc_client.rs`, `mm2src/mm2_main/src/rpc/streaming_activations/balance.rs`.
- **Transaction history also refreshes on Electrum notification.** The UTXO history loop re-reads balance every 30 s and reloads history only when it changed, so a confirmed transaction could sit unnoticed for most of an interval. It now waits for that interval *or* a script-hash notification, whichever comes first. The notification decides only *when* to look: the balance is still re-read authoritatively at the top of the loop, so nothing is inferred from the notification itself. Electrum-backed Iguana-derivation coins only; native-RPC coins are unchanged. Code: `mm2src/coins/utxo/utxo_common/utxo_common_history.rs`.
- **Script-hash watchers no longer displace one another.** The registry stored a single watcher per script hash, so a second consumer of the same address — the history loop alongside the balance streamer — would have silently replaced the first, leaving that subsystem permanently unwoken. Watchers now accumulate per hash and every one is notified; departed consumers are pruned by receiver liveness, so one consumer leaving cannot silence the others. Code: `mm2src/coins/utxo/rpc_clients/electrum_rpc_client.rs`.
- **Electrum-backed balances now update on notification instead of waiting for the next poll — CRD ch.38 R38.6.5.** The requirement — emit balance-change events with watched addresses registered at enable time and re-registered as needed — was only half met: events were emitted, but purely from a 30 s timer, and nothing was ever registered with a server. The client already received server-initiated notifications yet recognised exactly one method and discarded the rest as errors. It now routes `blockchain.scripthash.subscribe` notifications to a registry keyed by script hash, and the balance streamer subscribes its coin's address on start, races the notification against the poll deadline, and re-reads the balance as soon as a server reports a change. A deposit or incoming swap payment therefore appears in about the time the server takes to notice it, rather than up to a full poll interval later. Watched hashes are re-armed on every newly connected server, because a subscription belongs to one session and a reconnect or server swap drops it silently. **Polling is retained as the correctness floor**: a coin whose subscription fails, is unavailable, or dies silently keeps its existing timer, so the worst case is the latency we had before, never a stale balance. Applies to Electrum-backed UTXO, QTUM and BCH coins; native-RPC and non-UTXO families are unchanged. Unrecognised notification methods are now ignored rather than logged as errors, since a server may push subscriptions we never requested. Code: `mm2src/coins/utxo/rpc_clients/electrum_rpc_client.rs`, `mm2src/coins/utxo/utxo_builder/utxo_coin_builder.rs`, `mm2src/mm2_main/src/rpc/streaming_activations/balance.rs`.

### Changed / dependencies

- **Security and yanked dependency refresh.** Transitive `ruint` is upgraded
  from 1.18.0 to the security-fixed 1.20.0 for RUSTSEC-2026-0220. Compatible
  patch updates also replace the yanked `ahash 0.7.6`,
  `crossbeam-channel 0.5.1`, and `rmp-serde 0.14.3` releases; MessagePack stays
  on the wire-compatible 0.14 line. Obsolete `libsqlite3-sys` and `rand`
  advisory exceptions are removed from the audit policy.
- **Stable modern `librustzcash` stack with schema-safe shielded rescan.**
  Z-coin now uses the coherent stable crates.io line
  (`zcash_client_backend 0.23.0`, `zcash_client_sqlite 0.21.1`, and
  `zcash_primitives`/`zcash_proofs 0.28.0`) instead of the legacy vendored 0.5
  API. Reloaded stores the modern wallet and compact cache as
  `<TICKER>_RELOADED_WALLET.db` and
  `<TICKER>_RELOADED_COMPACT_BLOCKS.db`, leaving GLEEC/pre-upgrade files
  untouched. Exact legacy schemas are preserved and rebuilt/rescanned rather
  than migrated in place; unknown or corrupt schemas fail without mutation.
  The selected-current migration/schema fingerprint is pinned, cached-chain
  continuity is validated before scanning, and KDF P2SH transaction bytes are
  protected by full serialization regressions. The broad old Zcash workspace
  dependency is replaced by exact registry versions plus three narrow,
  documented crate patches for the obsolete `time-core` pin and required KDF
  transaction-builder compatibility. See CRD chapter 39 and
  [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md). This port completes the
  Light-mode modern store/scan path; the pre-existing Native full-node adapter
  does not yet populate that modern compact cache and remains an explicit CRD
  chapter-39 gap.
- **Shared SQLite binding upgraded from `rusqlite 0.24.2` to `0.37.0`.** All
  native SQLite consumers now share the version required by the stable Zcash
  wallet store. Legacy `NO_PARAMS`/named-query APIs and dynamic parameter calls
  were ported without changing their SQL contracts. The upgrade also exposed
  and removed one deprecated Solana keypair accessor in tests; the independent
  `nom 6.1.2` future-incompatibility remains owned by the pinned `sia-rust`
  dependency rather than Zcash.
- **`rand` 0.7 → 0.8** across all direct reloaded usages (RUSTSEC-2026-0097; upstream-blocked advisory). Code: multiple crates.
- **`mm2_metrics` rewritten** with a hand-rolled Prometheus registry; drops the dead `metrics-runtime 0.13` / `metrics-util` stack, clearing RUSTSEC-2021-0113. Code: `mm2src/mm2_metrics/`.
- **`anyhow` bumped to 1.0.103, `crossbeam-epoch` to 0.9.20** (advisory clears). Code: `Cargo.toml`.
- **CI test failures now include Rust backtraces.** `tests.yml` sets `RUST_BACKTRACE=1` for test jobs so panics provide actionable stack traces in CI logs. Code: `.github/workflows/tests.yml`.

## [0.2.0-beta.1] - 2026-08-05

### Added

- **Known beta limitation: shielded Zcash/ARRR is not supported in the web-wallet WASM bundle yet.** The WASM CI safety net now uses a direct `cargo check --target wasm32-unknown-unknown -p mm2_main` check because the packaging step hit a duplicate `secp256k1-sys` link collision from the Zcash/ARRR stack. Native and desktop builds remain the supported targets for shielded Zcoin activation and sync until the web bundle is normalized.

- **Z-coin (ARRR/ZHTLC) WASM support — R39.6.1.** The shielded coin module is now available on the `wasm32-unknown-unknown` target. `zcash_primitives` and `zcash_client_backend` are added as WASM dependencies. The sapling state cache is abstracted behind a `SaplingStateCacheOps` trait with a SQLite backend for native and an IndexedDB backend (mm2_db) for WASM. `MmCoinEnum::ZCoin` and the `z_coin` module are now gated for all targets. Transaction building (`gen_tx`/`send_outputs`) remains native-only pending WASM delivery of the sapling parameter files. Code: `mm2src/coins/z_coin/`, `mm2src/coins/Cargo.toml`, `mm2src/coins/lp_coins.rs`, `mm2src/coins/lp_coins_context.rs`.
- **Z-coin sync-parameter control — R39.6.2.** The `task::enable_z_coin::init` activation request now honours the externally-dictated sync controls (confirmed against the public komodoplatform API reference and KDF-family wallets): a `sync_params` field **inside the Light-mode `rpc_data`** (`activation_params.mode.rpc_data.sync_params`), shaped as the externally-tagged union `{"height": <uint>}` / `{"date": <unix-seconds>}` / `"earliest"`; plus top-level `scan_blocks_per_iteration` (u32) and `scan_interval_ms` (u64, `scan_interval` accepted as an alias). Height starts pass through, date starts resolve to a block height via a binary search over backend block timestamps, and `earliest` maps to Sapling activation. (Supersedes the earlier, incorrect top-level `sync_start` / `{"type","data"}` shape.) Code: `mm2src/coins_activation/src/z_coin_activation.rs`, `mm2src/coins/z_coin.rs`.
- **Z-coin HD-derived shielded key policy — R39.6.4 §2.** Under an HD (BIP39) wallet, the shielded Sapling spending key is now derived from the wallet seed along the coin's `z_derivation_path` with the activation `account` appended as a hardened child (`m/<z_derivation_path>/account'`); the legacy Iguana policy keeps deriving the ZIP32 master from the iguana secret. A new optional `account` field (u32, default 0) on `task::enable_z_coin::init` selects the account (ignored under the Iguana policy). Only the shielded key is HD-derived; the transparent UTXO side remains iguana-derived. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins_activation/src/z_coin_activation.rs`.
- **Z-coin activation reports the resolved shielded sync start — R39.8.0h.** When a `sync_params` is supplied, the activation result now includes a `first_sync_block` object `{ requested, is_pre_sapling, actual }`, where `requested` is the resolved start height (from a height or a date), `is_pre_sapling` flags a request below Sapling activation, and `actual` floors the start at Sapling activation. Code: `mm2src/coins_activation/src/z_coin_activation.rs`.
- **Z-coin (ARRR/ZHTLC) activation-wire conformance — R39.3 / R39.6.2 / R39.8.0h.**
  `task::enable_z_coin` now matches the dictated public activation wire on the
  remaining points: the status task reports the two dictated scan phases with
  live progress (`UpdatingBlocksCache` during the compact-block download and
  `BuildingWalletDb` during the wallet-DB scan, each with `current_scanned_block`
  / `latest_block`); the activation result includes the top-level `ticker` and
  emits `first_sync_block` unconditionally; the request honours
  `zcash_params_path` (native Sapling parameter directory) and `skip_sync_params`
  (resume from local state, consulting `sync_params` only when no prior state
  exists); and blocks-per-iteration now defaults to the dictated `1000`. The
  modern Light wallet-DB scanner now applies the effective
  `scan_blocks_per_iteration` and `scan_interval_ms` values instead of using a
  fixed 1,000-block batch with no pause. Activation logs the effective policy at
  INFO; network and wallet batch timing/frontier details are bounded DEBUG/TRACE
  diagnostics, while the normal Light-mode bypass of the legacy native cache is
  no longer reported as a warning. Code:
  `mm2src/coins_activation/src/z_coin_activation.rs`, `mm2src/coins/z_coin.rs`,
  `mm2src/coins/z_coin/z_coin_ops.rs`, `mm2src/coins/z_coin/z_coin_wallet_db.rs`.
- **Tendermint `denom` / `decimals` / `ibc_channels` in `CoinProtocol`.** `CoinProtocol::TENDERMINT` now carries optional `denom`, `decimals`, and `ibc_channels` fields (R36.3.3), aligned with the komodo-coins config schema for ATOM-family coins. Code: `mm2src/coins/tendermint/`.
- **V1 swap and taker order-status SSE.** The SSE streaming infrastructure now emits live maker/taker swap and order-status events under the existing `stream::*` namespace. Code: `mm2src/mm2_main/`.
- **SSE streamer parity endpoints.** The CRD and implementation now cover `stream::disable`, `stream::fee_estimator::enable`, `stream::tx_history::enable`, and native non-Windows `stream::shutdown_signal::enable`, completing the currently bound `stream::*` parity surface. Code: `docs/reloaded-rewrite/10-sse-streaming.md`, `mm2src/mm2_main/`.
- **RPC-dump development build workflow.** A manual `dev-build-rpc.yml` workflow builds with the RPC dump feature set, and the cross-platform build workflows pass feature flags consistently on Linux, macOS, Windows, iOS, and Android. Code: `.github/workflows/`.

### Fixed

- **ARRR Light activation checkpoint, recovery, and concurrency.** Lightwalletd
  `TreeState` display-order block IDs are now converted to the canonical
  little-endian compact-block representation before first-block continuity
  validation, fixing valid scans that failed immediately with a hash
  discontinuity and consequently reported a zero balance. A recoverable hot
  SQLite journal is classified through a temporary recovery copy without
  mutating the source, rather than being mislabeled as schema corruption after
  logout or interruption. Same-ticker ZCoin activation tasks are serialized
  across the full activation lifecycle to prevent concurrent rebuild/rename
  races on Windows. Compact-block network batches are validated and committed
  atomically in one SQLite transaction, reducing long historical-sync write
  overhead and preventing partial batches. After a restart, a complete cached
  segment is chain-validated and resumed at its next height, avoiding another
  download of already persisted history; a discontinuous segment is preserved
  and rebuilt instead of being trusted by its maximum height. Code:
  `mm2src/coins/z_coin/`, `mm2src/coins_activation/`.

- **ARRR Light scans with Pirate compact-block metadata.** Pirate's dictated
  compact-block protobuf predates the modern optional `ChainMetadata` field.
  Reloaded now supplies the missing first-block Sapling tree size in memory
  from the trusted checkpoint frontier and the block's explicit outputs, so
  the stable `zcash_client_backend` scanner no longer aborts with an unknown
  tree-size error. Reloaded also carries the exact compact-source frontier
  between bounded wallet-scan batches instead of reconstructing it from the
  prunable wallet tree, fixing later failures near completion with `Unable to
  compute root; missing values for nodes`. After a restart it refreshes the
  frontier at the persisted scan height and validates its block hash and tree
  size before continuing. Scan failures are also logged explicitly instead of
  appearing only in the activation task response. Code:
  `mm2src/coins/z_coin/`.

- **Z-coin (ARRR/ZHTLC) shielded sync honours the requested sync start.**
  The activation request's shielded sync point (`mode.rpc_data.sync_params`)
  was being dropped on the wire — the daemon expected a top-level `sync_start`
  with a different shape, so the parameter deserialized to nothing and the
  fetch plan always saw `requested_start_height=None`. On top of that, once a
  shielded wallet had scanned to the chain tip, activation short-circuited on
  the already-scanned state and reused the stale local cache, so the wallet
  displayed a balance almost instantly regardless of the requested start. The
  request now parses the dictated wire (`sync_params` as
  `{"height"|"date":N}`/`"earliest"` inside the Light `rpc_data`), and
  activation compares the requested start against the wallet's sync anchor and,
  when they differ (earlier or later), rewinds/recreates the compact-block
  cache and wallet database and rescans from the requested start, re-seeding
  the commitment tree from the light backend — matching upstream behaviour. An
  unchanged start still resumes from the tip without a rescan, and a start
  beyond the tip is clamped. Fixes #2. The remaining ZHTLC activation-wire
  divergences (two-phase scan progress, result `ticker`, `zcash_params_path`,
  `skip_sync_params`, unconditional `first_sync_block`, and the `1000`
  blocks-per-iteration default) are addressed by the activation-wire conformance
  entry above. Code:
  `mm2src/coins_activation/src/z_coin_activation.rs`,
  `mm2src/coins/z_coin/z_coin_wallet_db.rs`.

- **Small KMD direct-burn DEX fees retain the legacy wire shape.** Netid 8762
  KMD taker-fee construction now permits the positive 75% fee-collection
  output selected by the `v2.6.0-beta` policy even when that split component
  is below KMD's generic spendable-output dust threshold. The exception is
  scoped to that protocol-defined output; ordinary outputs and change retain
  the existing dust checks. Code: `mm2src/coins/utxo/`.
- **HD UTXO activation honours `min_addresses_number`.** API-v2 UTXO
  activation now persists and returns the requested minimum number of external
  addresses for each HD account, so an empty account activated with the
  wallet-standard value `1` has external address `0` available to balance,
  order, and swap-preimage paths. Empty HD accounts also retain the activated
  ticker's zero-valued entry in `total_balance` instead of returning an untyped
  empty object. Code: `mm2src/coins/`, `mm2src/coins_activation/`.
- **DEX-fee wire compatibility on both production netids.** Netid 8762 KMD
  takers now use the `v2.6.0-beta`-compatible discounted fee and two-output
  75/25 fee/OP_RETURN structure, while non-KMD takers remain single-output.
  Netid 6133 follows the v3/dev single-output fee structure. Both networks now
  use only the taker coin's minimum transaction amount as the fee floor,
  removing the erroneous additional `0.0001` floor. Fixes #1. Code:
  `mm2src/mm2_net_config/`, `mm2src/mm2_main/src/lp_swap/`,
  `mm2src/coins/utxo/`.
- **`CoinProtocol::NFT` variant accepted.** A permissive NFT variant is added to `CoinProtocol` so NFT-typed coin configs no longer fail deserialization. Code: `mm2src/coins/lp_coins.rs`.
- **NFT subsystem activation RPC parity.** `enable_nft` is routed as the explicit NFT subsystem activation method, with CRD coverage for the activation contract. Code: `docs/reloaded-rewrite/19-nft-module-layout.md`, `mm2src/mm2_main/`, `mm2src/coins/nft/`.
- **SSE activation response wire shape.** `stream::*::enable` success payloads now expose the upstream-compatible `streamer_id` field without the reloaded-only `active` boolean. Code: `docs/reloaded-rewrite/10-sse-streaming.md`, `mm2src/mm2_main/`.
- **ZHTLC `protocol_data` deserialization.** Bare `{"type":"ZHTLC","protocol_data":{...}}` configs now deserialize correctly; previously missing `protocol_data` support caused ARRR activation to fail. Code: `mm2src/coins/z_coin_activation.rs`.
- **Zcash consensus parameters sourced from `protocol_data` — R39.6.4.** The shielded builder now reads all network parameters (`consensus_params`, `check_point_block`, `z_derivation_path`) from `protocol.protocol_data` rather than hardcoded Zcash-mainnet constants. A ZHTLC coin with non-mainnet HRP/b58 prefixes or activation heights uses its declared parameters end-to-end. Code: `mm2src/coins/z_coin.rs`, `mm2src/coins_activation/src/z_coin_activation.rs`.
- **`{"type":"ETH"}` protocol config accepted again.** A regression caused standard ETH coins using the `{"type":"ETH"}` protocol object to fail activation; restored. Code: `mm2src/coins/lp_coins.rs`.
- **UTXO dynamic-fee trade preimage is consistent** for `UpperBound` vs `Exact` fee policies. Code: `mm2src/coins/utxo/`.
- **ETH `estimate_gas` insufficient-balance revert mapped to `NotSufficientBalance`** instead of a generic transport error. Code: `mm2src/coins/eth/`.
- **getrandom 0.3 `wasm_js` backend enabled on wasm32** so entropy sources compile correctly on the WASM target. Code: `Cargo.toml`.
- **sia-rust bumped to `e1c0725`** (nom 7.1.3 and WASM bindgen refresh).
- **ARRR/ZCoin activation and light-mode scanning.** ZHTLC activation no longer panics through the task manager when a failing Electrum candidate is encountered; activation fails over across available Electrum servers. Light-mode activation creates the required Sapling cache, bounds shielded-history scanning, handles stale checkpoints, uses the Pirate-compatible lightwalletd gRPC package, supports TLS lightwalletd endpoints, and reports shielded wallet DB balances for ARRR instead of returning zero. Code: `mm2src/coins/`, `mm2src/coins_activation/`.
- **ARRR/ZCoin and direct-withdraw coin previews through `task::withdraw`.** ZCoin/ARRR shielded withdraws, plus BCH, QRC20, SLP, Solana/SPL, Sia, and Tendermint native/token withdraws, are routed through the task-withdraw API instead of failing preview/status with `CoinDoesntSupportInitWithdraw` when clients use the task path. Lightning remains intentionally unsupported by withdraw because invoices are the payment entrypoint. Code: `mm2src/coins/rpc_command/init_withdraw.rs`, `mm2src/coins/z_coin.rs`.
- **ARRR/ZCoin light-mode shielded withdrawal construction.** Shielded ARRR withdraw previews in light mode now build spends from the scanned `<TICKER>_RELOADED_WALLET.db` note/witness data instead of entering the native zcashd `ZRpcOps` path, preventing preview-time KDF crashes on Electrum-backed activations. Code: `mm2src/coins/z_coin/`.
- **Orderbook address handling for Tendermint/Sia/Solana-family configs.** Orderbook and best-orders rendering no longer panic when a P2P order references non-UTXO protocol configs encountered by multi-coin wallet sessions. Tendermint addresses are derived from the order pubkey where possible; unsupported address families now return structured per-order errors and are skipped instead of crashing KDF. Code: `mm2src/mm2_main/src/lp_ordermatch/`.
- **Legacy maker-swap refund event compatibility.** Data-less historical
  `MakerPaymentRefundStarted` and `MakerPaymentRefundFinished` milestones now
  deserialize as their distinct unit events, so old refund-path swaps no longer
  disappear from swap history/status with `missing field data`. The known
  payload-bearing refund-start form remains accepted and preserves its
  `wait_until` deadline as a wait-refund milestone. Code:
  `mm2src/mm2_main/src/lp_swap/`.
- **Legacy swap instruction-event compatibility.** Saved maker/taker swap files whose `MakerPaymentInstructionsReceived` or `TakerPaymentInstructionsReceived` event omitted the optional `data` field now deserialize as `None`, preventing repeated `missing field data` errors in swap history/status polling. Code: `mm2src/mm2_main/src/lp_swap/`.
- **Legacy swap-history resilience.** `my_recent_swaps` now logs and omits missing or corrupt saved-swap rows instead of returning `null` entries or failing typed recent-swap consumers, preventing wallet swap pages and order processing from being destabilized by incompatible historical swap files. Code: `mm2src/mm2_main/src/lp_swap/`.
- **Ordermatch trie-delta removal test made deterministic.** The orderbook sync test no longer depends on nondeterministic ordering when asserting a delta after removed orders. Code: `mm2src/mm2_main/src/ordermatch_tests.rs`.
- **Coin activation/runtime hardening for multi-asset wallets.** ERC20/BEP20 tokens with missing or zero config decimals now fall back to on-chain `decimals()`, NFT activation lazily opens the native async SQLite store instead of failing with `async_sqlite_connection is not initialized`, and Tendermint RPC node selection falls back from `/health` to `abci_info` before declaring all nodes unavailable. Code: `mm2src/coins/`, `mm2src/coins_activation/`.
- **Runtime stubs converted to explicit no-op/errors where safe.** Unsupported TRON V1 swap paths now return structured errors, L2 SQL transaction-history queries return an unsupported-history error instead of panicking, wasm crash-report initialization is an intentional no-op, and public-key trait methods for enabled coin families no longer panic when called by production paths. Code: `mm2src/coins/`, `mm2src/common/`.

## [0.1.0-beta.2] — 2026-07-07

Stability release focused on HD-wallet interoperability with the Komodo DeFi wallet (Flutter SDK). Continues from `0.1.0-beta.1` under GPLv2-only.

### Fixed

- **HD-wallet login no longer hangs on a spinner.** `RpcTaskStatus` now serialises the terminal state as `status: "Ok"` / `status: "Error"` (tagged `status`/`details`) instead of the previous `Ready` envelope, matching what the SDK's task-status polling expects. Applies to `task::enable_utxo`, `task::account_balance`, `task::create_new_account`, `task::withdraw` and other long-running task-status responses. Code: `mm2src/rpc_task/`.
- **Concurrent coin activation no longer throws `CoinIsAlreadyActivated`.** A coin already registered under a racing activation request is now treated as an idempotent success rather than surfacing an uncaught exception to the client. Code: `coins_activation/src/standalone_coin/init_standalone_coin.rs`.
- **HD balance responses are ticker-keyed.** `HDAccountBalance.total_balance` and `HDAddressBalance.balance` are now `{ "<TICKER>": { spendable, unspendable } }` maps, fixing the wallet's `PUBKEY_ACTIVATION_ERROR` / `"String is not a subtype of Map"` on HD activation. The legacy `my_balance` response is unchanged (remains a flat object). Code: `coins/coin_balance.rs` and HD balance/activation paths.

### Changed / divergences

- **Withdraw with an omitted HD `from` defaults to the enabled address** (account 0 / external / index 0) instead of failing with `FromAddressNotFound`. This is a deliberate divergence from GLEEC KDF, documented in [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md); there is no compatibility switch in this release. An explicit but invalid `from` is still rejected.
- **Legacy `<db_root>/wallets/*.wallet` wallets are read (login/list/delete) but never written**; new wallets are always the canonical `<name>.json` record. Documented in [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md) and CRD chapter 07 §7.5/§7.7 (R9A). The storage format does not by itself select HD vs single-address signing.

## [0.1.0-beta.1] — 2026-07-05

Initial public beta (first published pre-release). Continues the Komodo DeFi Framework codebase from the GPLv2 anchor commit `c1d46c0c1592faa0860f704008b2b2381bc3840f` (2022-06-03) under GPLv2-only.

For the full feature delta against the GLEEC fork of the upstream codebase, see [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md).

Highlights:

- Mainnet support on netid 8762 (AtomicDEX) and netid 6133 (GLEEC).
- Stable Rust toolchain (no nightly pin).
- Self-hosted CI with split format / unit-test / docker-test jobs.
- On-demand release builds for Linux (additional platforms via the umbrella dev-build workflow).
- Documentation-only compatibility convention for GLEEC-equivalent operation (developer rule in [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md), central admin chapter in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md)); no divergent settings active in this release.
- Test-only netids (8100, 8999, 9000, 9998) gated behind the `regtest-netid` Cargo feature, off by default in production builds.
- EVM ABI encoding/decoding migrated from `ethabi` to `alloy` (`alloy-dyn-abi` / `alloy-json-abi` / `alloy-primitives`), guarded by a byte-identity golden regression suite.
- Tag-driven, GPG-signed release pipeline: signed `SHA256SUMS` manifest and drafted GitHub Releases — pre-releases from `staging` (`-alpha/-beta/-rc` tags), finals from `main`. Linux binaries build inside a Debian 11 container (glibc 2.31) for broad backwards-compatibility; unsigned all-platform snapshots via manual `dev-build.yml` and automatic `staging-build.yml`.

Tag `v0.2.0-beta.1` (2026-08-05) was cut on `staging` from the development line as of commit
6dc1b63b1, part-way through 0.3.0-alpha.1. Its section above is the tag's own text, restored on
2026-11-08; until then its entries were listed under 0.3.0-alpha.1. The correction of its KMD fee
entry is in the 0.3.0-alpha.1 section (the entry marked "Superseded (issue #11)").

[Unreleased]: https://github.com/kdf-reloaded/kdf/compare/v0.3.0-beta.1...HEAD
[0.3.0-beta.1]: https://github.com/kdf-reloaded/kdf/compare/ddb87a4849f91400b1b4aa120090f1acb12aa7bf...v0.3.0-beta.1
[0.3.0-alpha.3]: https://github.com/kdf-reloaded/kdf/compare/49a46bc54fc16761796ed0bad53a472b8183ed9b...ddb87a4849f91400b1b4aa120090f1acb12aa7bf
[0.3.0-alpha.2]: https://github.com/kdf-reloaded/kdf/compare/7a18f94b2f5de1ff7ba2ae69c4b445132d305a3f...49a46bc54fc16761796ed0bad53a472b8183ed9b
[0.3.0-alpha.1]: https://github.com/kdf-reloaded/kdf/compare/v0.1.0-beta.2...7a18f94b2f5de1ff7ba2ae69c4b445132d305a3f
[0.2.0-beta.1]: https://github.com/kdf-reloaded/kdf/releases/tag/v0.2.0-beta.1
[0.1.0-beta.2]: https://github.com/kdf-reloaded/kdf/compare/v0.1.0-beta.1...v0.1.0-beta.2
[0.1.0-beta.1]: https://github.com/kdf-reloaded/kdf/releases/tag/v0.1.0-beta.1
