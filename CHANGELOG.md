# Changelog

All notable changes to KDF Reloaded are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Pre-1.0 releases use the `0.MAJOR.MINOR-PRERELEASE.N` convention; expect breaking changes between alpha and beta.

## [Unreleased]

### Added

- **Orders too small to be spent at current fees are now refused.** `setprice`, `update_maker_order`,
  `buy`, `sell` and `trade_preimage` refuse an order whose smallest trade would leave a UTXO-family
  or Sia payment unspendable, meaning below the current spend-fee estimate plus dust, or the Sia
  miner fee plus one hasting. The refusal is the usual volume-too-low error, with that amount as the
  threshold. Example: BTC at 0.0001 when the fee is high. The minimum volume an order advertises does
  not change, and peers see nothing new. Set the `MM2.json` boolean `pre_trade_spend_check` to `false`
  to restore GLEEC KDF behaviour, with no check. Detail: PR #73; CRD ch.60 R60.6.7.

- **A custom fee for Siacoin withdrawals.** A Siacoin `withdraw` now honors the fee you enter in the
  wallet's custom-fee box, given as SC per kilobyte; leave it out and the fee is estimated as before.
  Any positive fee is accepted, including one below the estimate; a fee one tenth of the estimate was
  accepted and mined within about ten minutes on an uncongested transaction pool (checked live
  2026-10-26; a full pool is untested). Any other fee type sent for Siacoin is now rejected with an
  invalid-fee-policy error instead of being silently ignored. More in
  [`sia-withdraw-fee-control`](docs/plans/sia-withdraw-fee-control.md).
- **Activating a shielded coin without `sync_params` now logs a warning when the default recent scan
  window applies.** It names the start height and window size, notes that earlier funds will not be
  visible, and says to re-activate with `sync_params`; `first_sync_block` remains the machine-readable
  signal (CRD ch.39 R39.8.0aw). Detail: PR #61.
- **Releases and the runtime Docker image now ship the license files.** `COPYING`, `LICENSE` and
  `THIRDPARTY-LICENSES` are attached to every release and covered by the signed `SHA256SUMS`, and
  the image carries them in `/usr/share/doc/kdf/`. Detail: PR #36.
- **A shielded (ARRR) withdrawal short only because of an unconfirmed transaction now says so.**
  Instead of `NotSufficientBalance` with `available: 0` beside a healthy balance, it fails with
  `BalanceLockedByUnconfirmedTransaction` (HTTP 400) naming the amounts and that it clears when
  that transaction confirms. After a node restart the bare error is reported again. Detail: PR #50.
- **Radiant (RXD) now activates and signs with Radiant's own signature hash.** A UTXO coin whose
  `signature_version` is `fork_id_rxd` signs every input with the Radiant dialect: the Bitcoin-Cash
  BIP-143 preimage with one extra digest, a 76-byte summary per output, inserted before the outputs
  digest. RXD's shipped entry therefore no longer refuses to activate, and needs no switch.
  Withdrawals, swap transactions and `sign_raw_transaction` all use it. The digest is checked
  against two Radiant mainnet transactions, one of whose outputs carry Radiant references. Shapes
  with no on-chain check yet: duplicate references, the three reference opcodes that are skipped,
  and sighash modes other than `ALL | FORKID`. RXD's fitness for swaps beyond signing is not
  established, so this does not announce RXD trading. Detail: PR #65; CRD ch.58 §58.3, §58.5.
- **Documented why a migrated shielded (ARRR) wallet can read a zero balance.** Activating a
  shielded coin with no `sync_params` scans only a recent window (2,880 blocks, two days at a
  60-second block time), so funds older than that are invisible while the sync completes
  normally. This matches the reference, whose window is shorter; the remedy is to pass
  `sync_params`. Detail: `docs/GLEEC_COMPATIBILITY.md`.

### Changed

- **Electrum servers are kept in priority order and recover.** Servers are tried in an order drawn at
  random once when the coin is enabled, so users spread across the listed servers (set
  `electrum_server_order` to `listed` to keep the listed order); a recovered higher-ranked server is
  used again; a server that keeps failing is skipped for a growing
  time (20 s doubling up to 12 h); if every connected server fails, every listed server is tried;
  connecting is bounded by `timeout_sec` (default 20 s) and each request by 20 s; errors name each
  server tried as host:port; enabling a coin tries every listed server at once and succeeds as soon as
  one connects; if none does, the error names each server. More in ch.38 §38.6.4.

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

- **A node now asks for exactly the order-book changes it is missing.** When a maker's keep-alive
  shows that a node's copy of the maker's orders differs, the node asks another node for an update.
  It used to describe its copy by the maker's advertised state instead of its own, so while the maker
  kept changing orders the answer could be meant for a different copy, and the node kept only part of
  the maker's orders, asking again every 30 seconds. The node now names the state it actually holds and
  gets the changes it can apply, or the full set. Set the `MM2.json` boolean
  `orderbook_sync_names_held_root` to `false` to ask the way GLEEC KDF does. Detail: CRD ch.32 R-Q4a.

- **`update_maker_order` no longer broadcasts an update that changes nothing.** A market-making bot that
  re-submits all of its orders on every refresh used to send one message per order to every node, even
  when price, volumes and confirmations stayed the same. Such an update is still saved and answered as
  before, but no message goes out, so the network carries only real changes. Set the `MM2.json` boolean
  `suppress_unchanged_order_updates` to `false` to broadcast every update again. Detail: CRD ch.59
  R59.4.6.

- **A cancelled order's identifier is no longer ignored once its two-minute window is over.** A node remembers for 120 seconds that a peer cancelled an order and ignores a late announcement of the same order during that time. The check did not look at the age of the memory, so an old entry could keep ignoring the same order after the window, until the next cancellation cleared it. The check now treats an entry older than the window as absent. Orders get random identifiers, so this was very unlikely to matter in practice. Detail: PR #102; CRD ch.11 R10.

- **Posting and updating maker orders makes fewer Electrum and node requests.** Electrum servers
  serve each connection about one request per second once its first burst is used, so every extra
  request cost about a second. A legacy address such as RVN, DOGE or KMD now reads its balance and
  its coins in one request instead of two. Each order request reads a coin's fee estimate and an EVM
  coin's gas price only once, and an EVM gas price's two node queries run in parallel. An order
  update that changes only the price, the minimum volume or the confirmation settings skips the
  pre-trade spend check and makes no fee request. Detail: PR #114; CRD ch.38 R38.6.3a, R38.6.6j,
  ch.57 R57.2.6, ch.60 R60.6.7.

- **An unresponsive EVM node is given up after 10 seconds instead of 60.** The limit now covers the
  whole attempt, from connecting to the last byte of the answer, on every platform, so a slow node no
  longer holds a request for a minute before the next node is tried. A request given up on is now
  closed too, instead of staying open in the background for as long as the node keeps silent. More in
  ch.35 §35.1.F.

- **EVM and Tron log lines and errors name a node by scheme, host and port only.** Paths, query
  strings and credentials, where hosted providers keep API keys, are never printed: the activation
  probe, the per-node failure logs, the error that lists the nodes tried, and the lower-layer texts
  they carry. When a node answers with an error, the log says so
  (`EVM RPC <host> answered <method> with error <code>: <message>`) and the error returned to the
  caller ends with `(node <host>)`, so a revert reported by one of several nodes can be attributed.
  The same goes for the gas-station URL and the gas-API provider (Infura, Blocknative) URL, which
  also carry API keys. There is no switch to print the full URL. More in ch.35 §35.1.F.
- **ARRR native mode is refused in this release; use light mode.** Activating ARRR (or another
  shielded coin) with `mode.rpc` `Native` now fails at once with an error saying that native mode is
  not supported in this release and that light mode should be used. Native mode could not build a
  working wallet for a fresh activation, and after Ironwood it could build transactions the network
  rejects. Light mode is unchanged. Native mode returns in a later release, see
  [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md). GLEEC KDF accepts
  native mode. Detail: PR #115; CRD ch.39 R39.11.1a.
- **TRON (TRX, TRC20) cannot be activated in this beta.** Its support is not finished; activation
  answers "TRON support is not available in this build" instead, and the rest of the node is
  unaffected. It is planned to return in the next beta.

- **Solana (SOL, SPL tokens) cannot be activated in this beta.** Its support is not finished;
  `enable_solana_with_tokens` and `enable_spl` answer "Solana support is not available in this
  build" instead, and the rest of the node is unaffected. It is planned to return in the next beta.

- **Tendermint (Cosmos) coins are wallet-only in this beta.** Activation, balances and withdrawals
  work; orders and swaps are not available and are planned to return in a later release. A swap
  already in progress still completes. An order with a Tendermint coin saved by an earlier
  version is cancelled at startup.

- **A shielded (ARRR) send that has to wait for an earlier unmined send now waits up to 10 minutes
  instead of 5, retrying every 15 seconds, as GLEEC KDF does.** The wait still ends at once when no
  waiting could free enough funds. More in
  [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md). Detail: PR #85.

- **Fixed-fee UTXO coins now pay their `txfee` per kilobyte proportionally, as the reference does.**
  Every coin with a nonzero `txfee`, or none, used to pay the whole per-kilobyte fee for every
  started kilobyte, so a 226-byte KMD transaction cost 1,000 base units instead of 226. Withdrawals,
  swap payments and the DEX fee transaction now pay `floor(txfee × size / 1000)`, and a coin that
  forces the node's minimum relay fee pays that rate proportionally as well. A segwit transaction's
  size is its virtual size rounded up, as nodes count it, so a fee at the relay minimum is never one
  unit short. Swap spends and refunds
  are estimated over 496 bytes instead of 305 bytes rounded up to a whole kilobyte: a KMD spend costs
  496. An explicit `UtxoFixed` withdraw fee keeps its meaning. Detail: PR #72; CRD ch.38
  R38.6.6b–c.
- **The coins-file option `dingo_fee` is now honoured.** With `dingo_fee: true` and a positive
  `txfee`, a coin pays `txfee` for every started kilobyte. DINGO, DEV, NYC, SHIC, TRMP and XVG carry
  it. Their version-two swap spend fee is now one `txfee`, matching a reference peer that validates
  it exactly; before, such a swap failed that peer's check. Their minimum trading volume uses the
  same fee. Detail: PR #72; CRD ch.38 R38.6.6a, ch.60 R60.6.2.
- **The README now explains why the project exists.** Upstream's license added conditions on the
  trading-fee code, its default network since v3.0 charges a 2% fee, and v3.0 refuses to start on
  network 8762; KDF Reloaded keeps both networks usable. See also
  [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md),
  [`RELOADED_VS_GLEEC.md`](RELOADED_VS_GLEEC.md) and [`docs/NETWORK_CONFIG.md`](docs/NETWORK_CONFIG.md).
  Detail: PR #57.
- **A UTXO coin's header layout now reaches every header read, and `protocol.chain_variant` is honored for
  the first time.** The key is read from the coin's entry (eight values; an unrecognized one fails the
  coin's activation naming the ticker and value), and the coin's layout (`header_layout`, else
  `chain_variant`, else the Qtum reader for a `QTUM`/`QRC20` protocol type) is used by header sync, the
  reorg walk, block timestamps, SPV, the stored-header re-reads and the WalletConnect genesis lookup, not
  only the median-time-past. The structural fallback for `RIC` and `MEWC` now applies on all of those paths
  too. The families without a dedicated reader still read the standard layout (CRD ch.37 R37.5.1,
  R37.5.1a, R37.5.9e). Detail: PR #74.

- **The generated `coins/utxo/pb.rs` is no longer committed, and it is now formatted when generated.**
  `mm2src/coins/build.rs` regenerates it from `bchrpc.proto` on every build, so the committed copy
  left a formatting-only diff in every worktree. The file is git-ignored and `prost-build` runs with
  its `format` feature, producing the same text the repository used to carry. No runtime change.
  Detail: PR #63.
- **The initial swap gas-fee policy no longer depends on how the coin was activated, and a coin's
  `max_eth_tx_type` now caps the transaction type it will sign.** The legacy `enable` / `electrum`
  route resolves the policy the way the other routes do — the `swap_gas_fee_policy` request field,
  else the `swap_gas_fee_policy` key of the coin's entry, else `Legacy` — and that request field is
  new on the legacy route and parsed leniently there: an unrecognised value is ignored, whereas
  `enable_eth_with_tokens` and `task::enable_eth` still reject it with `InvalidRequest`. A swap send
  whose EIP-2718 type exceeds the coin's `max_eth_tx_type` (a token falls back to its platform
  coin's entry) is now refused at signing, with nothing broadcast and no lower type substituted.
  **A coin with no `max_eth_tx_type` key counts as `0`**, so a typed swap send for it is refused
  too; an operator who wants an absent key to mean unrestricted sets the node-level boolean
  `max_eth_tx_type_absent_unrestricted` to `true` (default `false`). Activation logs the effective
  policy and cap and where each came from. Detail: PR #30; CRD ch.35 D35.6 and D35.7.
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

### Removed

- **The legacy `iguana/` and `etomic_build/` directories are gone.** Both were remnants of the
  codebase's early days with no build, runtime, CI or documentation dependency. `iguana/tools/`
  held a single Python 2.7 byte-compiled file, `getos.pyc` — the only `.pyc` in the tree — that
  nothing referenced; `etomic_build/` held outdated `curl` wrapper scripts for the legacy RPC that
  launched a binary path and sourced a coins file which no longer exist. The `.gitignore` entries
  for paths under them were dropped with them; no other document changed. Three dead root scripts
  went with them: `start_ONE_ANOTHER_trade.sh` (drove a `docker-compose` setup the repository
  never contained and called scripts from the removed `etomic_build/`), `travis_cmake_linux.sh`
  and `travis_cmake_mac.sh` (Travis-era CMake installers; the repository has no Travis
  configuration). Detail: PR #64.

### Fixed

- **A peer that cannot keep up no longer makes a node's memory grow without limit.** The queue of
  gossip messages waiting to be sent to one peer is now capped at 2,048 messages or 4 MiB, whichever
  comes first. When a message does not fit, the oldest waiting messages toward that peer are dropped,
  so a peer that catches up keeps the newest; other peers are not affected, and the keep-alive
  exchange repairs what a dropped message carried. A peer that stays at the limit for a full minute is
  disconnected and resynchronizes when it reconnects. A peer that keeps up never reaches the limit.
  Messages are also sent in the order they were queued, where a backlog used to go out newest first.
  The first drop toward a peer logs one warning with its peer ID, then at most one summary line per
  minute. Nothing new is sent on the wire. More in ch.28 §28.9B.

- **A taker now keeps trying to send its swap payment until the same moment the reference does.** The
  taker gave up on sending its payment one third of the swap's lock duration after the start. It now uses
  the maker-payment wait deadline, two fifths of the lock duration, which is the same deadline it already
  uses to wait for the maker payment's confirmations. A taker whose maker payment confirmed late used to
  end the swap at this step, losing its dex fee and leaving the maker's payment locked until its refund.
  The maker's own deadlines do not move. Detail: PR #126; CRD ch.51 R29.
- **An ARRR swap that fails on a lightwalletd server without an address index now says so.** When the server's node refused an address lookup ("No information available for address"), the error blamed a missing transaction index, which is a different setting. For example, a maker payment failed with that explanation on a server whose node runs with the transaction index on. Such an answer now ends the lookup at its first request. The error says the server's node keeps no address index and quotes the node's own message. Detail: PR #118; CRD ch.39 R39.11.4i(e).
- **A swap that resumes after a restart no longer gives up on a payment it already sent.** Suppose a node restarted after broadcasting its swap payment but before recording it, and came back after that payment's send deadline. The swap checked the deadline first and ended as "payment failed", so the payment stayed on chain with no refund until someone ran `recover_funds_of_swap` by hand. The swap now first looks for a payment it may already have sent and carries on with it. The deadline stops only a payment that has not been sent yet. This applies to maker and taker, for every coin. Detail: PR #119; CRD ch.51 R17, R29, T21.
- **An ARRR swap that restarts finds its own earlier payment without asking a server.** Before sending
  its HTLC payment, a swap asks whether that payment is already on chain. The answer now comes first from
  the wallet's own record of the transactions it broadcast. This works even when the configured
  `lightwalletd` servers cannot answer address questions. The record only ever answers "sent"; anything
  else still goes to the servers. Detail: PR #125; CRD ch.39 R39.11.26.
- **ARRR swap recovery and spend detection no longer hang on a lightwalletd server whose node keeps no address index; such a server is skipped.** Two servers in the published ARRR server list answer every address lookup with "No information available for address". We took each of those refusals for one transaction the server could not deliver, and narrowed the search down to single blocks to step around it. For a swap a few weeks old that meant tens of thousands of requests per server, with nothing logged, so `recover_funds_of_swap` seemed never to return. Any activation that listed such a server before a working one was affected. Now:
  - such a server is recognized after a few requests, logged once as a warning, and skipped for the rest of the activation;
  - each server gets at most 120 seconds per check;
  - the search starts at the payment's own block;
  - each step is logged.

  `recover_funds_of_swap` now returns within 10 minutes for every coin. If it stops, its error names the step it was in, and for ARRR also the server it was reading. Detail: PR #117; CRD ch.39 R39.5.5, R39.11.4h, R39.11.4i.
- **The per-coin `estimate_gas_mult` setting in the coins file is now honored for the ERC-20 approval a swap sends.** Some layer-two coins (the public coins file sets it to 1.3 on its Arbitrum and Base entries) ask for the gas the node estimates for the approval to be multiplied, and a token takes its platform coin's value unless it sets its own. We ignored it, so an approval could run out of gas on those chains. The setting is read when the coin is activated (anything other than a number stops the coin from activating and names the key) and applied only to that approval's estimated gas, rounded down; the fixed gas limits and the trade-fee quote are not scaled. Detail: PR #107; CRD ch.57 R57.5.3b.
- **Enabling a UTXO coin no longer fails or stalls when the first listed Electrum servers are down and a
  later one is up.** Every server gets its turn within the call's time budget. More in ch.38 §38.6.4.
- **Electrum change notifications are no longer dropped.** A server's message that an address or the
  chain tip changed was read as an answer to no request and discarded, so balance streaming and
  history refresh never saw it; it now reaches the waiting watcher, also after the connection that
  carried the subscription was replaced. More in ch.38 §38.6.4 and §38.6.5.
- **A market maker's keep-alive is now sent once per trading pair, as other nodes expect.** A maker
  with orders on several pairs used to send, every 30 seconds, one message per pair that each listed
  every pair, so each node that relays the order book asked for every pair once per copy. Seed nodes
  answering those requests ran out of memory, and one pair they could not answer failed the whole
  request and stopped the maker's orders from spreading further. Each message now carries the one pair
  of the topic it is sent on, and pairs without orders get no message. This stops the flood of failing
  sync requests that overloaded seed nodes. Detail: CRD ch.32 R-K2a.

- **A node answers every order-book sync request at once and asks for one only when it needs it.** A
  node whose own order data was a few milliseconds behind used to refuse other nodes' sync requests
  with a "retry" answer; a relay that got that answer stopped passing the maker's orders on until the
  next keep-alive, 30 seconds later. The node now answers from the data it has, as other KDF nodes do,
  leaving out an order whose cancellation it is still applying. Before comparing a received keep-alive
  with its own data, it waits briefly for that data to catch up, so it no longer requests a sync it does
  not need. Detail: CRD ch.32 R-T3b items 2 and 4.

- **A maker node that is not a relay now keeps its order book of other makers up to date after a restart.** After a restart the node announced its stored orders again, but it did not record that it was following their trading pairs, so a keep-alive from another maker with a different order list never made it ask for that maker's orders, and its view of the other makers on its own pairs went stale until something else (an order book request, a new order on the pair) recorded the pair. Re-announcing a stored order now records the pair first, without requesting the order book. Detail: PR #101; CRD ch.59 R59.6.7.

- **Gossip message ids now use the compact 8-byte form other KDF nodes use, which should cut the bandwidth a relay spends on duplicate messages.** Our nodes announced each message with an id written as decimal text, while the other implementations write the same 64-bit value as 8 raw bytes, so an id announced by one side never matched the id the other side already held and every announced message was requested again, then dropped as a duplicate. The id is now the 8-byte big-endian form; what it is computed from is unchanged. Nodes on the old and new encoding still exchange messages normally, but each message is sent once more between them until both sides are upgraded. Detail: PR #99; CRD ch.28 R13.
- **A maker no longer advertises or serves a just-cancelled or just-repriced order from stale data.** For a few milliseconds after a cancel or a price change, the node's order index was already up to date while the data behind its keep-alive messages and its answers to other nodes' sync requests was not, so it could still announce the old state, and a node that had already seen the cancel could log "recently cancelled" when the old order was listed again. Keep-alives are now built only once both are up to date; until then the keep-alive is skipped for that round. Detail: PR #98; CRD ch.32 R-T3b.

- **A maker order for a shielded-coin pair (such as ARRR) no longer drops out of the maker's own order book every minute and a half and gets announced again.** Orders for those pairs are placed under a separate key of their own, and the cleanup that forgets silent peers treated that key as a silent peer, because the node never hears its own keep-alive back. The cleanup now leaves the node's own keys alone, so these orders stay put and the extra announcements stop. Detail: PR #94; CRD ch.59 R59.6.5.

- **An EVM node that answers with an error no longer ends the request; the next configured node is
  tried.** Before, the first node's answer was accepted even when it carried an error object, such as
  "block range too large" or a revert, so one picky node failed the call although the others could
  serve it. Now the nodes are tried one at a time, a refusing node counts as a failed node, and the
  node that finally answers is tried first the next time. When every node fails, the caller gets the
  last node's own error. More in ch.35 §35.1.F.

- **A swap or withdraw on an EVM coin no longer waits for all nodes to agree on the nonce.** One
  lagging node used to hold the transaction indefinitely. Now the highest pending nonce wins and the
  signed transaction is sent at once to every node that reported it; the send succeeds as soon as one
  of them accepts it. A transaction you broadcast yourself with `send_raw_transaction` is tried node
  by node instead. More in ch.35 §35.1.F.
- **An ARRR payment is no longer counted as confirmed one block early after the chain tip moves
  back.** Confirmations, and the decision that a transaction has expired, are now counted against
  the current tip, not against the highest one ever seen. A lagging server can now only make a wait
  longer, never shorter. Detail: PR #112; CRD ch.39 R39.11.10a.
- **The same lightwalletd server listed twice no longer counts as two votes.** When an ARRR node checks its
  `light_wallet_d_servers` against each other, a server listed twice, also under a different spelling
  such as with or without `https://` or the default port, now counts once. Before, `[A, A, B]` let A
  outvote B with only two real servers. Each dropped duplicate is logged. Detail: PR #113; CRD ch.39
  R39.11.7b.
- **The legacy `electrum` method now honors `min_connected` and `max_connected`.** Before, both were
  ignored and every listed server was connected; with `max_connected: 1` only one server is connected
  now, and unusable values (zero, a minimum above the maximum, a wrong type, an empty server list) are
  refused with the usual error answer, as on the newer activation methods. This also applies to BCH
  and QRC20 activated through `electrum`. More in ch.55 R55.4.3 and ch.38 R38.6.4.

- **Reading the order book of a pair that includes an ARRR order no longer fails on the older `orderbook` method.**
  Before, one ARRR order on the book made the whole older `orderbook` request fail with an internal
  error, and the older `best_orders` silently left the order out. The order now appears with the
  address `Shielded` (the same word the newer interface uses), and an order whose coin has no address
  to show is left out on its own instead of failing the response. Tendermint, Siacoin and Lightning
  orders now show up on the older methods too. Detail: PR #88; CRD ch.32 R-F6d, R-F6e.

- **PIVX and LBRY (LBC) swaps can now complete.** Their block headers are now read correctly. Before,
  a PIVX swap failed at the final step, after the taker had already paid, because the headers it needs
  to set the spend's time lock could not be read; LBC had the same gap. PIVX headers carry an extra field
  only in some chain versions (the headers are 80 or 112 bytes depending on the version), and LBC
  headers always carry one (112 bytes); both are now read exactly, as stored and as served. A PIVX
  header of version 7 is no longer misread. Detail: PR #82; CRD ch.37 R37.5.5.
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
  one instance per seed, as the known limitations already advise. Detail: PR #87; CRD ch.39 R39.8.0ag1.
- **Recovering an ARRR swap no longer fails with "does not pay the HTLC script".** Every ARRR swap
  uses its own key for the swap's payment, but `recover_funds_of_swap` checked the payment with the
  wallet's permanent key, so recovery always stopped with that error, even for a payment that had
  long been refunded. Recovery now tries the swap's own key first and the wallet's permanent key
  second, so swaps saved by older or other builds are recovered too. The log, and any error, say
  which key matched. A refunded payment is reported as refunded, naming the refund transaction; a
  payment that is still waiting is refunded; and the swap completes if the other side spent it. The
  error, when it is genuine, now says both keys were tried and shows the transaction id the way
  explorers do. Detail: PR #109; CRD ch.39 R39.5.4.
- **One malformed transaction no longer stops a coin's transaction history.** A transaction whose
  input points past the end of the previous transaction's outputs, a BCH transaction with no
  outputs, or an SLP token whose creation carries no decimals made the history task panic, and the coin's history stopped updating until a restart.
  Such a transaction is now logged and skipped, and the rest of the history keeps updating.
  Detail: PR #84; CRD ch.38 R38.5A.9.
- **Unconfirmed BCH and SLP transactions in the history now get their block height once mined.**
  The history compared the hash in two different forms that never matched, so a BCH or SLP
  transaction first seen unconfirmed stayed unconfirmed in the history. Detail: PR #84;
  CRD ch.38 R38.5A.10.

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
  which changes no coin in the public coins file. Detail: PR #66; CRD ch.60 R60.6.1, ch.38 R38.6.6e–f.
- **`min_trading_vol`, `setprice`, `buy`, `sell` and `trade_preimage` no longer crash the request for
  Lightning, Solana or SPL coins.** Lightning now reports its minimum trading volume and minimum
  transaction amount as the reference does: the largest counterparty minimum HTLC over its open
  channels, or 1 msat. Orders and trade previews involving Lightning, Solana or SPL, which cannot
  trade yet, are refused with an error naming the coin; `trade_preimage` returns the new typed error
  `TradingNotSupported` (HTTP 400). Detail: PR #66; CRD ch.60 R60.6.5, ch.41 R41.11A.
- **RIC (Riecoin) and MEWC (Meowcoin) swaps could not complete the HTLC spend.** Their Electrum
  servers serve headers the standard 80-byte reader could not lay out (RIC: 112 bytes; MEWC: native
  blocks at 120 bytes interleaved with merged-mined ones at 80), so every header list was rejected,
  and with it the median-time-past the spend queries before it signs. A new per-coin key,
  `protocol.header_layout`, beside `type` in the coin's entry, selects the layout:
  `"riecoin"` (112 bytes: 64-bit time, 256-bit nonce) or `"kawpow_auxpow_mixed"` (per header: AuxPoW
  flag bit 8 set reads the standard 80 bytes, clear reads the 120-byte KawPoW form). The
  median-time-past is then taken from each parsed header's time, and the full served bytes are what
  is kept. An unrecognized value fails the coin's activation. A coin with no key is read as before
  and, only when the standard read fails, retried as Riecoin and as the mixed layout (the one exact fit wins, one `info` line per coin naming the layout and suggesting the key); the new
  `MM2.json` switch `header_layout_fallback_families` (array of distinct `chain_variant` names,
  default `["BTC"]`; `[]` = reference behavior; anything else refuses the launch) names the
  families that may do so. A list that both alternatives consume exactly is rejected as ambiguous,
  naming both layouts, until the coin is pinned with `header_layout`.
  RIC and MEWC swaps therefore complete with the shared coin registry untouched; the registry
  maintainer may still add the key (`"riecoin"`, `"kawpow_auxpow_mixed"`) to pin the layout, and
  other implementations ignore it. The header-sync (SPV) and header-store paths still read standard
  headers. Detail: PR #62; CRD ch.37 §37.5.9.
- **An EVM gas price is no longer floored at +1 gwei.** The proportional increase added a fixed
  gigawei whenever its own increment was smaller, so sends and swap fee quotes on sub-gigawei
  chains (Ethereum today, Arbitrum, Base, BNB) overpaid or overquoted by up to hundreds of times,
  and the trade-preimage path applied the floor twice. The increase is now purely proportional.
  Detail: CRD ch.57 R57.3.2; PR #59.
- **UTXO refund polling no longer waits a full hour when the lock-time is nearer.** The refund
  eligibility check floored its "wait this long" answer at one hour, so a refund that became
  spendable in minutes was re-checked an hour later. The wait is now capped at one hour instead;
  nothing signed changes and no funds were at risk. Detail: PR #60; CRD ch.37 §37.6 finding.
- **A UTXO coin whose `signature_version` is not recognised now fails to activate instead of
  signing with a different algorithm, unless the new node-level boolean
  `coin_conf_unrecognised_enum_falls_back` is `true`.** The accepted values are `base`,
  `witness_v0`, `fork_id` and `fork_id_rxd`; any other value used to be silently replaced by the
  coin's default, so its transactions were signed wrongly and rejected by the network. RXD's
  `fork_id_rxd` was such a value until the Radiant signature hash was implemented (see Added).
  This is a deliberate divergence from GLEEC KDF, which falls back silently. By default
  activation now reports the offending value and the accepted set; with the switch set to `true` the
  coin activates with the default, as under GLEEC KDF, and the node logs one warning per activation
  naming the key and value. A coin with no `signature_version` is unchanged. The switch is named for
  coin-configuration enumerations in general; `signature_version` is its only reader so far.
  Detail: PR #54.
- **A malformed or non-string `fork_id` in a UTXO coin's configuration now fails the activation
  instead of crashing the node or taking the default.** A string such as `"0xZZ"` used to panic; a
  number such as `64` was silently replaced by the coin's default. An absent `fork_id` is unchanged,
  and the new switch does not apply: a malformed value is not an unrecognised enumeration member.
  Detail: PR #54.
- **A hostile or corrupt Electrum header response can no longer crash the node.** A header (or
  header list) whose length fields asked for gigabytes of memory could abort the process or kill the
  sync task, for any UTXO coin. The Equihash solution, Qtum's block signature and AuxPoW merkle
  branches now have length limits, and an over-long one is rejected as a malformed header like any
  other. Detail: PR #52; CRD ch.37 R37.5.6a.
- **A maker that goes offline now drops out of the orderbook on schedule, whatever its clock says.**
  The keep-alive expiry used the timestamp the maker put in its own message, so a maker with a fast
  clock stayed listed for that skew plus 90 s after it died, and one that wrote a far-future
  timestamp stayed listed indefinitely. The expiry now runs from when this node received the
  keep-alive (CRD ch.32 R-P3); the wire format is unchanged. Keep-alives also no longer advertise
  empty trie roots. Detail: PR #52.
- **A ZCash-family (ARRR) light-server endpoint whose block cache is stale is no longer used.**
  An endpoint can report the current chain height and the right branch while its cache of blocks
  stopped much earlier, so it cannot serve anything newer; it is now excluded at admission, as is
  one whose cache answers with an error or whose height is more than 40 blocks behind the others.
  A failed admission now lists every excluded endpoint and why, and its message no longer claims
  that an endpoint is "not on this coin's chain". Detail: PR #51; CRD ch.39 R39.11.7a(j).
- **A second ARRR withdrawal no longer fails with `bad-txns-duplicate-nullifier-requirements-not-met`
  because the first one's notes were still unspent.** `withdraw` and `task::withdraw` are unchanged
  (the client still broadcasts), but when the client submits the transaction through
  `send_raw_transaction` the notes it spends are now reserved until it is mined and scanned, or
  expires. A withdrawal that is built and never submitted reserves nothing. `withdraw` with `max`
  now reports an insufficient balance instead of an internal error when the spendable balance does
  not cover the fee. Two daemons on one seed with separate wallet databases are not protected.
  Detail: PR #49; CRD ch.39 D39.8.0e.
- **`enable_erc20` now works on an HD wallet.** The one-shot call refused every HD-derived EVM
  platform with `UnexpectedDerivationMethod`, so a client that knows only this call could not add an
  EVM token after startup in HD mode. It now activates the token on the platform's enabled address
  and returns one `balances` entry per known address — the same response shape as before, and
  byte-identical on a single-address platform. A `path_to_address` selecting another address is
  still refused, as is a hardware-backed platform, and only `task::enable_erc20` reports progress.
  The MM2.json boolean `erc20_hd_activation` set to `false` (default `true`) restores the former
  refusal. Detail: PR #28; CRD ch.35 R35.2.5.
- **Swaps with a shielded coin such as ARRR now report their outcome to the network like every other
  swap, and the report no longer names the traders.** When an older-style swap ends, whatever the
  outcome, the node publishes the usual `swapstatus` summary that statistics nodes collect; before,
  it stayed silent whenever a shielded coin was involved. The summary leaves out both parties' public
  keys (the sender's own key too, when the swap used a one-off key), and a statistics node now fills
  in only the key of the party that sent the report. More in
  [Legacy swap state machine](docs/reloaded-rewrite/51-legacy-v1-swap-state-machine.md) (R50A). Detail: PR #108.
- **A shielded (ARRR) wallet that met a reorg of its own scan tip no longer wedges forever.** Every
  sync pass failed with `Compact block hash discontinuity` and the wallet never rewound. The scan
  now re-checks its anchor against lightwalletd on every pass, rewinds within the commitment-tree
  checkpoints the wallet retains, clears compact blocks cached above the rewound height, and, for a
  divergence deeper than it can rewind, rebuilds at the wallet's sync start rather than at height 0.
  A compact cache that fails validation is deleted instead of renamed to `.invalid-chain.<ms>.bak`,
  which also ends the multi-gigabyte backup flood. No RPC or wire change. Detail: PR #31; CRD ch.39
  R39.8.0al and R39.8.0n.
- **The swap gas-fee policy now takes effect.** `set_swap_gas_fee_policy` stored a policy and
  `get_swap_gas_fee_policy` echoed it, but nothing read it, so every swap transaction was signed in
  the legacy form and `High` behaved exactly like `Legacy`. `Low`, `Medium` and `High` now emit an
  EIP-1559 (type 2) transaction priced from the matching tier of the same fee estimate the
  fee-estimation RPC reports, and the send fails rather than falling back to the legacy form if no
  estimate can be obtained. `Legacy` stays the built-in default, for peers on a reference build at
  or before `v2.0.0-beta` that cannot read a typed transaction — but the shipped coins file puts
  ETH, MATIC, AVAX, ETH-ARB20, ETH-BASE and MOVR on `"Medium"`, so swaps of those coins and of
  their tokens now emit type 2 unless the policy is overridden at activation or with
  `set_swap_gas_fee_policy`. Not yet verified against a live network send. Detail: PR #25.
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
- **EIP-1559 swap transactions are no longer priced from a stale base fee.** The fee estimate took
  the base fee of the oldest block in the fee-history window instead of the newest, so under a
  rising base fee `max_fee_per_gas` could land below the actual block base fee and the send was
  rejected; this affects the coins the shipped coins file puts on a non-legacy tier, since those
  price swaps from these estimates. An `eth_feeHistory` response that omits `baseFeePerGas`, which a
  conforming client may do, could also panic inside a swap send path and no longer can. Detail: PR #48.
- **A swap on a merged-mined (AuxPoW) coin could be neither completed nor refunded once the chain
  changed its block version.** The header reader recognized AuxPoW only by two exact version values,
  so Syscoin headers at version `0x20100180` (from about height 1,320,000) were read as plain
  80-byte headers and every header list was rejected. That broke median-time-past, which every HTLC
  spend and refund needs, so a swap that reached the payment stage was stuck. SYS, NMC, PEP, DINGO,
  MYDOGE, UNO, DOI, SHIC, TRMP and XMY were affected; DOGE worked only because its version equals
  one of the two values. AuxPoW is now recognized by the version's flag bit 8, per block, as the
  protocol defines it. Balances, history, deposits and plain withdrawals were never affected. Detail:
  PR #55; CRD ch.37 R37.5.2, R37.5.4.

- **A node no longer keeps memory for every swap it has ever run, and stopping it no longer waits for the swap bookkeeping.** Each finished swap left a small record behind for as long as the node ran (about 3 KB for a maker or taker swap in total), so a node that runs many swaps grew slowly without bound. The record of a running swap is now dropped as soon as the swap ends, on every way it can end, together with the messages kept for it. When the node is told to stop, it clears this list at once instead of waiting for it to empty. Detail: PR #100; CRD ch.51 R58A, R58B.

- **EVM swaps now use the gas limits from your coins file instead of one fixed number.** The coins file can set per-coin limits (`gas_limit` for the HTLC calls, sends and refunds, `gas_limit_v2` for the newer swaps), and a token inherits its platform coin's block. We ignored them and used 150000 for every swap call, which is too low on some chains, so swap transactions on chains such as Base and Arbitrum could fail on-chain. Limits are now read when the coin is activated and used for every swap call, the trade-fee estimates and the approval; a value that is not a whole number stops that coin from activating and names the key. Where the coins file sets nothing, the defaults now match the other KDF implementations: for example a platform-coin payment is 65000 and its refund 100000, so the trade fee shown for a platform coin goes down. Detail: PR #105; CRD ch.57 R57.5, R57.6.

### Plans touched

Plans this cycle moved, in any sense; each plan file carries the detail.

- [`arrr-htlc-electrum-fallback`](docs/plans/arrr-htlc-electrum-fallback.md) — new, in progress:
  ARRR swaps whose `lightwalletd` servers lack an address index get their HTLC answers from the
  wallet's own send record and, as a fallback, from Electrum; a taker that no source can watch is
  refused before its fee.
- [`arrr-native-and-ironwood-pool`](docs/plans/arrr-native-and-ironwood-pool.md) — new, in planning:
  ARRR native mode, the Ironwood pool and Pirate1 addresses for v0.4.0; native mode is refused in this
  release.

- [`solana-support`](docs/plans/solana-support.md), [`tron-swap-support`](docs/plans/tron-swap-support.md),
  [`tron-gasfree`](docs/plans/tron-gasfree.md),
  [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md) and
  [`trading-floors-parity`](docs/plans/trading-floors-parity.md) — each notes that its family is
  switched off (TRON, Solana) or wallet-only (Tendermint) for this beta, and that turning it back on
  belongs to the family's plan.

- [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md) — the
  withdraw path now records the transaction when the client submits it, and the plan moved to in progress (PR #49); the shortfall it causes is now named (PR #50). The test harness for the locked-notes check landed
  (PR #71); the only open item is two daemon instances sharing one wallet.
- [`arrr-ironwood-compatibility`](docs/plans/arrr-ironwood-compatibility.md) — the reorg-wedge fix
  landed (PR #31); the lightwalletd transport moved to tonic 0.14 on hyper 1 (PRs #37, #42);
  endpoints that have stopped following the chain are now excluded (PR #51); and activation warns
  when the default recent scan window applies (PR #61).
- [`v0.2.0-dependency-hygiene`](docs/plans/v0.2.0-dependency-hygiene.md) — the rustls, webpki, ring
  and tungstenite rows cleared, and the advisory ledger is green with one dated acceptance, the
  `crypto-bigint` timing advisory RUSTSEC-2026-0387 (PRs #37–#40, #42, #44, #68).
- [`walletconnect-fork-upgrade`](docs/plans/walletconnect-fork-upgrade.md) — superseded: the fork
  bump it proposed gave way to a `[patch]` onto upstream `tokio-tungstenite-wasm` 0.4.0 (PR #40).
- [`secp256k1-migration`](docs/plans/secp256k1-migration.md) — its last follow-up closed: the real
  WASM link step is back in CI and no longer `continue-on-error` (PRs #26, #27).
- [`clippy-ci-gate`](docs/plans/clippy-ci-gate.md) — the gate shipped; the vendor-filter follow-up
  stays open (PR #27).
- [`evm-test-infrastructure`](docs/plans/evm-test-infrastructure.md) — opened, and two dead-network
  EVM tests ported to a scripted local node (PR #30); tests against a local `geth` node now run in
  CI (PR #53).
- [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md) — steps 2–4 done:
  two mainnet test vectors sourced and the Radiant signature hash implemented (PR #65).
- [`utxo-header-family-selection`](docs/plans/utxo-header-family-selection.md) — Riecoin and
  Meowcoin swaps now complete: a per-coin `header_layout` key and a fallback switch landed (PRs
  #62, #67), and the layout reaches every header read (PR #74). The AuxPoW coins also left the set
  whose median-time-past errors (PR #55).
- [`trading-floors-parity`](docs/plans/trading-floors-parity.md) — opened as a beta blocker: CRD
  ch. 60 binds every coin family's trading floors, and the maintainer chose full parity plus a local
  pre-trade spend check (PR #69). The `lightning-activation-features`, `solana-support`,
  `tron-swap-support` and `evm-min-trading-volume` plans gained pointers to it.
- [`evm-min-trading-volume`](docs/plans/evm-min-trading-volume.md) — its substance shipped: the
  minimum trading volume now matches the reference for EVM, QRC20, TRON, UTXO and Lightning coins
  (PR #66).
- [`sia-withdraw-fee-control`](docs/plans/sia-withdraw-fee-control.md) — moved from parked to done:
  a Siacoin `withdraw` now honors a custom fee (PR #77), and a fee below the node's estimate was
  checked live on an uncongested transaction pool (2026-10-26).
- [`order-posting-latency`](docs/plans/order-posting-latency.md) — opened: order posting is slowed by
  the Electrum servers' per-session limit of about one request per second. The rules for batching,
  fee reuse, read spreading and balance polling are written (PR #95).

### Known limitations

What does not work yet, or works only in part, in this release. Each item says where more detail
lives.

- **Do not run two KDF instances on the same seed at the same time.** If you do, for any coin (RIN
  included), both instances can pick the same funds for two different transactions. The network
  accepts one and rejects the other, and the instance that loses gets no explanation and may try
  again. KDF guards against this inside a single instance only; shielded coins such as ARRR are the
  known case. More in [`shielded-in-flight-note-locking`](docs/plans/shielded-in-flight-note-locking.md).
- **ARRR (Pirate) atomic swaps work only between two KDF Reloaded nodes.** ARRR now uses a newer
  transaction format (v6, from the Ironwood upgrade) that no other KDF implementation can handle, so a
  swap with a Komodo or GLEEC counterparty cannot complete. Spending a swap's locked ARRR is under
  live testing. More in [`arrr-ironwood-compatibility`](docs/plans/arrr-ironwood-compatibility.md).
- **Siacoin (SC) swaps are not available.** Sending, receiving, the balance and the history work.
  Atomic swaps of SC are postponed until after the beta; the design is still a draft. More in the
  [Siacoin swap draft](docs/reloaded-rewrite/54-sia-swap-v2-path.md).
- **Radiant (RXD) is only partly usable, and the swap watcher does not work yet.** RXD activates and
  signs, but swaps with it are not established. The swap watcher, which finishes a swap for you when
  you are offline, does not work yet (issue #56). More in
  [`rxd-signature-version-support`](docs/plans/rxd-signature-version-support.md).
- **TRON (TRX, TRC20) and Solana (SOL, SPL) cannot be activated in this beta.** Their support is not
  finished, so activation answers a clear error instead. Both are planned to return in the next beta.
  More in [`tron-swap-support`](docs/plans/tron-swap-support.md) and
  [`solana-support`](docs/plans/solana-support.md).
- **Tendermint (Cosmos) coins are wallet-only in this beta.** Activation, balances and withdrawals
  work; orders and swaps are planned to return in a later release. More in
  [`tendermint-activation-features`](docs/plans/tendermint-activation-features.md).

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
- **Small KMD direct-burn DEX fees retain the legacy wire shape (superseded,
  see above).** Netid 8762
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
  takers now use the `v2.6.0-beta`-compatible discounted fee and dust-aware
  fee/OP_RETURN split (see the corrected three-range entry above for the
  exact structure), while non-KMD takers remain single-output.
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
- **Background retry and diagnostic noise hardening.** Persistent QRC20 history
  failures now back off from 10 seconds to a bounded five-minute retry interval
  and reset after recovery. Native Electrum reconnect loops retain the first or
  changed endpoint error at `ERROR` while identical retries move to `DEBUG`;
  redundant rustls handshake/alert internals are omitted from the application
  log while the endpoint-specific KDF error remains. Optional
  `eth_feeHistory`, late request-response completions, routine peer exchange,
  and duplicate peer-exchange dial diagnostics no longer appear as operator
  errors. Code: `mm2src/coins/`, `mm2src/common/`, `mm2src/mm2_p2p/`.

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

Tag `v0.2.0-beta.1` (2026-08-05) was cut from `staging`, which has since diverged from the
development line; it has no section in this file, and neither `0.3.0-alpha` milestone descends
from it.

[Unreleased]: https://github.com/kdf-reloaded/kdf/compare/ddb87a4849f91400b1b4aa120090f1acb12aa7bf...HEAD
[0.3.0-alpha.3]: https://github.com/kdf-reloaded/kdf/compare/49a46bc54fc16761796ed0bad53a472b8183ed9b...ddb87a4849f91400b1b4aa120090f1acb12aa7bf
[0.3.0-alpha.2]: https://github.com/kdf-reloaded/kdf/compare/7a18f94b2f5de1ff7ba2ae69c4b445132d305a3f...49a46bc54fc16761796ed0bad53a472b8183ed9b
[0.3.0-alpha.1]: https://github.com/kdf-reloaded/kdf/compare/v0.1.0-beta.2...7a18f94b2f5de1ff7ba2ae69c4b445132d305a3f
[0.1.0-beta.2]: https://github.com/kdf-reloaded/kdf/compare/v0.1.0-beta.1...v0.1.0-beta.2
[0.1.0-beta.1]: https://github.com/kdf-reloaded/kdf/releases/tag/v0.1.0-beta.1
