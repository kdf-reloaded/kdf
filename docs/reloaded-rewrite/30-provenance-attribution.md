# Chapter 30 — Provenance and Attribution Index

**Status:** driving-spec (meta-chapter).

This chapter is the cross-cutting index over the rest of the document set. It
binds the per-chapter capsule index, the aggregated input register, the
per-substrate reverse map, and the discipline by which the document set
composes as a single attribution record.

## 30.1 Executive Summary

The substantive driving-spec chapters of this document set each carry, in
their own provenance footers, a record of the materials they were checked
against and the substrate they bind. Read in sequence those chapters describe
the project's complete delta from the baseline tree; read individually each
records where its content came from.

This closing chapter does not add substantive substrate of its own. It is a
cross-cutting index over the rest of the set, intended to make three classes
of question fast to answer:

- *Which chapter covers a given subsystem or substrate?* — answered by the
  per-subsystem reverse map in §30.5.
- *Which external specifications, wire formats and sibling repositories
  does the document set cite?* — answered by the aggregated input register
  in §30.4, which routes the reader back to the chapter that carries the
  authoritative citation.
- *What is the per-chapter capsule?* — answered by the one-row-per-chapter
  index in §30.3.

The index is not authoritative on any individual claim it summarises. The
authoritative statement for any individual claim is the chapter that makes
it; this index is a lookup table. Where the index appears to disagree with a
chapter, the chapter is right and the index is the defect.

## 30.2 Subsystem Shape

The index is a meta-chapter. It has three navigational tables — per-chapter
capsule, aggregated input register grouped by external-specification family,
per-subsystem reverse map — plus four binding rules that govern the
document set as a whole (R1–R4) and a single deferred item recording the
audit-tooling gap (D1).

The reverse map is keyed by the substrate names bound in the chapters
themselves (see Chapter 27 for the infrastructure substrate inventory), not
by source-tree paths. This is deliberate: chapters bind contract surface,
not file layout, and the index must reflect what they bind.

## 30.3 Per-Chapter Capsule Index

The columns are: chapter number, title, the substrate(s) the chapter binds,
and a one-sentence capsule of what the chapter documents.

| #  | Title                                                                                                | Substrate(s) bound                                                                                       | Capsule                                                                                                                                                                  |
| --: | ---------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 00 | [Overview and Purpose](00-overview.md)                                                               | (meta)                                                                                                   | States what the document set is, who it is for, and how it is structured.                                                                                                |
| 01 | [Clean-Room Rules and Methodology](01-clean-room-rules.md)                                           | (meta)                                                                                                   | The normative rules: permitted inputs, forbidden inputs, identifier hygiene, citation discipline, chapter shape.                                                         |
| 02 | [Baseline State](02-baseline-state.md)                                                               | (anchor)                                                                                                 | Records the exact baseline commit and the inherited tree at that commit.                                                                                                 |
| 03 | [Toolchain Modernisation](03-toolchain-modernization.md)                                             | toolchain pin, unstable-feature avoidance                                                                | Migration to stable Rust and removal of post-stabilisation unstable features.                                                                                            |
| 04 | [Error-Aggregation Type Adaptation to the Modern Trait Solver](04-error-aggregation-type-adaptation.md)             | error-envelope substrate, explicit-lift helper                                                           | Adaptation of the error framework to the modern trait solver while preserving wire shape.                                                                                |
| 05 | [Hierarchical-Deterministic Wallet Support](05-hd-wallet-support.md)                                                         | BIP-32/39/43/44 derivation, mnemonic primitives, dual-curve key paths                                    | The hierarchical-deterministic derivation stack and encrypted-mnemonic primitives.                                                                                       |
| 06 | [Network-ID and Network-Configuration Registry](06-network-id-seed-node.md)                                    | network-config accessor surface, seed-node registry                                                      | The pluggable per-netid configuration substrate and the removal of hard-coded seed addresses.                                                                            |
| 07 | [Wallet Lifecycle and Key Export](07-wallet-lifecycle-and-key-export.md)         | named-wallet store, startup handshake, three management RPCs                                             | Named wallets, encrypted mnemonic file format, create/list/delete RPCs, startup verification handshake.                                                                  |
| 08 | [Atomic-Swap Fee-Routing Engine](08-fee-routing-engine.md)                                           | `DexFee` typed descriptor, `compute_dex_fee`, `ValidateFeeArgs`                                          | The typed fee descriptor, the arithmetic-only producer, and the struct-arguments swap-trait boundary.                                                                    |
| 09 | [Third-Party Swap-Watcher Infrastructure](09-watcher-reward-infrastructure.md)                                        | swap-watcher protocol, `swpwtchr/<ticker>` topic, watcher state machine                                  | The third-party-watcher protocol, its gossipsub topic naming, and the bound watcher state machine.                                                                       |
| 10 | [Server-Sent-Events Streaming Substrate](10-sse-streaming.md)                                                                 | event-streamer registry, SSE backbone                                                                    | The push-mode Server-Sent-Events substrate and its streamer registry.                                                                                                    |
| 11 | [Order-Match Cancellation Race Mitigation](11-order-match-cancellation.md)                                      | 120-second recently-cancelled cache, per-orderbook isolation                                             | The time-bounded recently-cancelled cache that resolves gossipsub message reordering across order cancellation.                                                          |
| 12 | [Maker-Order State Store](12-order-match-state-store.md)                                             | maker-order context, TTL backing, ticker reverse indices                                                 | The maker-orders context store, its TTL backing, and the ticker reverse indices.                                                                                         |
| 13 | [Atomic-Swap Version Negotiation](13-swap-version-negotiation.md)                                           | `SwapVersion` numeric tag, element-wise-min negotiation                                                  | The version tag carried on order-request and order-reservation messages and the pair-negotiation rule.                                                                   |
| 14 | [Generic State-Machine Runtime](14-state-machine-runtime.md)                                                 | persistent state-machine runtime, transition discipline                                                  | The generic persistent state-machine runtime carved out of the legacy pattern module.                                                                                    |
| 15 | [Atomic-Swap Version-Two UTXO Path](15-swap-v2-utxo-path.md)                                                         | V2 UTXO two-stage taker funding, dual-secret maker HTLC                                                  | The V2 UTXO two-stage taker funding specification and the dual-secret maker HTLC.                                                                                        |
| 16 | [Atomic-Swap Version-Two Pre-Burn Output](16-swap-v2-pre-burn-output.md)                                             | split-DEX-fee on-chain shape                                                                             | The split-DEX-fee specification with a burn share and a residual fee share.                                                                                              |
| 17 | [Atomic-Swap V2 EVM Path & Contract Interaction](17-swap-v2-evm-path.md)                                                           | EVM V2 maker/taker contract pair, reveal-on-spend flow                                                   | The EVM V2 contract pair and the reveal-on-spend flow.                                                                                                                   |
| 18 | [Tendermint, IBC, and Cross-Chain HTLC Surfaces](18-tendermint-ibc-htlc.md)                          | Tendermint HTLC dialects, IBC `MsgTransfer` envelope, CW20-style support                                 | The Tendermint HTLC dialects, the IBC transfer envelope, and CW20-style token support.                                                                                   |
| 19 | [NFT Module Layout](19-nft-module-layout.md)                                                         | EVM-NFT trait surface, storage abstraction, metadata-refresh path                                        | The EVM-NFT trait surface, the storage abstraction, and the pluggable metadata-refresh path.                                                                             |
| 20 | [Siacoin Network Integration](20-siacoin-integration.md)                                                     | Sia spend-policy substrate, atomic-swap policy, walletd-backed account model                             | The Sia atomic-swap spend policy and the walletd-backed account model.                                                                                                   |
| 21 | [Tron Integration](21-tron-integration.md)                                                           | Tron withdraw-only path, Base58Check, protobuf transactions, bandwidth-and-energy fees                   | Withdraw-only Tron support: Base58Check addresses, protobuf transactions, the bandwidth-and-energy fee model.                                                            |
| 22 | [WalletConnect v2](22-walletconnect-v2.md)                                                           | WalletConnect v2 relay client, x25519+HKDF+ChaCha20-Poly1305 transport, session persistence              | The WalletConnect v2 relay client, the keyed transport, and the session-persistence stores.                                                                              |
| 23 | [External Trading-API Client](23-trading-api-client.md)                                                       | external-DEX-aggregator typed HTTP client                                                                | The typed HTTP client for the external DEX-aggregator API: quote / build-tx / tokens / portfolio request and response types.                                              |
| 24 | [GUI-Facing Account-State Persistence](24-gui-account-state.md)                                             | GUI account-state schema, three identity variants, eleven management RPCs                                | The named-account storage layer (Iguana / HD / hardware-wallet variants) and its dual-backend persistence.                                                               |
| 25 | [Single SQLite Gateway Substrate](25-sql-query-builder.md)                                             | async SQLite façade, typed query-builder DSL, pragma helpers                                             | The async SQLite façade, the typed query-builder DSL, and the pragma helpers.                                                                                            |
| 26 | [Cross-Platform Build Substrate and WebAssembly Adaptation](26-cross-platform-and-wasm.md)                                             | build-target matrix, native/WASM gating macros, dual storage and transport, dual entry-point pattern    | The build-target matrix, the platform-guard macros, dual storage and transport, and the dual entry-point pattern.                                                        |
| 27 | [Infrastructure Subsystem Inventory](27-infrastructure-crate-carve-outs.md)                          | (registry of cross-cutting infrastructure substrates)                                                    | The substrate inventory referenced by the rest of the document set: thirteen cross-cutting infrastructure substrates.                                                    |
| 28 | [P2P Substrate Consolidation](28-libp2p-modernization.md)                                             | P2P-substrate consolidation, composed network behaviour, relay-mesh extension                            | The consolidation of the baseline P2P substrate, the composed network behaviour, and the relay-mesh extension.                                                           |
| 29 | [Treatment of License Conditions (e) and (f)](29-license-conditions-e-f.md)                          | (legal position)                                                                                         | The project's legal position on the additional copyright-holder conditions appended after the baseline date.                                                              |
| 30 | [Provenance and Attribution Index](30-provenance-attribution.md)                                     | (meta — index)                                                                                           | Cross-cutting index.                                                                                                                                                     |
| 31 | [Central Application-Context Substrate](31-central-application-context.md)                            | central-context substrate (`MmCtx` / `MmArc` / `MmWeak`, `Constructible<T>` once-set fields, sub-context slot pattern, `MmCtxBuilder`, FFI registry, stop signal) | The shared application-context substrate every consumer crate fetches its shared state through.                                                                          |
| 32 | [Orderbook Patricia-Trie and P2P Surface](32-orderbook-p2p-and-trie.md)                              | orderbook Patricia-trie substrate, keep-alive root advertisement, sync-delta request surface             | The per-pubkey/per-pair Patricia-trie orderbook substrate and its P2P root-advertisement plus delta-synchronisation surface.                                          |
| 33 | [Bound Swap V2 P2P Wire Schema (B-gleec Embedding)](33-swap-v2-wire-schema-embedding.md)            | swap-v2 protobuf wire schema (`swap_v2.proto`)                                                           | Verbatim embedded wire descriptor for swap-v2 P2P messages, with fresh CRD commentary binding interoperability constraints.                                            |
| 34 | [Provenance Ledger](34-provenance-ledger.md)                                                          | (meta — companion ledger)                                                                                | Per-file classification and source/constraint ledger companion referenced by the chapter set.                                                                           |
| 35 | [EVM (Ethereum) V2 Activation & Token RPC Surface](35-evm-v2-activation-rpcs.md) | EVM platform-coin-with-tokens activation, ERC-20 child-token RPCs, swap gas-fee policy, foreign-transaction byte decoding | The platform-coin-with-tokens activation surface for EVM chains and the ERC-20 token RPCs layered on it, plus the two EVM coin-layer contracts the chapter carries: the per-coin swap gas-fee policy and the EIP-2718 contract for reading a counterparty's serialized transaction. |
| 36 | [Tendermint (Cosmos) V2 Activation & Token RPC Surface](36-tendermint-v2-activation-rpcs.md) | Tendermint platform-coin-with-tokens activation, IBC and native token RPCs | The platform-coin-with-tokens activation surface for Cosmos-SDK chains and their IBC and native asset RPCs. |
| 37 | [UTXO SPV & Block-Header Validation](37-utxo-spv-and-block-header-validation.md) | SPV activation configuration, block-header storage, header proof-of-work validation | Simplified payment verification for UTXO coins: header storage, validation against proof-of-work, and per-coin SPV configuration. |
| 38 | [UTXO Coin Maintenance & Features](38-utxo-coin-maintenance-and-features.md) | UTXO feature surface (as-built and deferred split) | A broad UTXO feature surface, split between capabilities verified present and those recorded as gaps. |
| 39 | [Zcash / z_coin Shielded Coin](39-zcash---z_coin-shielded-coin.md) | shielded-coin type, Native and Light activation modes, lightwalletd sync, shielded HTLC | The shielded-coin substrate: dual activation modes, multi-lightwalletd light mode, the task-RPC trio, and shielded HTLCs. |
| 40 | [Solana Coin](40-solana-coin.md) | Solana platform coin, SPL child tokens | Solana as a platform coin carrying SPL token children, following the platform-coin-with-tokens pattern. |
| 41 | [Lightning Network](41-lightning-network.md) | Layer-2 node over a parent UTXO coin, channel and BOLT-11 payment surface | Lightning as a Layer-2 coin over a parent UTXO coin: task activation, peer and channel management, BOLT-11 payments. |
| 42 | [Price Service & Makerbot](42-price-service-and-makerbot.md) | makerbot start/stop RPC pair, per-pair maker configuration registry, periodic price refresh | The market-maker bot and price service: its RPC pair, per-pair configuration registry, and periodic price-fetch-and-refresh loop. |
| 43 | [Daemon, RPC Dispatcher & Misc Maintenance](43-daemon-rpc-dispatcher-and-misc-maintenance.md) | local HTTP JSON-RPC endpoint, legacy and versioned request shapes, method classification | The single local JSON-RPC endpoint, the two accepted request shapes, and the classification of methods by exposure. |
| 44 | [Database Persistence and Schema Migrations](44-database-persistence-and-migrations.md) | native SQLite database files, migration contract, persisted event vocabularies | The native SQLite persistence contract: database files, schema migrations, and the persisted swap event vocabularies. |
| 45 | [Startup Configuration and Environment Tolerance](45-startup-configuration-and-environment-tolerance.md) | environment-variable configuration surface, startup tolerance rules | What the daemon must tolerate at startup when driven by environment variables plus a minimal runtime configuration. |
| 46 | [Sia Coin V2 (Task-Based) Activation RPC Surface](46-sia-v2-activation-rpcs.md) | Sia standalone-coin task activation | Sia as a standalone coin activating through the shared standalone-coin task-activation mechanism. |
| 47 | [MetaMask (Browser EIP-1193 Wallet) Integration](47-metamask-integration.md) | EIP-1193 browser-wallet surface, WASM target | The browser-injected EIP-1193 wallet integration and the surfaces it targets. |
| 48 | [Platform-Coin Task-Activation Framework](48-platform-coin-task-activation.md) | task-based platform-coin-with-tokens activation | The long-running task form of platform-coin-with-tokens activation, shared by the EVM and Tendermint families. |
| 49 | [Withdrawal task path](49-withdrawal-task-path.md) | direct `withdraw` method, `task::withdraw::*` family | The withdrawal JSON-RPC surface: the direct method and the long-running task family beside it. |
| 50 | [EVM Trezor / hardware-wallet transaction signing](50-evm-trezor-signing.md) | device-held EVM account keys, activation-time address and public-key learning | EVM transaction signing under a hardware-wallet policy, where the account key never leaves the device. |
| 51 | [Legacy (V1) Atomic-Swap State Machine](51-legacy-v1-swap-state-machine.md) | legacy maker and taker stage graphs, negotiation refusal contract, per-stage reserved-funds semantics | The legacy V1 maker and taker state machines: stages, events, resume mapping, the refusal contract, message budgets, and reserved-funds rules. |
| 52 | [Version-Two Atomic-Swap State Machine](52-swap-v2-state-machine.md) | V2 maker and taker state graphs, bidirectional refusal contract, reserved-amount ledger semantics | The V2 maker and taker state machines: states, events, resume mapping, the refusal and abort contract, message budgets, and reserved-funds rules. |
| 53 | [Siacoin Transaction History](53-sia-transaction-history.md) | walletd address-event projection, coin-generic runtime history store, Sia transaction-type wire values | Sia transaction history as a projection of walletd's per-address event log into the shared transaction-details record, served through the framework's history RPC surface. |
| 54 | [Siacoin Atomic-Swap Version-Two Path](54-sia-swap-v2-path.md) | Sia V2 swap path over the coin-generic V2 trait surface | DRAFT, not yet approved for implementation: binds the coin-generic V2 trait surface to Siacoin's V1 primitives and the shared V2 state-machine substrate; closes chapter 20 §20.10 D5. |
| 55 | [Coin Activation Wire Contract](55-coin-activation-wire-contract.md) | coin-activation RPC error envelope, task-framework method errors, per-family unresponsive-backend outcomes, baseline activation families | The cross-family wire contract of coin activation, and the full contracts of the activation families that no other chapter owns. |
| 56 | [EVM EIP-1559 Fee-Per-Gas Estimation](56-evm-eip1559-fee-estimation.md) | `eth_feeHistory` base-fee anchor selection, EIP-1559 max-fee and priority-fee estimate, headroom over the selected base fee, typed fee-cap-below-base-fee rejection, fee fields of a gas-limit estimation request | How a type-2 fee-per-gas estimate is derived from an `eth_feeHistory` response: the dictated array ordering that makes the trailing element the freshest base fee, the obligation to anchor `maxFeePerGas` on that value rather than on a stale one, the node-dictated rejection of a cap below the base fee and its typed classification, and the fee fields an estimation request must carry. |
| 57 | [EVM Fee Inputs: Legacy Gas-Price Resolution, Per-Coin Gas Limits, and the Withdraw Fee Form](57-evm-fee-inputs.md) | legacy gas-price resolution, the proportional-increase arithmetic, per-coin `gas_limit` / `gas_limit_v2` / `gas_price_adjust` configuration blocks, the withdraw fee form and envelope-type derivation | How an EVM transaction's gas price and gas limit are resolved: the two-source maximum, the no-absolute-floor increase arithmetic, the per-coin override blocks and their defaults, and the withdraw fee object that decides the signed envelope type. |
| 58 | [UTXO `signature_version` Contract and the Radiant Sighash](58-utxo-signature-version-and-radiant-sighash.md) | the `signature_version` / `fork_id` configuration contract, per-value sighash selection, the Radiant fork-id preimage | The closed set of signature-version values a UTXO coin configuration may name, the algorithm each selects, and the Radiant dialect in full — a BIP-143 preimage with one inserted per-output summary digest. |
| 59 | [Legacy Order-Match Negotiation Loop and Dead-Order Defences](59-legacy-ordermatch-negotiation.md) | the four-message V1 negotiation exchange, taker-side reservation selection, negotiation timeouts, the peer penalty, keep-alive liveness | The legacy negotiation loop: collect-then-choose taker selection, at-most-once maker reservation, the two timeout arms, the one-hour penalty on a counterparty whose negotiation fails, and liveness kept on the receiver's own clock. |
| 60 | [Per-Coin Trading Floors: Minimum Trading Volume, Minimum Transaction Amount, and Where They Refuse or Fail a Trade](60-trading-floors-and-refusal-contract.md) | per-family `min_trading_vol` / `min_tx_amount` values, order-builder default-and-refusal arithmetic, the maker's volume test on requests, the DEX-fee floor both peers compute, coin-layer spend refusals | The two per-coin trading floors for every family, where the order-match and swap layers consume them, and every point at which a floor, a dust limit or a deducted fee makes a peer refuse a trade or a started swap fail. |

## 30.4 Aggregated Input Register

This register lists the categories of external input the document set cites,
with the chapters in which they appear. It is sorted by category. It does
not duplicate the citations themselves; each listed chapter carries its own
*External References* section that is the authoritative form of the
citation.

### 30.4.1 BIP / SLIP family

- Hierarchical-deterministic-wallet BIPs and SLIPs (BIP-32 family,
  SLIP-0010, SLIP-0044, and adjacent documents) — cited in Chapter 05.
- The mnemonic-encoding BIP (BIP-39) — cited in Chapter 05 and
  referenced from Chapter 07.
- SLIP-0021 — referenced from Chapter 07 as the seed-bootstrapped
  alternative to password-based key derivation.

### 30.4.2 EIP family

- Token standards (ERC-20, ERC-721, ERC-1155) — cited in Chapters 17 and 19.
- Typed structured-data signing (EIP-712) — cited in Chapter 17.
- Related ERC and proxy/permit standards as required for the V2 contract
  pair and the NFT inventory — cited in Chapters 17 and 19.

### 30.4.3 IBC / Cosmos family

- Inter-Blockchain Communication core packet types, including the fungible-
  token `MsgTransfer` envelope — cited in Chapter 18.
- Tendermint HTLC dialects — cited in Chapter 18.

### 30.4.4 IETF / W3C standards

- Server-Sent Events (W3C) — cited in Chapter 10.
- HKDF (RFC 5869), ChaCha20-Poly1305 (RFC 7539), Curve25519 / x25519
  (RFC 7748), Argon2 (RFC 9106), and adjacent RFCs governing the
  WalletConnect v2 transport and the mnemonic-encryption format — cited
  in Chapter 22 and Chapters 05 / 07.

### 30.4.5 libp2p protocol family

- Gossipsub, floodsub, request-response, ping, noise, mplex, the DNS and
  WebSocket transports, secp256k1 keys — cited in Chapter 28, with
  topic-naming consumers in Chapters 09 and 11.

### 30.4.6 Per-chain protocol references

- Bitcoin Script and the UTXO HTLC construction — cited in Chapters 15
  and 16.
- Solidity contracts published on public blockchains and their ABIs —
  cited in Chapter 17.
- Tendermint ABCI / Cosmos SDK message types — cited in Chapter 18.
- Sia spend-policy semantics — cited in Chapter 20.
- Tron transaction protobuf, TAPOS, and TRC-20 — cited in Chapter 21.
- Zcash shielded-transaction primitives — referenced from Chapter 29.
- Komodo consensus: KIP-0001 and the public Komodo daemon's
  active-user-reward rule — cited in Chapter 38 (§38.4, §38.10).

### 30.4.7 External APIs and wire-format counterparties

- WalletConnect v2 protocol (relay role) — cited in Chapter 22.
- External DEX-aggregator API — cited in Chapter 23.
- Hardware-wallet wire protocol and protobuf schemas — referenced from
  Chapter 05 and Chapter 27.
- The public Komodo DeFi Framework API reference (coin-activation, task-managed
  activation, common-structures and error-types pages) — cited in Chapters
  35, 36, 39, 40, 41, 46, 48 and 55.

### 30.4.8 Sibling open-source repositories

Sibling repositories under licenses compatible with the project's
licensing intent are governed by Chapter 01 (permitted inputs) and by the
internal allow-list maintained alongside the project's clean-room rules.
That allow-list is not part of the published document set; it is reviewed
by the chapter author at draft time. Chapters that quote an allow-listed
identifier name the identifier and cite the sibling repository in their
own *External References* section.

The chapters most likely to invoke that allowance are those that define
data shapes exchanged with a graphical client — Chapters 22, 23, 24 in
particular.

### 30.4.9 Baseline tree

The single largest input cited by every chapter is the baseline tree at
project baseline commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`. It is
the inherited corpus on which every documented change builds; Chapter 02
describes its contents in detail. Where a later chapter says "at the
baseline X was Y", the X is resolvable by checking out the baseline
commit and inspecting the tree.

### 30.4.10 Behavioural observation

Chapter 01 admits observable behaviour of the live peer-to-peer mesh and
of public blockchains as a permitted input. Two chapters cite it
explicitly:

- Chapter 11 cites the observed message-ordering behaviour of the live
  gossipsub mesh as the motivation for the recently-cancelled time cache.
- Chapter 13 cites the observed presence of mixed-version peers as the
  motivation for the swap-version tag.
- Chapters 08, 16 and 38 cite the issue #11 KMD mainnet observations
  (the on-wire legacy taker-fee split and the network's rejection of
  over-claimed KMD rewards).

### 30.4.11 Ledger companion artifact

- Chapter 34 carries the per-file provenance ledger companion used by
  the chapter set; it references and operationalises the rule framework
  bound in Chapter 01.

## 30.5 Per-Substrate Reverse Map

The reverse map answers "which chapter(s) bind the substrate that owns this
concern". It is keyed by substrate name rather than by source-tree layout.
Substrate names are drawn from Chapter 27's infrastructure inventory and
from the substrate vocabulary bound in the substantive chapters themselves.
Where a chapter touches a substrate peripherally rather than as primary
subject, the citation is marked *(ref)*.

| Substrate                                                              | Chapter(s)                                          |
| ---------------------------------------------------------------------- | --------------------------------------------------- |
| coin-handle abstraction (top-level)                                    | 13 *(ref)*, 15, 16, 17, 18, 19, 20, 21, 29          |
| EVM coin substrate                                                     | 17, 19, 21, 35 *(ref)*, 56, 57                      |
| EVM V2 swap contract pair                                              | 17                                                  |
| Tron coin substrate                                                    | 21                                                  |
| NFT substrate                                                          | 19, 25 *(ref)*                                      |
| Sia coin substrate                                                     | 20                                                  |
| Tendermint coin substrate                                              | 18                                                  |
| UTXO coin substrate                                                    | 15, 16, 37 *(ref)*, 38 *(ref)*, 58                  |
| Zcash-family coin substrate                                            | 29                                                  |
| coin-activation substrate                                              | 18 *(ref)*, 35, 36, 39, 40, 41, 46, 48, 55          |
| shared-utility substrate (Chapter 27 §27.x)                            | 03 *(ref)*, 14, 26, 29                              |
| ref-counter diagnostics substrate                                      | 27                                                  |
| HD / mnemonic crypto substrate                                         | 05, 07                                              |
| SQL substrate                                                          | 25                                                  |
| enum-conversion derive substrate                                       | 27                                                  |
| serialization-error derive substrate                                   | 04, 27                                              |
| hardware-wallet abstraction substrate                                  | 27                                                  |
| UTXO-primitives substrate family                                       | 15 *(ref)*                                          |
| WalletConnect-protocol substrate                                       | 22                                                  |
| Ledger-protocol substrate (scaffolding only)                           | 27                                                  |
| platform entry-point substrate                                         | 26                                                  |
| central-context substrate                                              | 31, 27 *(ref)*, 10 *(ref)*, 14 *(ref)*, 18 *(ref)*, 19 *(ref)*, 22 *(ref)*, 24 *(ref)*, 25 *(ref)*, 28 *(ref)* |
| IndexedDB / browser-storage substrate                                  | 26                                                  |
| error-envelope substrate                                               | 04, 27                                              |
| EVM-utility substrate                                                  | 26 *(ref)*                                          |
| event-streamer substrate                                               | 10, 27                                              |
| GitHub-client substrate                                                | 27 *(ref)*                                          |
| GUI-storage substrate                                                  | 24                                                  |
| file-IO substrate (native-only)                                        | 26                                                  |
| application-root substrate                                             | 07, 08, 09, 10, 11, 12, 13, 14, 17, 29              |
| node-init substrate                                                    | 06, 29                                              |
| order-matching substrate                                               | 11, 12, 13 *(ref)*, 59, 60                          |
| orderbook Patricia-trie + orderbook sync substrate                     | 32, 59 *(ref)*                                      |
| atomic-swap substrate                                                  | 08, 09, 13, 15, 16, 17, 29, 33, 51, 52, 59, 60      |
| wallet-lifecycle substrate                                             | 07                                                  |
| RPC-dispatcher substrate                                               | 10 *(ref)*, 27 *(ref)*                              |
| browser-wallet-bridge substrate                                        | 05 *(ref)*, 26                                      |
| metrics substrate                                                      | 27                                                  |
| HTTP / WebSocket / gRPC-web transport substrate                        | 26                                                  |
| network-config substrate                                               | 06, 08, 28 *(ref)*, 29                              |
| high-precision-numerics substrate                                      | 27                                                  |
| P2P-networking substrate                                               | 06 *(ref)*, 09 *(ref)*, 11 *(ref)*, 28              |
| RPC-data-types substrate                                               | 27                                                  |
| state-machine substrate                                                | 14                                                  |
| proxy-signature substrate                                              | 27, 28                                              |
| long-running-task substrate                                            | 27                                                  |
| external-DEX-aggregator client substrate                               | 23                                                  |
| hardware-wallet wire-protocol substrate                                | 05 *(ref)*, 27                                      |

The reverse map is a navigation aid. A substrate may legitimately be
touched by changes that no chapter discusses individually (test
scaffolding, dependency adjustments, configuration follow-ups); the
absence of a chapter listing here means the document set does not single
that substrate out for treatment, not that the substrate is untouched.

Substrates that exist in the workspace but are not given a row of their
own include: the renamed Bitcoin-primitive substrate family (referenced
collectively on the UTXO-primitives row); the vendoring crates used as
direct-dependency replacements; the test-helper substrates; and the
preserved-for-history substrate retained for git-archaeology continuity
(see Chapter 28).

## 30.6 Document-Set Composition (binding rules)

**R1.** *Per-chapter self-containment.* Each substantive chapter (03
through 28, 31 through 33, and 35 through 60) MUST
satisfy the project's chapter-shape rules (Chapter 01 §6): executive
summary, subsystem shape, the binding-rule sections appropriate to the
chapter's substrate, tests / deferred work / external references /
baseline verifications, and a provenance footer. The four-section
post-substantive tail (tests, deferred, external refs, baseline
verifications) MUST appear on every driving-spec chapter. A reader who
picks up a single chapter alone MUST be able to verify that chapter's
attribution without reading the rest of the document set. Chapter 54
is DRAFT and not yet approved for implementation (see its own status
line); it is listed in §30.3 for V2's completeness check but its
content is not yet a binding driving specification until its status
changes.

**R2.** *Whole-delta coverage.* The substantive chapters as a set MUST
describe the entire substrate delta from the baseline tree. Behaviours
present in the project that are not derivable from the baseline tree plus
the published-spec inputs cited by some chapter MUST either prompt
drafting a new chapter (Chapter 01 §8) or have the corresponding source
brought back to a derivable state. Un-derivable substrate is not allowed
to accumulate without an explicit chapter recording the gap.

**R3.** *Index non-authority.* This chapter (Chapter 30) is a lookup
table and MUST NOT be treated as authoritative on any claim it
summarises. Where this index disagrees with a chapter it indexes, the
chapter is correct and this index is the defect.

**R4.** *Substrate-keyed navigation.* The per-substrate reverse map in
§30.5 MUST be keyed by substrate name (the contract surface the chapter
binds), not by source-tree path. New chapters that bind additional
substrate add a row here; refactoring source layout that does not
introduce or remove substrate MUST NOT require any change to this
chapter.

## 30.7 Tests / Audits

The index has no executable test invariants of its own. The audit
discipline that protects it is:

- *Capsule consistency audit.* Re-reading each chapter's executive
  summary against the corresponding row in §30.3 MUST be performed when
  any chapter's executive summary is materially edited; row and summary
  MUST agree.
- *Citation-presence audit.* Each external-reference category named in
  §30.4 MUST have at least one citation in the chapter(s) the row
  references. A category whose chapters drop all citations of the
  referenced family MUST be removed from §30.4.
- *Substrate-naming audit.* Substrate names used in §30.5 MUST appear
  somewhere in the chapter(s) listed for that row. The audit is a simple
  cross-grep over the chapter text for the substrate name.

## 30.8 Deferred Work

**D1.** A machine-checked audit script that performs the three audits
listed in §30.7 against the working tree on each pre-commit boundary is
deferred. The audits are currently performed manually at chapter-edit
time.

This single deferred item is also the consolidated hand-off point every
other chapter's own automated-tooling gap resolves to when it names
"chapter 30 D1" or "chapter 30's audit-tooling-gap binding" rather than
tracking a separate open item of its own: chapter 00's D1 (automated lint
enforcement of its R5/R6 per-chapter-shape rules), chapter 01's D1
(automated linting of R11 identifier hygiene, R13–R15 citation discipline,
R16/R19/R20 per-chapter shape, the §1.4A documentary checks T1–T3, and
the R35 residual-similarity gate),
chapter 02's D1 (a mechanical verification harness re-deriving the R4
workspace-member registry, the R5 patched-dependency registry, and the
§2.8 module layout from the anchor commit), and chapter 34's D2 (automated
enforcement of its own three §34.6 ledger audits) are folded into this
same deferred tooling effort rather than tracked as five separate open
items. None of the five constituent automated checks exist yet; all are
performed manually at the relevant chapter's or the ledger's edit time in
the interim, and a future implementation of this item MUST cover all five
before being considered complete.

**D2.** A glossary chapter consolidating substrate names introduced
across the document set is deferred. The per-substrate reverse map in
§30.5 is currently the working substitute.

**D3.** A reading-order appendix recommending entry points by reader role
(implementer, reviewer, integrator) is deferred. Chapter 00 currently
carries the high-level entry-point guidance.

## 30.9 External References

This chapter is itself an index and does not introduce new external
citations. The chapters indexed in §30.4 and §30.5 carry their own
*External References* sections; those are the authoritative citations.

The two documents this chapter does cite are internal to the document
set:

- Chapter 01 (clean-room rules and methodology) — the methodology against
  which every other chapter is read.
- Chapter 02 (baseline state) — the description of the inherited corpus
  every chapter's delta is computed against.

## 30.10 Baseline Verifications

**V1.** The baseline commit hash cited in §30.4.9 MUST match the hash
recorded in Chapter 02. The two values are bound to be identical.

**V2.** The chapter-count covered by the per-chapter capsule index
(§30.3) MUST equal the number of files at `docs/reloaded-rewrite/NN-*.md`
in the working tree. A simple `ls | wc -l` over the chapter directory
MUST agree with the row count of §30.3.

**V3.** Each chapter title in §30.3 MUST match the first-level `#`
heading of the chapter file it links to. A `head -n 1` of every chapter
file MUST agree with the row title here.

## 30.11 Provenance Footer

- *Inputs:* every other chapter of this
  document set in the working tree at project
  baseline commit `c1d46c0c1592faa0860f704008b2b2381bc3840f`. The
  per-chapter capsule summaries paraphrase the executive summaries of
  the chapters they point to; the input register groups citations by
  category against the *External References* of those chapters; the
  per-substrate reverse map was assembled by scanning the same chapters
  for the substrate names they bind.
- *Permitted-input classes used:* the document set itself.
- *Sibling-allowlist consultations:* none.
- *Forbidden corpus:* not consulted.
