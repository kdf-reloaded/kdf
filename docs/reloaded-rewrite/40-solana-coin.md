# Chapter 40 -- Solana Coin

**Status:** driving-spec (reloaded's own activation surface is primary; the reference surface of §40.1-§40.3 is bound as an additional compatibility surface, deferred; the remaining sections document shipped reloaded behaviour).

> **Availability in the beta (maintainer decision 2026-10-26).** Solana
> support is not finished, so it is switched off for the coming beta: while
> the build constant `SOLANA_ACTIVATION_AVAILABLE` (`coins/lp_coins_ops.rs`)
> is `false`, `enable_solana_with_tokens` answers `PlatformCoinCreationError`
> (object `{ticker, error}`), `enable_spl` answers `Internal` (string), both
> HTTP 500 with "Solana support is not available in this build", and the
> legacy `enable` / `electrum` answer the same text (Chapter 55 R55.4.7);
> nothing is registered, and the coin code stays compiled with its unit tests
> running. Solana is postponed, not dropped: it is planned to return in the
> next beta. "Shipped" behaviour in the status line and in §40.4-§40.6
> describes the code with the constant set to `true`; the 2026-10-11
> maintainer decision below stands for when Solana returns.

> **One-sentence claim:** the project shall support Solana as a platform coin and
> SPL tokens as its child tokens, activating them through the
> `experimental::enable_solana_with_assets` / `experimental::enable_solana_token`
> methods with request fields, success shapes, `error_type` discriminants,
> `error_data` shapes and HTTP statuses wire-identical to the reference of
> §40.0.1, querying balances and chain height over Solana's JSON-RPC, and
> exposing address/withdraw/transaction operations against the Solana and
> SPL-Token programs.

> **Maintainer decision (binding, 2026-10-11).** Reloaded's own Solana
> activation surface -- `enable_solana_with_tokens` and `enable_spl` with their
> current request, result and coin-configuration shapes (§40.6) -- is the
> **primary** contract of this project and stays routed. The reference
> implementation marks its Solana activation as experimental and its key-policy
> handling as not production-ready, so it is not adopted as the primary surface.
> Because backward compatibility with clients and coins files written for the
> reference matters, §40.1-§40.3 bind the reference's `experimental::` methods
> and its `SOLANA` / `SOLANATOKEN` coin-configuration forms as an **additional
> compatibility surface** to be added alongside the primary one, not instead of
> it. That work, the formal binding of the primary surface's own wire shapes,
> and the remaining Solana feature gaps are deferred to a dedicated session
> (D40.3, `docs/plans/solana-support.md`). Until then the §40.6 deviations are
> known gaps, not defects of this branch.

> **Treatment:** **T-PORT** for the activation wire contract of §40.1-§40.3
> (reloaded today ships a different, baseline-inherited Solana activation
> surface, described in §40.6); **T-DOC** for §40.4-§40.5.

> **Historical note (informative).** Solana support in this lineage was removed
> at one point as an unmaintained proof-of-concept and later reintroduced. The
> reintroduced reference implementation exposes its activation under the
> `experimental::` namespace with request and coin-configuration shapes that
> differ from the baseline-era surface still shipped by reloaded.

> **Binding scope (R36).** Requirements bind observable behaviour, the public
> activation RPC method strings and their request/response field names, the
> `error_type` discriminants with their `error_data` shapes and HTTP statuses,
> and externally *dictated* interop: the **Solana JSON-RPC** method set, the SPL
> **Token** and **Token-2022** program account model including associated token
> accounts, ed25519 keypairs, base58 address encoding, and lamport units. Private
> types and helper structure are informative. Human-readable `error` text is
> never bound.

> **Source of truth (informative).** The public Komodo DeFi Framework API
> documentation does not document Solana activation at all (the only trace is the
> protocol-name list inside a published parse-error example). By maintainer
> decision every wire shape in §40.1-§40.3 is therefore what the reference
> implementation of §40.0.1 does.

## 40.0 Executive Summary

Solana is integrated as a **platform coin** (SOL) that carries a set of **SPL
token** child coins. One request enables the platform plus an inline list of its
tokens; a second request enables one more token against an active platform.
Balances and chain height come from Solana's JSON-RPC; addresses and signing
follow Solana's ed25519 / base58 account model and the SPL Token programs.

The envelope, the adjacently tagged `error_type` / `error_data` error object and
the HTTP-status policy are bound for every activation family by
[Chapter 55](55-coin-activation-wire-contract.md) §55.1; the per-family
unresponsive-backend table is ch. 55 §55.3.

### 40.0.1 Reference version

R40.0.1 The reference implementation for this chapter's activation contract is
the GLEEC KDF release tag **`v3.0.0-beta`**. The netid-8762 legacy reference
`v2.6.0-beta` exposes the identical Solana activation methods, request fields,
results, coin-configuration protocol shapes and error mapping; there is no
reference-version or network dependence.

## 40.1 Activation methods and coin configuration

R40.1.1 The project shall route the mmrpc-2.0 methods
`experimental::enable_solana_with_assets` (platform with tokens, §40.2) and
`experimental::enable_solana_token` (single token, §40.3) on **all targets,
including WASM**. There is no task-managed Solana family.

> *Reloaded status (clean side, 2026-11-19):* both methods are routed on
> native builds (PR #426), next to `enable_solana_with_tokens` and
> `enable_spl` and behind the same switch: while
> `SOLANA_ACTIVATION_AVAILABLE` is `false` the platform method answers the
> `PlatformCoinCreationError` of T40.9 and the token method its `Internal`,
> each with "Solana support is not available in this build", and nothing is
> registered. WASM routing follows with the WASM package of
> `docs/plans/solana-support.md`. Both methods share the activation code of
> §40.3A: a platform activated through either surface takes tokens through
> either token method, and `disable_coin` treats it the same.

> *Reloaded status (clean side, 2026-11-19, browser builds):* both methods,
> and `enable_solana_with_tokens` and `enable_spl`, are routed in browser
> (WASM) builds as well (PR #430), with the same switch: while
> `SOLANA_ACTIVATION_AVAILABLE` is `false` each answers its T40.9 refusal
> there too instead of `NoSuchMethod`. The node client uses the HTTP
> transport shared by both targets, so requests, answers and host-only node
> names (R40.5A.1) are the same; the node pool of R40.1.5 runs unchanged.

R40.1.2 **Platform coin configuration.** A Solana platform coin's
coins-configuration `protocol` is `{"type": "SOLANA", "protocol_data": {}}`
(an empty `protocol_data` object). The platform's decimals are fixed at 9
(lamports).

R40.1.3 **SPL token configuration.** An SPL token's `protocol` is
`{"type": "SOLANATOKEN", "protocol_data": {...}}` with:

| `protocol_data` key | JSON type | Required? | Role |
| --- | --- | --- | --- |
| `platform` | string | **required** | ticker of the Solana platform coin |
| `decimals` | integer | **required** | token decimals |
| `mint_address` | string (base58 public key) | **required** | the token's mint account |
| `program_id` | string (base58 public key) | **required** | the token program that owns the mint (SPL Token or Token-2022) |

A `mint_address` / `program_id` that is not a valid base58 public key makes the
protocol unparsable (`TokenProtocolParseError`).

> **Reloaded status (clean side, 2026-11-15).** Implemented in PR #361, on
> native targets; WASM follows with the WASM package of
> `docs/plans/solana-support.md`. Both forms of R40.1.2 and R40.1.3 are read.
> Any object given as the platform's `protocol_data` is accepted and its
> members are ignored, as `{}` is.
> Reloaded also keeps reading its own forms: `{"type": "SOLANA"}` without
> `protocol_data` (and with `protocol_data: null`), and `SPLTOKEN` with
> `token_contract_address`, which means the SPL Token program. A Solana
> platform protocol is always written back as `{"type": "SOLANA"}`, the form
> of ch. 36 R36.0.3. Each token keeps its `program_id`, but the token's
> account, balance and transfers use it only from the Token-2022 package on.

> *Reloaded status (clean side, 2026-11-19):* browser (WASM) builds read
> every form above as native builds do (PR #430).

R40.1.4 **Keys and addresses.** The Solana keypair shall be the ed25519 keypair
whose 32-byte seed is the wallet's single-key (Iguana) secp256k1 private-key
secret; the platform address is the base58 encoding of its public key. Only the
single-key context is supported: an HD-wallet, hardware-wallet or WalletConnect
context fails platform activation (`PlatformCoinCreationError`). A token's
address is the associated token account of the platform address for
(`mint_address`, `program_id`), per the SPL associated-token-account derivation.

> **Reloaded status (clean side, 2026-11-16).** Resolved for Reloaded by the
> maintainer's decisions of 2026-11-15: a global-HD login activates with a
> key derived from the seed (§40.1A) instead of failing; a hardware-wallet
> policy is refused (R40.1A.6); no request selects WalletConnect, and its
> `priv_key_policy` form is an invalid request; a request for `IguanaPrivKey`
> under a global-HD login uses the wallet's single internal key (§40.1A,
> maintainer decision of 2026-11-17).
> The single-key rule above is unchanged. PR #392.

R40.1.5 **Node selection.** Each request that needs the chain selects, in list
order, the first node whose Solana JSON-RPC `getHealth` succeeds and uses it;
when none succeeds the operation fails (mapped per §40.2 / §40.3).

> *Reloaded status (clean side, 2026-11-19):* by the maintainer's decision of
> 2026-11-19 every Solana coin, whichever surface activated it, reaches its
> nodes through one pool (PR #426):
>
> - *Order.* The listed nodes are put into a uniformly random order once
>   when the coin is built, so the users of one coins file spread over its
>   nodes. The top-level `MM2.json` string `solana_node_order` set to
>   `listed` keeps the listed order instead; an absent key means `random`,
>   and any other value means `random` with one warning at activation naming
>   the key and the value.
> - *Health.* A node is asked `getHealth` before it is used when it has not
>   been checked in the last 30 seconds or its set-aside has just ended; a
>   failed check sets it aside like a transport failure. The reference asks
>   before every request.
> - *Set-aside.* A node is set aside for 20 seconds, doubling on each
>   further failure up to 12 hours, and a success resets it. Nodes are tried
>   in the coin's order, so a recovered node that ranks higher is used again.
>   When every node is set aside, each is tried once before the operation
>   fails.
> - *One operation, one node.* The reads of one withdraw, one balance read
>   and one broadcast each go to a single node; after a transport failure the
>   operation runs again on the next node. An activation runs its mint
>   checks and its balance and height reads as two such operations (the
>   token methods as three), which use the same node unless it fails in
>   between. A signed transaction that has been sent is never built again:
>   a failed broadcast is retried with the same signed bytes, and when every
>   node fails the error carries the transaction's signature.
>
> When no node answers, the failure maps as §40.2, §40.3 and §40.3A bind.

## 40.1A Keys under each login (Reloaded-original)

The reference derives no Solana key from an HD seed at any public state and
refuses a global-HD login (R40.6B.7). By the maintainer's decisions of
2026-11-15 Reloaded derives the key under a global-HD login, keeps the
single-key rule of R40.1.4 and refuses a hardware wallet.

**R40.1A.1** *Key source by login.*

| Login | Key |
| --- | --- |
| single-key (passphrase) | the key of R40.1.4; `hd_path_layout` and `path_to_address` are ignored |
| global-HD | the SLIP-0010 ed25519 key derived from the wallet's BIP-39 seed at the path of R40.1A.2; the passphrase-hash key is never used for Solana |
| global-HD, request with `priv_key_policy` `IguanaPrivKey` | the wallet's single internal key: the secret the HD login derives from the seed for the node's own use, which the UTXO family also takes for `IguanaPrivKey` under an HD login; the keypair is made from it as R40.1.4 makes it from the single key. It is not the key or address a single-key login with the same words gives. `hd_path_layout` and `path_to_address` are ignored |

**R40.1A.2** *Path.* The coin entry's `derivation_path`, then the account
level and, for the default layout, a change level; every level is hardened.

| `hd_path_layout` | Path | Example (account 0) |
| --- | --- | --- |
| `"bip44_change"` (default when absent) | `<derivation_path>/N'/0'` | `m/44'/501'/0'/0'` |
| `"bip44"` | `<derivation_path>/N'` | `m/44'/501'/0'` |

N is `path_to_address.account_id` (default 0); `path_to_address.chain`
must be `External` and `path_to_address.address_id` must be 0, because
neither layout has an address-index level. A coin entry without
`derivation_path` uses `m/44'/501'` (SLIP-0044 coin type 501).

**R40.1A.3** *No stored state.* The layout is a request parameter, like
`path_to_address`: the request's value, or the default when the member is
absent. Nothing is stored per wallet; the same request gives the same
address on every machine and in every session. A client that activated with
`"bip44"` must send `"bip44"` again on every later activation, or it gets the
default layout's address.

**R40.1A.4** *One seed phrase, three addresses.* The same words give three
different Solana addresses: used as a single-key passphrase (R40.6B.8), as an
HD seed with `"bip44_change"`, and as an HD seed with `"bip44"`. None of them
is wrong; a user who changes login or layout sees another address, and funds
stay at the address they were sent to.

**R40.1A.5** *Request members* on `enable_solana_with_tokens` (the
compatibility methods take the same members when they are added):

| Member | JSON type | Default | Under a single-key login |
| --- | --- | --- | --- |
| `hd_path_layout` | string, `"bip44_change"` or `"bip44"` | `"bip44_change"` | ignored |
| `path_to_address` | `{account_id, chain, address_id}` | account 0, `External`, 0 | ignored |
| `priv_key_policy` | object | the context key | as under HD |

**R40.1A.6** *Errors.*

| Case | `error_type` | `error_data` | HTTP |
| --- | --- | --- | --- |
| `hd_path_layout` other than the two values | `InvalidRequest` | string | 400 |
| `priv_key_policy` of a form this method does not accept (the WalletConnect form included) | `InvalidRequest` | string | 400 |
| `priv_key_policy` of a hardware wallet | `PlatformCoinCreationError` | `{ticker, error}` | 500 |
| under HD: `path_to_address` that does not parse, a `chain` other than `External`, or an `address_id` other than 0 | `PlatformCoinCreationError` | `{ticker, error}` | 500 |
| under HD: a `derivation_path` that is not a two-level hardened path | `PlatformCoinCreationError` | `{ticker, error}` | 500 |

A refused `priv_key_policy` is answered before any node is contacted.

**R40.1A.7** *Result.* `derivation_method` is `{"type": "HDWallet", "data":
"<full path>"}` under a global-HD login and `{"type": "Iguana"}` under a
single-key login and whenever the request asks for `IguanaPrivKey`
(R40.3A.2).

**T40.1A.1** *Vectors.* Mnemonic of eleven times `abandon` then `about`,
empty BIP-39 passphrase; computed outside this repository with an
independent SLIP-0010 implementation that reproduces the published
SLIP-0010 ed25519 test vector 1:

| Layout | Account | Address |
| --- | --- | --- |
| `bip44_change` | 0 | `HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk` |
| `bip44_change` | 1 | `Hh8QwFUA6MtVu1qAoq12ucvFHNwCcVTV7hpWjeY1Hztb` |
| `bip44` | 0 | `GjJyeC1r2RgkuoCWMyPYkCWSGSGLcz266EaAkLA27AhL` |
| `bip44` | 1 | `ANf3TEKFL6jPWjzkndo4CbnNdUNkBk4KHPggJs2nu8Xi` |

The same words as a single-key passphrase give
`7zuqksARN8uXHz2yvrkiZZDPeALBSjzK12nkMPNzKSCJ` (R40.6B.8), different from all
four.

**T40.1A.2** Every row of R40.1A.6; the refused `priv_key_policy` rows with no
node contacted.

**T40.1A.3** Under a single-key login, `hd_path_layout` and `path_to_address`
change nothing.

**T40.1A.4** `priv_key_policy` `IguanaPrivKey` gives `derivation_method`
`{"type": "Iguana"}` under either login. Under a single-key login the
address is the one of R40.6B.8's rule; under a global-HD login it is the
address of the keypair made from the wallet's single internal key, the same
secret the UTXO family uses there, and differs from R40.6B.8's address and
from the T40.1A.1 vectors.

> *Status (clean side, 2026-11-17):* by the maintainer's decision of
> 2026-11-17, every coin family is to honour an activation that asks for
> `IguanaPrivKey` under a global-HD login with the wallet's single internal
> key; for Solana, R40.1A.1, R40.1A.6, R40.1A.7 and T40.1A.4 state it
> (PR #392).

## 40.2 `experimental::enable_solana_with_assets`

R40.2.1 **Request fields** (flat in `params`; unknown fields ignored):

| Field | JSON type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | **required** | platform ticker (`SOLANA` protocol) |
| `nodes` | array of `{url}` | **required** (empty array is parsed, then fails per R40.2.4) | Solana JSON-RPC endpoints, tried in order (R40.1.5) |
| `nodes[].url` | string | **required** | must be an absolute URL; an unparsable URL is a request-parse failure (`InvalidRequest`, ch. 55 §55.1) |
| `tokens_params` | array of `{ticker, protocol?}` | **required** (may be empty) | SPL tokens to activate inline; `protocol` enables custom-token activation; other members ignored |
| `tx_history` | boolean | optional, default `false` | see R40.2.5 |

There is no `get_balances`: balances are always returned.

R40.2.2 **Activation semantics** (observable order; the first failure decides
the error): reject an already-active ticker; resolve the platform configuration
and protocol; build the platform without contacting a node (non-empty `nodes`,
supported key context, key derivation); resolve every token's configuration or
custom protocol and check that each token's `platform` equals the requested
ticker; initialise each token, which contacts a node (R40.1.5) and checks via
`getAccountInfo` that the mint account, if it exists, is owned by `program_id`
(a non-existent mint account is accepted); then read the SOL balance
(`getBalance`), the chain height (`getBlockHeight`) and each token balance; then
register the platform and tokens. No failure leaves anything registered.

R40.2.3 **Success result** (HTTP 200):

| Field | JSON type | Content |
| --- | --- | --- |
| `ticker` | string | platform ticker |
| `address` | string | base58 platform address |
| `current_block` | integer | chain block height (`getBlockHeight`), not the slot |
| `balance` | `{spendable: string, unspendable: string}` | SOL balance in SOL (9 decimals); `unspendable` is `"0"` |
| `tokens_balances` | object (map) | token ticker -> `{spendable, unspendable}` for every token activated in this call (`{}` when none); a mint unknown to the node reports `"0"` |

All balances are decimal strings.

> *Status note (Spec Reader round 0.5.0-20, 2026-11-21; passed the KDF
> Dirty Gate with §40.2A):* the form of these strings — zero, trailing
> zeros, large values, a token with 0 decimals — and the form of the amounts
> in a withdraw answer are stated in §40.2A. No sentence of R40.2.3 is
> changed.

R40.2.4 **Errors** (all mmrpc-level):

| `error_type` | Returned when | `error_data` | HTTP |
| --- | --- | --- | --- |
| `PlatformIsAlreadyActivated` | ticker already active, or registration race | string: ticker | 400 |
| `PlatformConfigIsNotFound` | no configuration entry for `ticker` | string: ticker | 400 |
| `CoinProtocolParseError` | platform `protocol` unparsable | `{ticker: string, error: string}` | 500 |
| `UnexpectedPlatformProtocol` | platform protocol is not `SOLANA` | `{ticker: string, protocol: object}` (ch. 36 R36.0.3 form) | 400 |
| `TokenConfigIsNotFound` | token without `protocol` not in configuration | string: token ticker | 400 |
| `TokenProtocolParseError` | token protocol unparsable | `{ticker: string, error: string}` | 500 |
| `UnexpectedTokenProtocol` | token protocol is not `SOLANATOKEN` | `{ticker: string, protocol: object}` | 400 |
| `CustomTokenError` | custom `protocol` supplied for a ticker present in the configuration | `{"DuplicateTickerInConfig": {"ticker_in_config": string}}` | 500 |
| `PlatformCoinMismatch` | a token's `platform` is not the requested ticker | absent | 400 |
| `Transport` | token initialisation: no healthy node, a node error while reading the mint account, or a mint owned by a program other than `program_id` | string | **502** |
| `PlatformCoinCreationError` | empty `nodes`; unsupported key context; key derivation failure; failure to read the SOL balance, the block height or a token balance | `{ticker: string, error: string}` (`ticker` = platform ticker) | 500 |
| `Internal` | internal failure | string | 500 |

**Unresponsive nodes.** When no node is healthy or every node fails:
- with at least one entry in `tokens_params`, the request fails during token
  initialisation with `Transport` (string), HTTP **502**;
- with an empty `tokens_params`, it fails at the balance read with
  `PlatformCoinCreationError` (`{ticker, error}`), HTTP **500**.

Both outcomes are bound; the split is a consequence of R40.2.2's order.

R40.2.5 **`tx_history`.** Solana transaction history is not implemented. A
request with `tx_history: true` shall not crash the process or the request
handler; the history flag shall have no effect and activation shall otherwise
proceed as with `false`.

> **Upstream divergence (informative).** In the reference, `tx_history: true`
> reaches an unimplemented path after the result has been computed and aborts
> handling of the request (no normal response). A crash cannot be a parity
> target; R40.2.5 binds the non-crashing behaviour.

> *Reloaded status (clean side, 2026-11-19):* built as R40.2.1-R40.2.5 bind
> (PR #426), with these additions of this repository's own:
>
> - the request also takes `hd_path_layout`, `path_to_address` and
>   `priv_key_policy` (R40.1A.5);
> - `tx_history` is read as an optional boolean and has no effect, so a
>   value of another type is `InvalidRequest` (HTTP 400, chapter 55
>   §55.1);
> - an inline `protocol` may also take Reloaded's own `SPLTOKEN` form, as a
>   coins-file entry may (status note after R40.1.3);
> - for a ticker present in the configuration, `CustomTokenError` is
>   answered before the inline `protocol` is parsed.
>
> The request has no commitment member, so a coin activated through this
> method sends no commitment level and the node's default applies. A token
> activated from an inline `protocol` is wallet-only. A wallet without a
> token account reads `"0"` (D40.6B.2 (a)).

## 40.2A The string form of balances and amounts (reference behavior)

> *Status note (Spec Reader round 0.5.0-20, 2026-11-21; passed the KDF
> Dirty Gate on 2026-11-21, measure 0.00; attestation in §40.12):* this
> section was written by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A, from four inputs: (1) the
> reference at the rows `v3.0.0-beta`, `v2.6.0-beta` and
> `main (as of 2026-10-07)` of chapter 02 §2.15A, which agree on everything
> below, so "the reference" means all three; (2) the published behavior of
> the public decimal-number library those rows pin, at the one version all
> three pin; (3) observations of the public Solana mainnet JSON-RPC endpoint
> made on 2026-11-21 (chapter 01 R6); (4) this chapter. No row was added to
> chapter 02 §2.15A. The section completes R40.2.3, R40.3.3, R40.4.1,
> R40.6B.9, R40.6B.10 and R40.6B.12 and edits none of them. Nothing of the
> reference is quoted; member names, `error_type` values and public JSON-RPC
> names are the interface. This note is the section's provenance record
> (section-scoped; §40.12 is unchanged).

*One term.* The **shortest form** of an amount is the decimal text of its
exact value with no zeros after the last non-zero decimal, no decimal point
when the value is whole, a single `0` before the point when the value is
below one, no exponent and no digit grouping. Zero is `"0"`. A negative
value has a leading `-`.

**R40.2A.1** *SOL balance.* The string is the exact value of the lamport
count divided by 10^9, in the shortest form. It is never padded to nine
decimals and never rounded, at any size: every lamport count a node can
report is rendered with all its digits.

| Lamports | String |
| --- | --- |
| 0 | `"0"` |
| 1 | `"0.000000001"` |
| 5000 | `"0.000005"` |
| 1000000000 | `"1"` |
| 1500000000 | `"1.5"` |
| 10000000000 | `"10"` |
| 12345678901234 | `"12345.678901234"` |
| 18446744073709551615 | `"18446744073.709551615"` |

**R40.2A.2** *SPL token balance.* The string is the node's own
`uiAmountString` member of the `getTokenAccountBalance` answer (R40.4.1),
passed on digit for digit. The reference does not compute it from the raw
amount and the decimals, and it neither trims, pads nor rounds it: zeros
the node sends after the last non-zero decimal would be kept. The form is
therefore the node's:

- *Observed.* The public mainnet endpoint on 2026-11-21 (a node reporting
  API version 4.4.0) sends the shortest form: for a raw amount of
  4572211441380 at 8 decimals it sent `"45722.1144138"`, and for a raw
  amount of 8799429046467041142 at 5 decimals `"87994290464670.41142"`,
  every digit kept.
- *Not observed in this round.* A zero amount and a token with 0 decimals.
  The published Solana node software forms the string the same way for
  both: `"0"` for zero whatever the decimals, and a whole number without a
  decimal point for a token with 0 decimals.
- On such a node the token string equals the shortest form of the raw
  amount divided by 10^decimals, the decimals being those of the mint.
- The token's configured `protocol_data.decimals` takes no part in the
  balance string. Where it differs from the mint's decimals the balance
  still shows the mint's scale, while a withdraw scales by the configured
  value (R40.6B.10).
- The zero of an unknown mint (R40.2.3) is `"0"` and does not come from a
  node string.

**R40.2A.3** *Where the form applies.* One balance read serves every
surface, so the same string appears in:

| Surface | Member |
| --- | --- |
| `experimental::enable_solana_with_assets` | `balance.spendable`; each `tokens_balances.<ticker>.spendable` |
| `experimental::enable_solana_token` | `balance.spendable` |
| `my_balance` | `balance` |

The unspendable member (`unspendable`, and `unspendable_balance` of
`my_balance`) is `"0"` for both kinds on every surface; it has no other
value. No surface pads or rounds differently from another.

**R40.2A.4** *Amounts in a withdraw answer.* All are exact; none is
rounded. Their form depends on how the member is made:

- *One converted figure: the shortest form.* `total_amount` (both kinds);
  `received_by_me` (the amount, or `"0"`); `spent_by_me` of an SPL
  withdraw; `fee_amount`; `rent_amount` (`"0"` when no account is created
  and in every SOL withdraw); the `total_amount` of `fee_details` in a SOL
  withdraw, which equals `fee_amount`.
- *A sum or a difference: not reduced.* It shows as many decimals as the
  operand that has more of them, so it can end in zeros and a zero result
  can read `"0.0"`. This applies to `spent_by_me` of a SOL withdraw
  (amount plus fee), to `my_balance_change` of both kinds, and to the
  `total_amount` of `fee_details` in an SPL withdraw (fee plus rent).

| Case | Member | String |
| --- | --- | --- |
| SOL, amount 1.5, fee 0.000005, to another address | `spent_by_me` / `my_balance_change` | `"1.500005"` / `"-1.500005"` |
| SOL, amount 0.999995, fee 0.000005, to another address | `spent_by_me` / `my_balance_change` | `"1.000000"` / `"-1.000000"` |
| SOL, amount 0.123456789, fee 0.000005, to the wallet itself | `received_by_me` / `spent_by_me` / `my_balance_change` | `"0.123456789"` / `"0.123461789"` / `"-0.000005000"` |
| SPL, amount 1.5, to another address | `my_balance_change` | `"-1.5"` |
| SPL, amount 1.5, to the wallet itself | `my_balance_change` | `"0.0"` |
| SPL, amount 0.25, to the wallet itself | `my_balance_change` | `"0.00"` |
| SPL, amount 2, to the wallet itself | `my_balance_change` | `"0"` |
| SPL, fee 0.000005, no account created | `fee_details.total_amount` | `"0.000005"` |
| SPL, fee 0.000005, rent 0.00203928 | `fee_details.total_amount` | `"0.00204428"` |

The amounts in the table are worked examples of the rule, not values taken
from a run or from any reference test.

*Error members.* `NotSufficientBalance` of a SOL withdraw: `available` and
`required` in the shortest form. Of an SPL withdraw: `available` is the
token balance string of R40.2A.2 and `required` the shortest form.
`NotSufficientPlatformBalanceForFee`: `available` is the SOL balance of
R40.2A.1 and `required` the fee-plus-rent sum, in the form of a sum. The
`threshold` of `AmountTooLow` is `"0.000000001"`.

**R40.2A.5** *Requirement.* On every surface, R40.3A.2 and R40.3A.4
included, this project shall render a SOL balance as R40.2A.1 states, and
a token balance as a string equal to the shortest form of the token
account's raw amount divided by 10^decimals of the mint, which is what
R40.2A.2 gives on today's nodes. No limit on significant digits and no
rounding may be applied: a balance of 12345678901234 lamports is
`"12345.678901234"`. Every amount of a withdraw answer shall be exact.
Whether the padded forms of R40.2A.4's sums are reproduced is D40.2A.1.

**D40.2A.1** *The padded sums of R40.2A.4.* (a) This project answers the
shortest form in those members too: every value is numerically equal to
the reference's, and the text differs only in the cases R40.2A.4 lists
(`"1"` for `"1.000000"`, `"0"` for `"0.0"`). (b) This project reproduces
the reference's text. A client that reads the members as decimal numbers
sees no difference between the two.

> *Reloaded status (clean side, 2026-11-21):* by the maintainer's decision
> of 2026-11-21, neither option is taken as written: a sum or difference of
> R40.2A.4 is written with the coin's full decimals, 9 for SOL and the
> mint's decimals for a token amount. The members are `spent_by_me` and
> `my_balance_change` of a SOL withdraw, `my_balance_change` of an SPL
> withdraw, the `total_amount` of `fee_details` in an SPL withdraw, and
> `required` of `NotSufficientPlatformBalanceForFee` and of a SOL
> withdraw's `NotSufficientBalance`. Every value equals the reference's;
> the text differs from R40.2A.4 where the reference writes fewer
> decimals: `"1.500005000"` for `"1.500005"`, `"1.000000000"` for
> `"1.000000"`, `"-1.500000"` for `"-1.5"` (a 6-decimal mint), `"0.000000"`
> for `"0.0"` and `"0.00"`, `"0.002044280"` for `"0.00204428"`, and for a
> SOL `NotSufficientBalance` `"1.000005000"` where R40.2A.4's error
> sentence gives the shortest form. A value with more decimals than its
> coin's keeps them all. Single figures and every `available` keep the
> shortest form. T40.2A.4's sum text follows this note. Built in PR #456.

**T40.2A.1** Every row of R40.2A.1's table, on each surface of R40.2A.3
and on `enable_solana_with_tokens`.

**T40.2A.2** Token balances from a raw amount and the mint's decimals:
1500000 at 6 gives `"1.5"`; 0 at 6 gives `"0"`; 7 at 0 gives `"7"`;
4572211441380 at 8 gives `"45722.1144138"`; 123456789012345678 at 9 gives
`"123456789.012345678"`.

**T40.2A.3** The unspendable member is `"0"` for SOL and for a token on
every surface of R40.2A.3.

**T40.2A.4** A SOL withdraw of 12345.678901234 SOL answers that text as
`total_amount`, and `spent_by_me` equal in value to the amount plus the
fee with no digit lost; the text of a sum follows the option chosen in
D40.2A.1.

## 40.3 `experimental::enable_solana_token`

R40.3.1 **Request fields:** `ticker` (string, **required**);
`activation_params` (object, **required**, may be `{}`; members ignored);
`protocol` (object, optional, custom `SOLANATOKEN` protocol).

R40.3.2 **Semantics:** reject an already-active token; resolve configuration or
custom protocol; find the `platform` among active coins and check it is a Solana
platform; initialise the token as in R40.2.2 (node selection, mint-owner
check); read its address, balance and the chain height; register it and add it
to the platform's token set.

R40.3.3 **Success result** (HTTP 200): `{ticker: string, address: string (the
associated token account, R40.1.4), current_block: integer (block height),
balance: {spendable: string, unspendable: string}}`.

R40.3.4 **Errors** (all mmrpc-level):

| `error_type` | Returned when | `error_data` | HTTP |
| --- | --- | --- | --- |
| `TokenIsAlreadyActivated` | token already active, or registration race | string: token ticker | 400 |
| `TokenConfigIsNotFound` | not in configuration and no `protocol` | string: token ticker | 400 |
| `TokenProtocolParseError` | protocol unparsable | `{ticker, error}` | 500 |
| `UnexpectedTokenProtocol` | protocol is not `SOLANATOKEN` | `{ticker, protocol}` | 400 |
| `CustomTokenError` | custom `protocol` for a configured ticker | `{"DuplicateTickerInConfig": {"ticker_in_config": string}}` | 400 |
| `PlatformCoinIsNotActivated` | `platform` not active | string: the **platform** ticker | 400 |
| `UnsupportedPlatformCoin` | `platform` active but not a Solana platform | `{platform_coin_ticker: string, token_ticker: string}` | 500 |
| `Transport` | no healthy node; node error reading the mint account, the balance or the block height; mint owned by another program | string | **500** |
| `Internal` | internal failure | string | 500 |

**Unresponsive nodes:** `Transport` (string), HTTP **500**. (The single-token
method maps `Transport` to 500, the platform method of R40.2.4 to 502; both are
bound as the reference does.)

> *Reloaded status (clean side, 2026-11-19):* built as R40.3.1-R40.3.4 bind
> (PR #426). The platform may have been activated through either surface. A
> Token-2022 token is refused before any node is contacted, with `Internal`
> as on `enable_spl` (D40.6B.4 (a)); an inline `protocol` follows the status
> note after R40.2.5.

## 40.3A Reloaded's primary surface (Reloaded-original)

This section binds the request, result and error shapes of Reloaded's own
activation methods, `enable_solana_with_tokens` and `enable_spl`, which stay
the primary contract (maintainer decision of 2026-10-11 at the top of this
chapter). Their failures map as on the compatibility surface (§40.2.4,
§40.3.4; maintainer decision of 2026-11-15), so that a client that handles
one surface handles both. While `SOLANA_ACTIVATION_AVAILABLE` is `false`
both methods answer the refusal of T40.9 before anything below applies.

**R40.3A.1** *`enable_solana_with_tokens`, request.* Members flat in
`params`; members not listed are ignored, `tx_history` included (T40.6B.21):

| Member | JSON type | Required / default | Effect |
| --- | --- | --- | --- |
| `ticker` | string | **required** | platform ticker (`SOLANA` protocol) |
| `confirmation_commitment` | string: `processed`, `confirmed` or `finalized` | **required** | the commitment level sent with node requests |
| `client_url` | string | optional | one node, tried first |
| `client_urls` | array of strings | optional, default `[]` | further nodes, in order after `client_url` |
| `spl_tokens_requests` | array of `{ticker}` | **required**, may be empty | SPL tokens activated inline, each from its configuration entry |

At least one node must be given in `client_url` or `client_urls`. A token
request has no `protocol` member: a token is activated from its
configuration only. A request that does not parse (a required member
missing, an unknown commitment level) is `InvalidRequest` (HTTP 400, chapter
55 §55.1).

**R40.3A.2** *`enable_solana_with_tokens`, result* (HTTP 200):

| Member | JSON type | Content |
| --- | --- | --- |
| `current_block` | integer | the chain's block height (`getBlockHeight`) |
| `solana_addresses_infos` | object | one entry keyed by the platform address: `{derivation_method, pubkey, balances: {spendable, unspendable}}` |
| `spl_addresses_infos` | object | one entry keyed by the platform address: `{derivation_method, pubkey, balances}`, `balances` mapping each token ticker activated in this call to `{spendable, unspendable}` |

`derivation_method` is `{"type": "Iguana"}` under a single-key login;
`pubkey` is the base58 platform address. Balances are decimal strings and
`unspendable` is `"0"`.

**R40.3A.3** *`enable_solana_with_tokens`, errors.* As R40.2.4, with these
cases of this surface:

| `error_type` | Returned when | `error_data` | HTTP |
| --- | --- | --- | --- |
| `PlatformCoinCreationError` | neither `client_url` nor `client_urls` names a node; key derivation failure; failure to read the block height, the SOL balance or a token balance | `{ticker, error}` (`ticker` = platform ticker) | 500 |
| `TokenProtocolParseError` | a token's configuration protocol does not parse, an invalid `token_contract_address` or `mint_address` included | `{ticker, error}` | 500 |
| `Internal` | the platform address cannot be rendered | string | 500 |

The other rows of R40.2.4 apply unchanged. Node names in every error text
are host-only (R40.5A.1). Initialising the inline tokens contacts no node
on this surface today, so its `Transport` row (HTTP 502) is not reached; it
is reached once the mint-owner check of R40.2.2 is built (plan
`solana-support`, the compatibility-methods package).

> *Reloaded status (clean side, 2026-11-18):* the mint-owner check of
> R40.2.2 is built for both methods of this surface (PR #425). Initialising an
> inline token now contacts a node, so a node failure there, or a mint owned
> by another program than the token's `program_id`, answers the `Transport`
> row (HTTP 502) instead of `PlatformCoinCreationError`. `enable_spl`
> answers `Transport` (HTTP 500) for the same cases.

> *Reloaded status (clean side, 2026-11-19):* the `PlatformCoinMismatch`
> row of R40.2.4 (HTTP 400, no `error_data`) is now answered on this
> surface as well, before any node is contacted (PR #426). Before, an inline
> token whose `platform` named another coin was not checked.

**R40.3A.4** *`enable_spl`.* Request: `ticker` (string, **required**) and
`activation_params` (object, **required**, may be `{}`; members ignored);
there is no `protocol` member. Result (HTTP 200): `{balances,
token_contract_address, platform_coin}`, `balances` holding one entry
keyed by the wallet's platform address with `{spendable, unspendable}`,
`token_contract_address` the base58 mint and `platform_coin` the platform
ticker. Errors: as R40.3.4. A node failure while the balance is read is
`Transport` (string, HTTP 500); an invalid `token_contract_address` or
`mint_address` is `TokenProtocolParseError` (`{ticker, error}`, HTTP 500).

**R40.3A.5** *Decimals.* The platform's decimals are 9 whatever the
configuration entry says; a token's decimals are its
`protocol_data.decimals` (R40.6B.2).

**T40.3A.1** Each error row of R40.3A.3 and R40.3A.4 that can be produced
without a node, serialized as the RPC layer does: the exact `error_type`,
the exact `error_data` members and the HTTP status.

**T40.3A.2** A node that refuses the connection, with no inline token:
`PlatformCoinCreationError`, the platform ticker, HTTP 500; the text names
the node host-only. The same node behind an active platform with
`enable_spl`: `Transport`, HTTP 500.

**T40.3A.3** R40.3A.5: T40.6B.3.

## 40.4 Balances & chain state (R31 dictated by Solana JSON-RPC)

R40.4.1 SOL balances shall be read with `getBalance` and converted from lamports
with 9 decimals. SPL balances shall be read for the owner via
`getTokenAccountsByOwner` (mint filter) and `getTokenAccountBalance`, reporting
the RPC's UI amount string; a node answer stating the mint is unknown yields a
zero balance.

R40.4.2 The current chain height shall be `getBlockHeight` and is surfaced as
`current_block`.

## 40.5 Addresses, withdraw & transactions (R31 dictated by Solana/SPL)

R40.5.1 Addresses shall follow Solana's ed25519 keypair / base58 account model
(R40.1.4); SPL balances shall be read from the owner's associated token accounts
for each mint.

R40.5.2 The coin shall support constructing, signing, and broadcasting SOL and
SPL transfers and shall decode transaction details returned by the node into the
project's common transaction-details shape.

## 40.5A Node naming and credentials (Reloaded-original; applies when Solana returns)

R40.5A.1 **Host-only naming of Solana nodes and credentials in renderings
(Reloaded; clean-room original; maintainer decision 2026-10-26; no switch).**
Every log line, error text and status message the Solana coin layer emits that
names a node shall name it in the host-only form of
[chapter 35](35-evm-v2-activation-rpcs.md) R35.1.14: scheme, host and, when
present, port. This covers the node URLs of either activation surface (the
`nodes[].url` of R40.2.1 and the node URL members of Reloaded's own surface,
§40.6 item 3), the health-check selection of R40.1.5, the "no healthy node"
errors of R40.2.4 and R40.3.4, and any lower-layer text (HTTP client, TLS, DNS)
embedded in an emitted text, which shall carry the host-only form in place of
a full URL. Chapter 35 R35.1.17 applies to the Solana node clients, their pool
and the activation requests, with its scope limit for the RPC wire dump.

The rule applies when Solana returns (D40.4). While `SOLANA_ACTIVATION_AVAILABLE`
is `false` no node is contacted, so nothing it binds can be observed. The code
stays compiled, so the rule may be implemented and T40.10 run before then.

It has no opt-back switch, for the reason R35.1.14 gives: node access reaches
no peer and the rule only protects the operator, so by the second arm of the
default-side rule (`docs/COMPAT_SWITCHES.md`) the Reloaded form is the default.
It is recorded in `docs/GLEEC_COMPATIBILITY.md` with "none -- no switch
exists".

> **Status (2026-10-28).** Implemented. Transport failures name the node
> host-only through the shared helper (`coins/node_url.rs`), and the node
> client's debug form shows the node host-only. T40.10 covers it; the test
> against the live devnet needs network access.

## 40.6 Reloaded status (informative)

Reloaded currently ships the baseline-era Solana surface, which differs from
§40.1-§40.3 in every wire aspect (in this beta the surface is routed but
refuses every request; see the availability note at the top of this chapter):

1. **Method names.** Reloaded routes `enable_solana_with_tokens` and
   `enable_spl` (both present at the GPLv2 baseline) and routes neither
   `experimental::` method. Both bound methods must be added.
2. **Targets.** Reloaded routes Solana on native only; bound: all targets
   including WASM.
3. **Platform request.** Reloaded expects `confirmation_commitment`,
   `client_url` / `client_urls` and `spl_tokens_requests`; bound: `nodes[{url}]`,
   `tokens_params`, `tx_history` (R40.2.1).
4. **Results.** Reloaded returns `current_block`, `solana_addresses_infos` and
   `spl_addresses_infos` (address-keyed derivation/pubkey/balance objects) for
   the platform, and `balances` / `token_contract_address` / `platform_coin` for
   the token; bound: R40.2.3 and R40.3.3.
5. **Coin configuration.** Reloaded parses `{"type": "SOLANA"}` without
   `protocol_data` and `SPLTOKEN` with `token_contract_address`; bound:
   `SOLANA` with `protocol_data: {}` and `SOLANATOKEN` with `mint_address` and
   `program_id` (R40.1.2-R40.1.3).
6. **Errors.** Reloaded maps block-height and balance failures to `Internal`,
   has no node-health selection, no mint-owner check, no
   `PlatformCoinMismatch` / `CustomTokenError`, and maps token-creation failures
   to `Internal`; bound: R40.2.4 / R40.3.4.
7. **Keys.** Reloaded's keypair derivation produces the same ed25519 key as
   R40.1.4 for the same 32-byte secret; no change needed.

> **Reloaded status (clean side, 2026-11-15).** The list above was checked
> against the code of this date. Item 6 is partly out of date: the platform
> reaches its nodes through a failover pool that tries them in the configured
> order and sets a node aside for a while after a transport failure. The pool
> sends no `getHealth` request, so it is not the node selection of R40.1.5. The
> rest of item 6 holds. The shipped code has four further gaps; none of them can
> be reached while Solana is switched off:
>
> - *Token-2022.* Reloaded's token configuration carries the `program_id`
>   (PR #361, status note after R40.1.3), but nothing uses it yet: the
>   token's account, its balance, the transfer instruction and the creation of
>   the receiver's token account all use the SPL Token program. A Token-2022
>   mint therefore fails in every one of these operations, not only in the
>   balance read the code-quality finding below names.
> - *SPL `withdraw` with `max: true`* transfers the requested amount, not the
>   balance the answer reports.
> - *Rent.* An SPL `withdraw` that creates the receiver's associated token
>   account also pays that account's rent deposit. The balance check and
>   `fee_details` do not count it.
> - *Unimplemented operations.* Several coin operations that ordinary RPCs reach,
>   such as transaction history, address conversion, the raw-transaction query
>   and the confirmation settings, are unimplemented and end the request
>   abnormally.
>
> `docs/plans/solana-support.md` schedules each of them before Solana is
> switched back on.

> *Reloaded status (clean side, 2026-11-18):* of the four gaps above, the
> SPL `max` withdraw and the rent are fixed in PR #425, and a Token-2022 token
> is refused at activation (D40.6B.4 (a)) until its own package; the
> unimplemented operations answered from PR #401 on.

> **Decision recorded (informative).** The baseline-inherited
> `enable_solana_with_tokens` / `enable_spl` methods stay routed as the primary
> surface (see the maintainer decision at the top of this chapter); in this
> beta they refuse every request while `SOLANA_ACTIVATION_AVAILABLE` is
> `false`, and the decision applies again when Solana returns. Items 1-6
> above are therefore read as "the compatibility surface is not yet added", not
> as defects of the primary surface; item 6's error-mapping differences apply
> to the compatibility surface and are re-evaluated for the primary surface in
> the Solana plan. Release scope is tracked in `UPSTREAM-PARITY-GAPS.md`.

> **Reloaded status (clean side, 2026-11-15).** Re-evaluated: by the
> maintainer's decision of 2026-11-15 the primary surface maps its failures
> as the compatibility surface does; §40.3A binds it. PR #371.

> **Code-quality finding (informative; trigger: a code comment states the
> problem).** The reference's Solana key-policy handling is acknowledged in the
> reference itself as provisional and not production-ready. Observable
> consequence: only the single-key context activates (R40.1.4); HD-wallet users
> cannot activate Solana at all. Reloaded binds that restriction for parity;
> HD support is deferred (D40.2).

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** In the reference, the SPL token's own address is the associated
> token account derived with the token's `program_id`, but the token balance is
> read from the associated token account derived for the default SPL Token
> program. For a Token-2022 mint the two differ, so the reported balance can be
> wrong (typically zero). Proposed fix for reloaded: read the balance from the
> associated token account derived with `program_id`. Several SPL-token
> capability queries are also unimplemented in the reference and abort the
> request if reached; reloaded shall return structured errors instead.

## 40.6A Trading floors (pointer; Spec Reader 2026-10-25, cleared by the Dirty Gate with chapter 60 on 2026-10-25)

R40.6A.1 In both reference designations (`v2.6.0-beta` and `v3.0.0-beta`
agree) neither SOL nor an SPL token has a usable minimum trading volume:
the accessor is unimplemented and ends the request abnormally, so the
reference cannot place an order in either. The minimum transaction amount
is one lamport (10^-9) for SOL **and** for every SPL token, whatever the
token's decimals. Reloaded reports a minimum trading volume of 0.00777
and a minimum transaction amount of zero for both, values carried over
from the baseline anchor (R1). Neither tree can swap Solana, so no peer can
observe the difference; reloaded may keep its values, but no Solana
request may terminate abnormally
([Chapter 60](60-trading-floors-and-refusal-contract.md) row P1, R60.6.5,
finding F6).

> **As-built note (2026-10-25, clean-side record).** Implemented in PR
> #66. Reloaded keeps its values. `setprice`, `buy`, `sell` and both trade
> previews used to reach unimplemented Solana and SPL fee functions; they
> now refuse a pair involving a Solana or SPL coin with an error naming
> the coin, so no Solana request ends abnormally. The plan
> `solana-support` chooses Solana's floors when Solana trading is
> designed. The cleared rule text above is unchanged.

> **Reloaded status (clean side, 2026-11-17).** By the maintainer's decision
> of 2026-11-15 the minimum transaction amount is one lamport
> (0.000000001) for SOL and one smallest unit of the token's own decimals
> (10^-decimals) for each SPL token; the minimum trading volume stays 0.00777
> until Solana trading is designed. PR #401.

## 40.6B Reference behavior beyond activation: keys, withdraw, per-method answers, history and swaps

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-15,
> measure 0.01 (attestation in §40.12).**
> This section was written on 2026-11-15 by the KDF Spec Reader (Spec Reader
> round 0.5.0-4) under the two-team procedure of chapter 01 §1.4A, from five
> inputs: (1) the reference lineage at the rows `v3.0.0-beta`, `v2.6.0-beta`
> and `main (as of 2026-10-07)` of chapter 02 §2.15A, and at the row
> `v2.1.0-beta` for the single question of §40.6B.7 whether a Solana swap
> ever existed at a release; (2) the sibling coins-registry row `coins
> solana-integration (as of 2026-11-08)`: its four Solana entries, its
> integration document, its registry tooling and its generated fixtures,
> read as reference-side material that only the two roles of chapter 01 R37
> read; (3) the sibling swap-program rows `satomic-swap dev (as of
> 2024-06-18)` and `satomic-swap fix/htlc-correctness (as of 2026-10-28)`;
> (4) observations of the public Solana devnet and mainnet JSON-RPC
> endpoints made on 2026-11-15 (chapter 01 R6); (5) this repository's own
> tree. The three sibling rows were added to chapter 02 §2.15A in this round
> and passed the gate with this section.

*Scope.* §40.1-§40.3 bind the reference's two activation methods and
§40.4-§40.5 state in two sentences each that balances are read and that
transfers can be built. This section records what the reference does on the
rest of the surface a wallet touches once a Solana coin is active: which
configuration keys count (§40.6B.2), where the key comes from (§40.6B.3),
what a withdraw answers (§40.6B.4), what each of a list of wallet methods
answers (§40.6B.5), transaction history (§40.6B.6), swaps (§40.6B.7) and
what the sibling registry branch adds (§40.6B.8). Where the reference ends
a request abnormally or produces a wrong result, the section says so and
raises a decision (§40.6B.9) instead of binding the behavior. It prescribes
no design. Method names, request and response member names, `error_type`
values, public program addresses and public JSON-RPC method names are the
interface and are quoted; nothing else of the reference is. It does not
restate §40.1-§40.3; each rule names the cleared rule it completes.

### 40.6B.1 States read, and one term

**F40.6B.1** *The three states agree.* The Solana coin, both activation
methods and every shared request handler this section names behave the same
at `v2.6.0-beta` and at `v3.0.0-beta`; `main (as of 2026-10-07)` differs
from `v3.0.0-beta` in its read-me only. "The reference" below therefore
means all three rows, and no statement of this section differs by state
unless it names a row. The reference metadata of the historical record
(branch and tag names, identifiers, dates) was compared with the live
remote on 2026-11-15: no branch or tag is newer than
`main (as of 2026-10-07)`, so no row for a newer state of the v3 line was
needed or added. R40.0.1's statement that the two release rows agree is
confirmed and extends to everything below.

**F40.6B.2** *Abnormal end.* Many operations of the reference's Solana coin
and SPL token are placeholders that state their own incompleteness. A
request that reaches one **ends abnormally**: on a native target the caller
receives HTTP 500 with a short plain-text body in place of a JSON answer,
handling of that one request stops at the point it had reached, and the
process goes on serving other requests. This is what R40.2.5's note calls
"no normal response" and what chapter 60 finding F6 calls terminating
abnormally. It was established by
reading the request service, not by running the reference; the outcome on
the WASM target was not established. An abnormal end is never a parity
target (R40.6B.14).

### 40.6B.2 Activation: what §40.1-§40.3 leave open

**R40.6B.1** *No drift.* The method names, targets, request members,
results, `error_type` values, `error_data` shapes and HTTP statuses of
§40.1-§40.3 are what the reference does at all three rows. Nothing in
§40.1-§40.3 needs correction; R40.6B.2-R40.6B.6 add cases those rules do
not state.

**R40.6B.2** *Configuration keys.* Of a Solana platform entry and of an SPL
token entry, the reference's activation reads one top-level key, `protocol`
(R40.1.2, R40.1.3). No other top-level key of the entry changes the address,
the balance, the result or the error of either activation method:

| Top-level key | Effect on a Solana coin at the three rows |
| --- | --- |
| `protocol` | read; R40.1.2, R40.1.3 and R40.6B.3 |
| `derivation_path` | never read by any Solana path. Present, absent or malformed, it changes nothing; no key is derived from it (R40.6B.7) |
| `decimals` | never read. The platform's decimals are 9 whatever the entry says; a token's decimals are `protocol_data.decimals` only. Where a token entry carries `decimals` both at the top level and inside `protocol_data`, the top-level value is ignored, also when the two disagree |
| `required_confirmations`, `requires_notarization` | never read; the coin has no confirmation setting (§40.6B.5) |
| `wallet_only` | not read by the coin; read from the configuration by the order-book methods (R40.6B.6) |
| any other key | not read by activation |

This completes R40.1.2-R40.1.3.

> **Reloaded status (clean side, 2026-11-15).** Reloaded fixes the
> platform's decimals at 9 and reads a token's decimals from
> `protocol_data` only (R40.3A.5, PR #371); before, it read a platform
> entry's top-level `decimals`. `derivation_path` is not read under a
> single-key login.

**R40.6B.3** *Protocol parsing, completing R40.1.2-R40.1.3.*

1. A platform entry needs `protocol_data` to be present and to be an
   object; an entry with `"type": "SOLANA"` and no `protocol_data` is
   unparsable (`CoinProtocolParseError`, HTTP 500, `{ticker, error}`).
   Members inside the object are ignored. That bare form is this
   repository's own platform form (§40.6 item 5), so once both forms are
   accepted (plan `solana-support`, work package 2) Reloaded accepts an
   entry the reference refuses; that is a difference to record, not a
   refusal to copy.
2. All four members of a token's `protocol_data` are required and none has
   a default. A `SOLANATOKEN` entry **without `program_id`** is unparsable:
   `TokenProtocolParseError` (HTTP 500, `{ticker, error}`) from either
   method. The reference does not fall back to the classic Token program.
3. `program_id` may be **any** valid base58 public key; parsing does not
   restrict it to the SPL Token and Token-2022 programs. Whether it fits is
   decided on chain by the mint-owner check of R40.2.2 (`Transport` when
   the mint is owned by another program).
4. `decimals` is an integer from 0 to 255; anything else is unparsable.
5. Unknown members inside a token's `protocol_data` are ignored.

**R40.6B.4** *A token the wallet holds no account for.* The reference reads
a token balance in two node calls: `getTokenAccountsByOwner` filtered by the
mint, of which only failure matters, and `getTokenAccountBalance` for the
associated token account derived for the classic Token program. Two cases
follow that R40.2.3 and R40.4.1 do not state:

- The node answers that it does not know the mint: balance `"0"` (this is
  R40.2.3's case).
- The mint exists and the wallet has no such token account, which is the
  normal state of a wallet that never received the token: the node answers
  `getTokenAccountBalance` with a JSON-RPC error (observed on the public
  devnet on 2026-11-15 for an address with no account), and the reference
  does **not** turn that into zero. The balance read fails. Inline in the
  platform method this is `PlatformCoinCreationError` (HTTP 500,
  `{ticker, error}`, R40.2.4's "failure to read ... a token balance"); in
  the token method it is `Transport` (HTTP 500, R40.3.4). Nothing is
  registered. A token whose account is closed after activation fails
  `my_balance` the same way.

So at the three rows a token can be activated only by a wallet that already
holds an account for it, or against a node that does not know the mint.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** A wallet cannot activate a token it has not yet received. A
> missing token account is an ordinary zero balance, not a node fault. This
> repository's own balance read already answers zero when the owner holds
> no account for the mint. Reloaded must not copy the failure
> (D40.6B.2).

**R40.6B.5** *A Token-2022 `program_id` at activation.* Such an entry
parses (R40.6B.3 item 3) and passes the mint-owner check when the mint is
owned by the Token-2022 program; the token's address is the associated
token account derived with that program (R40.1.4). The balance, however, is
read from the account derived for the classic program (the second
code-quality finding of §40.6). For an existing Token-2022 mint that
account normally does not exist, so by R40.6B.4 the activation **fails**;
it reports zero only where the node does not know the mint. Withdraw is
refused as well (R40.6B.11). At the three rows a Token-2022 token is
therefore unusable, not merely misreported.

> **Note (refines a cleared finding; no cleared sentence is edited).** The
> second code-quality finding of §40.6 says the reported Token-2022 balance
> "can be wrong (typically zero)". By R40.6B.4 the typical outcome for a
> mint that exists is a failed activation, not a zero balance. The proposed
> fix of that finding stands.

**R40.6B.6** *`wallet_only`.* The key is honored where the configuration is
consulted and has no effect on the coin itself:

- The order-book and trade-preview methods that test the configuration
  refuse or skip a ticker whose entry has `wallet_only: true`, as for every
  coin family. A token activated with a request-supplied `protocol` (a
  custom token) has no configuration entry and is treated by those methods
  as wallet-only.
- The coin's own answer to "may this coin trade" is a placeholder for both
  Solana kinds, and so are its minimum trading volume (R40.6A.1) and its
  trade-fee estimates. Placing an order with a Solana coin (`setprice`,
  `buy`, `sell`) ends abnormally whatever the key says, and no order-book
  address form exists for the two protocols.

No value of `wallet_only` makes a Solana coin tradable at the three rows;
for the methods that test the configuration, `wallet_only: false` only
moves the failure from an orderly refusal to an abnormal end. This
repository already refuses a Solana pair with an error
naming the coin (the as-built note of §40.6A).

### 40.6B.3 Keys

**R40.6B.7** *Key source, completing R40.1.4.* Neither activation request
has a key-policy member, and unknown request members are ignored, so
nothing in a request can select a hardware wallet or WalletConnect. The key
follows the login alone:

| Login | Outcome at the three rows |
| --- | --- |
| passphrase (single-key) login | the key of R40.1.4; activation proceeds |
| global-HD login | refused: `PlatformCoinCreationError` (HTTP 500, `{ticker, error}`), before any node is contacted; the token method then answers `PlatformCoinIsNotActivated` |
| a hardware wallet initialized beside the login | never consulted; the outcome is the one of the login's row above |
| an open WalletConnect session | never consulted; the outcome is the one of the login's row above |

R40.1.4 says an "HD-wallet, hardware-wallet or WalletConnect context fails
platform activation". The first holds as stated. The other two are
refusals the reference provides for but that no request can reach; this
agrees with chapter 05's hardware-source table, which lists Solana as not
reachable from the request.

**No derivation exists.** No row derives a Solana key from an HD seed. There
is no path layout, no ed25519 derivation scheme and no HD seed input to
report, and no address under a global-HD login can be given for any
mnemonic. The key-export method `get_private_keys` answers an error for a
Solana ticker in both of its modes, and `show_priv_key` ends abnormally, so
the Solana secret cannot be exported through any method.

**R40.6B.8** *Single-key test vector.* Under a passphrase login whose
passphrase is the public twelve-word BIP-39 test mnemonic (eleven times
`abandon`, then `about`), used as a plain passphrase:

- 32-byte secret (the baseline's passphrase rule: SHA-256 of the passphrase
  text with the baseline's three bit adjustments):
  `c057eec878dfd852ba3f88087c4f350f09c55537ab5e549c3cd14320ec3cef78`;
- Solana address (the ed25519 public key for that 32-byte seed, base58):
  `7zuqksARN8uXHz2yvrkiZZDPeALBSjzK12nkMPNzKSCJ`.

The vector was computed in this round from R40.1.4, the baseline's
passphrase rule as this repository carries it (`crypto/src/privkey.rs`) and
RFC 8032 key generation, and cross-checked with two independent ed25519
implementations. It was not obtained by running the reference and is not
taken from any reference test.

> **Consistency note (two cleared rules disagree; neither is edited).**
> R40.1.4 and T40.8 have a global-HD login refused on the compatibility
> surface. [Chapter 05](05-hd-wallet-support.md) R46 item 3 has this
> repository's shared platform-with-tokens activation hand the legacy
> passphrase key to Solana under a global-HD login, which activates with
> the address of R40.6B.8's rule instead of refusing. Both cannot hold for
> one activation path. D40.6B.7 puts the choice.

### 40.6B.4 Withdraw and broadcast

R40.5.2 says transfers can be built, signed and broadcast. The rules below
give the reference's contract for the `withdraw` method (both its legacy
and its versioned form answer alike) and for `send_raw_transaction`. Node
access is as in R40.1.5: every operation first selects a node by
`getHealth`, more than once within one withdraw. No request carries a
commitment level, so the node's default applies. Any node failure during a
withdraw answers `Transport` (HTTP 502, string).

**R40.6B.9** *SOL withdraw.*

- *Request.* `coin`, `to`, `amount` and `max` are honored. `fee`, `memo`,
  `from`, `broadcast` and every other member are accepted and ignored
  (chapter 49's fee-policy note agrees for `fee`).
- *Recipient.* Any base58 text that decodes to 32 bytes is accepted; any
  other text answers `InvalidAddress` (HTTP 400, string). The reference
  does not test whether the address is on the curve or whether the account
  exists.
- *Fee.* The node's quote (`getFeeForMessage`) for a plain one-signature
  transfer on a fresh `getLatestBlockhash`. No priority fee and no
  compute-budget instruction are added.
- *Amount.* Given in SOL; digits beyond the ninth decimal are dropped, not
  rounded and not refused. A resulting amount below one lamport answers
  `AmountTooLow` (HTTP 400, `{amount, threshold}`, the threshold being one
  lamport). With `max`, the amount is the balance less the fee; when that
  is nothing, the answer is `AmountTooLow` as well.
- *Balance.* When amount plus fee exceeds the balance:
  `NotSufficientBalance` (HTTP 400, `{coin, available, required}`,
  `required` being amount plus fee).
- *Transaction.* One system-program transfer, the sender as fee payer, the
  legacy (unversioned) message form, signed with the wallet key.
- *Answer.* `tx_hex` is the hex of the signed transaction in Solana's wire
  serialization; `tx_hash` is the base58 form of its first signature;
  `from` holds the wallet address and `to` the recipient; `total_amount`
  is the amount; `spent_by_me` is amount plus fee; `received_by_me` is the
  amount when the recipient is the wallet itself and zero otherwise;
  `my_balance_change` is received less spent; `block_height` is 0;
  `timestamp` is the local clock; `internal_id` is the hex of the
  characters of `tx_hash`; `transaction_type` is `StandardTransfer`;
  `memo` is null; `fee_details` is R40.6B.12.

**R40.6B.10** *SPL withdraw.*

- *Recipient.* `to` is validated as in R40.6B.9 and is **always** read as
  a wallet (owner) address: the destination is the associated token
  account of `to` for the token's mint and `program_id`.
- *Amount.* Given in token units and scaled by `protocol_data.decimals`;
  surplus digits are dropped. A resulting zero answers `AmountTooLow`,
  whose threshold is one lamport in SOL terms whatever the token's
  decimals (R40.6A.1). A token balance below the amount answers
  `NotSufficientBalance` with the token's ticker. A failed token-balance
  read answers `Transport` (HTTP 502), which by R40.6B.4 includes a wallet
  that holds no account for the token.
- *`max`.* See the finding below; the reference does not send the whole
  balance.
- *Creating the recipient's account.* When `getAccountInfo` finds no
  destination account, the transaction first creates it with the
  idempotent creation instruction of the associated-token-account program,
  paid by the sender, and then transfers. The rent is what
  `getMinimumBalanceForRentExemption` quotes for the size of a classic
  token account (165 bytes) and is reported as `rent_amount`; it is zero
  when the account exists.
- *Transfer.* The checked transfer of the token program (amount, decimals
  and mint are all stated), from the wallet's own token account to the
  destination, authorized and paid by the wallet key; legacy message form.
- *Fee and the SOL check.* The fee is the node's quote for the message
  actually built. When fee plus rent exceeds the wallet's SOL balance, the
  answer is **`NotSufficientPlatformBalanceForFee`** (HTTP 400,
  `{coin, available, required}`, where `coin` is the *platform* ticker and
  `required` is fee plus rent).
- *Answer.* Amounts are in token units: `total_amount` and `spent_by_me`
  are the amount; `received_by_me` is the amount when `to` is the wallet's
  own address and zero otherwise; `my_balance_change` is received less the
  amount (the fee, being SOL, is not in it). `from` holds the wallet
  address, not the token account; `to` holds the recipient's wallet
  address. `transaction_type` is the `TokenTransfer` form carrying 32
  bytes, the SHA-256 of the mint's public-key bytes. `tx_hex`, `tx_hash`,
  `internal_id`, `block_height`, `timestamp` and `memo` are as in
  R40.6B.9.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** *The SPL "max" withdraw sends the wrong amount.* With `max`
> the reference takes the whole-number part of the displayed token balance
> and uses that number as a count of the token's smallest units. A wallet
> holding 12.5 units of a six-decimal token sends 0.000012; a wallet
> holding less than one whole unit is answered `AmountTooLow`. Reloaded
> must send the token account's full amount (D40.6B.3).

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** *A recipient given as a token-account address.* The reference
> accepts either kind of address in `to` but treats both as an owner. When
> `to` is itself a token account, the destination of the transfer is a new
> token account whose owner is the account named in `to`, not the account
> named in `to` itself. Reloaded must not build that transfer; the choice
> of answer is D40.6B.6.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** *No rent guard on SOL.* Neither withdraw checks Solana's
> rent-exempt minimum: a SOL withdraw that would leave the sender's account
> between zero and that minimum, or that sends less than the minimum to an
> address with no account, is built and answered normally and is then
> refused by the node at broadcast. Proposed fix: test both against
> `getMinimumBalanceForRentExemption` and answer a structured error at
> withdraw time.

**R40.6B.11** *Token-2022 withdraw.* For a token whose `program_id` is not
the classic Token program the transfer cannot be built, and the answer is
`InternalError` (HTTP 500, string). The request gets that far only if the
balance read succeeded, which R40.6B.5 makes unlikely. The rent quote of
R40.6B.10 assumes the classic account size and would understate the rent
of a Token-2022 account that carries extensions. The registry branch
publishes no Token-2022 entry (§40.6B.8).

> *Reloaded status (clean side, 2026-11-18):* Reloaded refuses a Token-2022
> token at activation (D40.6B.4 (a)), so its withdraw is not reached. Where a
> token with another `program_id` than the classic Token program reaches the
> withdraw anyway, the answer is `UnsupportedError` (HTTP 400, string; ch. 49
> R49.27a), before any transaction is built, not `InternalError`. PR #425.

**R40.6B.12** *`fee_details`.* For both Solana kinds the member is an
object of four members:

| Member | JSON type | Content |
| --- | --- | --- |
| `type` | string | `Solana` |
| `fee_amount` | decimal string | the network fee, in SOL |
| `rent_amount` | decimal string | the rent for a created recipient token account, in SOL; `"0"` for a SOL withdraw and when no account is created |
| `total_amount` | decimal string | `fee_amount` plus `rent_amount` |

It has no member naming the coin the fee is paid in, although for a token
that coin is the platform, not the token. This repository's tree answers a
different object for the same tag: `type` and a single `amount`
(D40.6B.5).

**R40.6B.13** *`send_raw_transaction`.* `tx_hex` is hex-decoded and read as
one transaction in Solana's wire serialization, no longer than one network
packet's payload (1232 bytes); it is submitted with `sendTransaction` and
the answer is `{tx_hash}`, the base58 signature the node returns. The
method does not wait for confirmation. A token ticker takes the platform's
path unchanged.

### 40.6B.5 Per-method answers on an active Solana coin

**R40.6B.14** *No abnormal end (requirement).* No request naming a Solana
coin or an SPL token shall end abnormally in this project; where the table
below says "abnormal end" for the reference, Reloaded shall give a JSON
answer, a success or a structured error. This extends R40.2.5 and chapter
60 R60.6.5 from activation and trading to the wallet surface. Which answer
each method gives is D40.6B.1.

"Both" in the table means the platform coin and an SPL token alike. The
last column is this repository's tree as read on 2026-11-15, with Solana
switched off, so none of it can be observed in a build today; "not
examined" means this round did not read it.

| Method | Reference at the three rows | This repository's tree |
| --- | --- | --- |
| `sign_message` | Both: abnormal end. No prefix and no signature encoding exist to report. A request that carries an `address` member under a passphrase login is refused with `InvalidRequest` before the coin is consulted, as for every family | signs the UTF-8 bytes of the message as they are, with no prefix and no pre-hash, and answers the 64-byte ed25519 signature in base58 |
| `verify_message` | Both: abnormal end; the address check that comes first is itself a placeholder | verifies such a signature against the base58 address taken as the public key and answers `is_valid` |
| `get_public_key` | Not a per-coin method: it takes no coin and answers `{public_key}`, the node's secp256k1 public key, whatever is active. The Solana ed25519 public key is exposed by no method other than as the address | not Solana-specific |
| `get_public_key_hash` | Likewise: `{public_key_hash}`, the 20-byte hash of that node key | not Solana-specific |
| `validateaddress` | Both: abnormal end | answers `{is_valid, reason?}`; see the finding below |
| `convertaddress` | Both: abnormal end | a placeholder as well |
| `get_raw_transaction` | Both: abnormal end (an inactive ticker answers `NoSuchCoin` first) | a placeholder as well |
| `my_balance` | SOL: `{coin, balance, unspendable_balance, address}` with `unspendable_balance` `"0"` and the wallet address. Token: the same members, `address` being the token's associated token account. A failed read, R40.6B.4's case included, answers the legacy error body | not examined |
| `my_tx_history` (legacy form) | Both: abnormal end | a placeholder as well |
| `my_tx_history` (versioned form) | Both: `NotSupportedFor` (HTTP 400, string: the ticker) | not examined |
| `set_required_confirmations` | Both: abnormal end; the coin has no confirmation setting to change or to report, and `set_requires_notarization` ends the same way | a placeholder as well |
| `get_enabled_coins` | Legacy form: an item `{ticker, address}` per active coin, the address as in `my_balance`. Versioned form: `{coins: [{ticker}]}` | not examined |
| `disable_coin` | A platform with active tokens, without `force_disable`: a normal answer `{coin, cancelled_orders, passivized: true}`; platform and tokens stay registered and the platform can be activated again (chapter 55 D55.6). **A token: abnormal end**, and the token stays listed. **A platform without tokens, or with `force_disable`: abnormal end**, after part of the removal has already happened | not examined |

Outside the list asked for, the same reading found these to end abnormally
for both kinds: `show_priv_key`, `get_trade_fee`, `sign_raw_transaction`
and `min_trading_vol` (chapter 60 finding F6). The reading was not
exhaustive (§40.6B.11).

> **Code-quality finding (informative; this repository's own code;
> trigger: apparent with little extra effort).** *Address validation is too
> narrow.* This repository's Solana address check accepts only text of
> exactly 44 characters that decodes to a point on the curve. A 32-byte
> public key encodes to between 32 and 44 base58 characters, so valid
> shorter addresses are refused; and an owner that is a program-derived
> address is off the curve by construction. Both withdraws run this check
> on `to`. Proposed fix: accept any base58 text that decodes to exactly 32
> bytes, and decide separately whether an off-curve recipient needs a
> warning.

> **Code-quality finding (informative; this repository's own code;
> trigger: apparent with little extra effort).** *This repository's SPL
> "max" withdraw is also wrong, differently.* It reports the full token
> balance in the answer but builds the transfer from the request's `amount`
> member; with `max` set and no amount given, the transaction moves nothing
> while the answer says the whole balance left. It also supports the
> classic Token program only, and answers a short SOL balance with
> `NotSufficientBalance` naming the platform coin where the reference
> answers `NotSufficientPlatformBalanceForFee`. D40.6B.3 and T40.6B.9
> cover both.

> *Reloaded status (clean side, 2026-11-18):* both parts of this finding are
> fixed in PR #425: `max` moves the token account's whole amount (D40.6B.3),
> and a short SOL balance answers `NotSufficientPlatformBalanceForFee`
> (T40.6B.9). The classic Token program remains the only one supported
> (D40.6B.4 (a)).

### 40.6B.6 Transaction history

**R40.6B.15** *Not implemented at any row read.* D40.1 stands. No row has a
Solana or SPL history: there is no history entry shape to report for SOL
or for SPL, and the reference reads no JSON-RPC method for history. What a
caller sees is: `tx_history: true` at activation ends the request
abnormally with nothing registered (R40.2.5's note); the legacy
`my_tx_history` ends abnormally and the versioned one answers
`NotSupportedFor` (§40.6B.5). The older implementation at the row
`v2.1.0-beta` had no history either. The registry branch records no check
of history (§40.6B.8).

### 40.6B.7 Swaps

**F40.6B.3** *The three rows carry no Solana swap.* For the platform coin
and for the SPL token alike, every swap operation is a placeholder: sending
and validating the dex fee; sending, validating, spending and refunding
either payment; finding a payment or its spend; extracting the secret;
deriving the swap key pair; negotiating a swap-contract address; waiting
for confirmations; the trade-fee estimates; the protocol information
exchanged while orders are matched; and the minimum trading volume
(R40.6A.1). Consequently there is, at these rows, no on-chain payment form,
no program address for any cluster, no instruction set, no account or
address-seed scheme, no expression of secret hash, lock time, claim or
refund, no dex-fee form, no swap version (neither the legacy protocol nor
the version-two one) that uses Solana, no swap message field and no watcher
support. An order cannot be placed (R40.6B.6). R40.6A.1's trading floors
are unchanged and are all there is.

**F40.6B.4** *One earlier release carried a partial proof of concept.* At
the row `v2.1.0-beta` an older Solana implementation, the lineage this
repository's own surface descends from, could build three transactions for
SOL only: lock a payment, claim it with the secret, and refund it. Each was
a single instruction to an on-chain program of the sibling swap-program
repository at its state `satomic-swap dev (as of 2024-06-18)`, naming a
32-byte SHA-256 secret hash, a lock time, an amount in lamports and the
counterparty; the two accounts a payment used had addresses that followed
from the lock time and the secret hash alone. The program's address was to
arrive through the generic swap-contract argument, yet the coin's own
swap-contract answer and its negotiation were placeholders, and no program
address was configured anywhere outside test material. The dex fee,
payment validation, the spend search, secret extraction and the
confirmation wait were placeholders, and so was every SPL swap operation.
It could not complete a swap. The whole implementation is gone at the row
`v2.2.0-beta`; the release rows from there to `v2.5.2-beta` carry no Solana
coin at all, and the reintroduced implementation of §40.0.1 is first
present, among the tabled release rows, at `v2.5.2-bebdc9a` (established by
file listing; that row was not read). Nothing of the proof of concept binds
Reloaded.

**F40.6B.5** *A public swap program exists; a public swap client does
not.* The sibling swap-program repository carries, at
`satomic-swap fix/htlc-correctness (as of 2026-10-28)`, a rewritten
hash-time-locked payment program with a checked-in interface description,
a read-me and a deployment document. By its own record it is unaudited,
deployed on devnet only, and not deployed on mainnet, where the same
program address is merely intended.

| Cluster | Program address | Verified |
| --- | --- | --- |
| devnet | `54mDjQz6jS9sggCV1P5GoUzqHaBEueGVZqmaMPLt2tDx` | Public-network observation on 2026-11-15 (chapter 01 R6): `getAccountInfo` returns an executable account owned by the upgradeable program loader |
| mainnet | none | Public-network observation on 2026-11-15: no account exists at that address |

The branch is a program-side artifact. Its documents speak of a framework
client that builds the program's instructions itself and of swaps without
watchers; **no state of the historical record contains such a client**, so
its messages, its dex-fee form, its swap versions, its confirmation rule
and its coin-configuration keys cannot be read anywhere public.

**Answer to the swap question.** Solana swaps are implemented at no row
read. There is no reference counterparty a Reloaded Solana swap could be
tested against, and no released behavior to be wire-compatible with.

**R40.6B.16** *No compatibility claim.* Reloaded MUST NOT state or imply
wire compatibility of Solana or SPL swaps with `v2.6.0-beta`,
`v3.0.0-beta` or `main (as of 2026-10-07)`; all three lack the capability.

**The public swap program's interface (dictated interop, conditional).**
The following is the interop surface of the program of F40.6B.5, read from
its published interface description. It is recorded because any client
that talks to that program must conform to it (R29/R31). It binds Reloaded
**only if** D40.6B.8 selects this program; until then it is information.
The symbols are quoted under chapter 01 R11's inter-operability class.
Whether the devnet deployment is the build this description belongs to was
not established (§40.6B.11).

| Instruction | Arguments, in order | Accounts, in order (s = signer, w = writable) | Discriminator (hex) |
| --- | --- | --- | --- |
| `lamports_payment` | `secret_hash` (32 bytes), `lock_time` (u64), `amount` (u64) | `sender` (s, w), `receiver`, `payment` (w), the system program | `9f43536c629db085` |
| `receiver_spend_lamports` | `secret` (32 bytes), `secret_hash`, `lock_time`, `amount` | `receiver` (s, w), `sender` (w), `payment` (w) | `3d53e089b4be6cf9` |
| `sender_refund_lamports` | `secret_hash`, `lock_time`, `amount` | `sender` (s, w), `receiver`, `payment` (w) | `0f4143dd6c3304d4` |
| `spl_payment` | `secret_hash`, `lock_time`, `amount` | `sender` (s, w), `receiver`, `mint`, `sender_token_account` (w), `payment` (w), `vault` (w), `token_program`, the associated-token-account program, the system program | `3c9f1eedb6f565aa` |
| `receiver_spend_spl` | `secret`, `secret_hash`, `lock_time`, `amount` | `receiver` (s, w), `sender` (w), `mint`, `receiver_token_account` (w), `payment` (w), `vault` (w), `token_program` | `7aea0623eae4b43e` |
| `sender_refund_spl` | `secret_hash`, `lock_time`, `amount` | `sender` (s, w), `receiver`, `mint`, `sender_token_account` (w), `payment` (w), `vault` (w), `token_program` | `cc9051672484978c` |

What a client depends on:

- **Instruction data.** The eight-byte discriminator, then the arguments in
  order at fixed width, integers little-endian. The discriminators are the ones
  the Anchor framework's public convention yields for these names (the
  first eight bytes of SHA-256 over `global:` followed by the name); this
  round recomputed all six.
- **The payment address is the commitment.** `payment` is a
  program-derived address of the swap program. Its seeds for SOL are the
  text `payment`, the sender key, the receiver key, the secret hash, the
  lock time and the amount; for a token they are the text `payment_spl`,
  the sender key, the receiver key, the mint key, the secret hash, the
  lock time and the amount. Lock time and amount enter as eight
  little-endian bytes each. `vault` is the associated token account of the
  `payment` address for the mint under `token_program`. A client finds or
  checks a payment by deriving this address from the terms it agreed.
- **Hash lock.** SHA-256 over the 32-byte secret; the hash is 32 bytes and
  is not narrowed to 20.
- **Time lock.** The program takes `lock_time` as whole seconds counted
  from the Unix epoch and tests it against the time the cluster itself
  reports. It gates the refund only: the receiver may claim at any
  time, the sender may refund once the clock has reached it. A value that
  does not fit a signed 64-bit integer is refused.
- **Who may act.** Only the receiver can claim and only the sender can
  refund; each must sign. No third party can do either, so the program has
  no watcher role.
- **Settlement leaves no account.** A claim or a refund closes the payment
  account and returns its rent to the sender; for a token it first moves
  everything the vault holds. A missing payment account therefore does not
  mean "never paid"; the transaction list of the payment address
  (`getSignaturesForAddress`) tells the two apart.
- **Where the secret is.** In the data of the claim instruction, the 32
  bytes after the discriminator, also when the claim is an inner instruction of
  another program's transaction. The program emits events (`PaymentSent`,
  `PaymentSpent`, `PaymentRefunded`), but they travel in the transaction
  log, which the node may truncate; a client must not rely on them for the
  secret.
- **Tokens accepted.** Classic Token mints always. A Token-2022 mint only
  when every extension it carries is one the program allows; a transfer
  fee is refused.
- **Errors**, numbered from 6000 in this order: `ZeroAmount`,
  `InvalidReceiver`, `InvalidSecret`, `LockTimeNotExpired`,
  `InvalidLockTime`, `InsufficientVaultBalance`, `LamportOverflow`,
  `TransferFeeNotSupported`, `MintExtensionNotSupported`.
- **Not in the program:** a dex fee, a swap identifier other than the
  payment address, and any record of the secret hash after settlement.

### 40.6B.8 What the registry branch adds

**F40.6B.6** *Content.* The row
`coins solana-integration (as of 2026-11-08)` adds four registry entries: a
native entry and a classic-program dollar-stablecoin token entry for
mainnet, and the same pair for devnet. It also adds a node list per
cluster, an integration document, registry tooling with its tests, and
generated fixtures: a four-entry wallet set and a "swap" set made of the
two devnet entries and one UTXO test coin. The branch holds no reference
implementation code. Its "swap fixtures" are coin lists only: they contain
no program address, no transaction, no message and no swap result, and so
add nothing to §40.6B.7.

**F40.6B.7** *What it establishes, against the three rows.*

| The registry branch | The three rows | Verdict |
| --- | --- | --- |
| Entries use `protocol` of type `SOLANA` with an empty `protocol_data`, and `SOLANATOKEN` with `platform`, `decimals`, `mint_address`, `program_id` | R40.1.2, R40.1.3 | agrees |
| Activation nodes are objects with `url` only | R40.2.1 | agrees |
| All four registry entries carry `wallet_only: true` | no Solana coin can trade (F40.6B.3) | agrees |
| The swap set carries the two devnet entries with `wallet_only: false` | changing the key makes nothing tradable (R40.6B.6) | **not supported by any public state**: the fixture presupposes a swap-capable build that is not in the historical record |
| Every entry carries `derivation_path`, a two-level hardened prefix under purpose 44 and SLIP-0044's Solana coin type | the key is never read and a global-HD login is refused (R40.6B.2, R40.6B.7) | **not supported by any public state**; harmless to the three rows |
| Native entries carry top-level `decimals` of 9, and the tooling requires it; the generated application form also copies a token's decimals to the top level | top-level `decimals` is never read (R40.6B.2) | no conflict; the key is for consumers other than the reference daemon |
| Entries carry `required_confirmations` and `avg_blocktime` | never read; the coin has no confirmation setting (R40.6B.2) | no conflict |
| The tooling admits two token programs only, the classic one and Token-2022, and none of the four entries is a Token-2022 token | any public key parses as `program_id` (R40.6B.3); Token-2022 fails at activation and at withdraw (R40.6B.5, R40.6B.11) | agrees on substance |
| The tooling identifies a cluster by `getGenesisHash` before trusting a node | activation never asks a node which cluster it serves; only `getHealth` (R40.1.5) | no conflict; see the finding below |
| What the branch verified against live nodes is limited to which cluster a node serves and whether the node reports itself healthy; sending, looking up past transactions and trading were not exercised | no history and no swap (R40.6B.15, F40.6B.3) | agrees |
| The tooling refuses a ticker with more than one hyphen | this repository's swap-statistics store, baseline behavior, splits a ticker at hyphens into a coin part and a platform part and ignores a third part | agrees with the baseline |

**Nothing in the document or the fixtures contradicts the behavior of the
three rows.** Two of their features (tradable devnet entries, a derivation
path) look ahead to capabilities that no public state has, which is
consistent with F40.6B.5.

> **Code-quality finding (informative; trigger: apparent with little extra
> effort).** Nothing in the reference ties a coin entry to a cluster.
> Activation does not ask the node which cluster it serves, and an entry
> activates the same way against a node of either cluster; the answer does
> not name the cluster. Proposed fix: let an entry or a
> request state the expected genesis hash and compare it with
> `getGenesisHash` at activation; with nothing stated, behave as today.

### 40.6B.9 Decisions

Each item states the observable requirement, the options and a
recommendation. None is decided here.

**D40.6B.1** *Answers for the placeholder methods (R40.6B.14).*
Requirement: a JSON answer for every method of §40.6B.5 on both Solana
kinds. Options: (a) refuse every operation that is a placeholder today
with one structured not-supported error; (b) answer where the answer is
plain and the code exists in this repository (`sign_message`,
`verify_message`, `validateaddress` with the finding of §40.6B.5 fixed),
make `disable_coin` and `set_required_confirmations` work, and refuse the
rest (`convertaddress`, `get_raw_transaction`, the legacy `my_tx_history`
until D40.1 is done) with a structured error. *Recommendation:* (b).
`disable_coin` has to succeed under either option, for a token and for a
platform; a wallet must be able to switch a coin off. The message-signing
form is Reloaded's own under either option, because the reference has none
to match.

> *Reloaded status (clean side, 2026-11-17):* option (b) is built for every
> method except `disable_coin`, which follows in its own change (PR #401):
> - `sign_message` / `verify_message` keep Reloaded's form: the message's
>   UTF-8 bytes as they are, no prefix and no pre-hash, and the 64-byte
>   ed25519 signature in base58, checked against the base58 address as the
>   public key; a token signs with its platform's key.
> - `validateaddress` accepts any base58 text that decodes to exactly 32
>   bytes (the finding of §40.6B.5 fixed) and gives a reason otherwise.
> - `set_required_confirmations` and `set_requires_notarization` store the
>   value per coin and the coin reports it (defaults 1 and false).
> - `show_priv_key` answers the 64-byte keypair (secret then public key) in
>   base58, the form common Solana wallets import (maintainer decision of
>   2026-11-15); a token answers its platform's. The key-export method
>   `get_private_keys` reports the same form for an active Solana coin in
>   its default mode; its offline derivation mode still refuses Solana.
> - `convertaddress`, `get_raw_transaction`, `get_trade_fee`,
>   `sign_raw_transaction` and the legacy `my_tx_history` answer a
>   structured error naming the coin; the versioned `my_tx_history` answers
>   `NotSupportedFor`, as the reference does.
> - `max_taker_vol` and `max_maker_vol` reach the coin's trade-fee
>   estimates, which answer a structured internal error naming the coin
>   (HTTP 500) instead of ending abnormally; refusing those two methods for
>   an untradable coin, as the trade previews do, is left to a separate
>   change because the refusal also covers Lightning.
> - The history loop and the history state of an activation answer without
>   doing anything; transaction history itself is package S10.
> - Every operation a swap would use answers an error naming the coin:
>   Solana swaps are not supported. Orders and trade previews are refused
>   earlier by the trading refusal of §40.6A; a swap that reaches the node
>   otherwise (an imported swap, its recovery, or a restart) gets those
>   errors instead of an abnormal end.

> *Reloaded status (clean side, 2026-11-18):* `disable_coin` answers for
> both Solana kinds (PR #413), completing option (b). A token, and a platform
> without tokens, are removed and can be activated again; a disabled token is
> also taken out of its platform's token records. A platform with active
> tokens is handled as every other family handles it today: the platform is
> removed and its tokens stay registered, without the passive state of
> chapter 43 R43.1E.6 and without `force_disable`, both of which wait for
> chapter 55 D55.6. A coin that is not active answers the refusal of chapter
> 43 R43.1E.3 in its extended shape. T40.6B.20 holds except its passive and
> forced clauses.

**D40.6B.2** *Balance of a token the wallet holds no account for
(R40.6B.4).* Options: (a) answer zero, reading the associated token
account derived with the token's `program_id`, which is also the address
R40.1.4 reports and the account a withdraw spends from; (b) copy the
reference's failure. *Recommendation:* (a). It also settles the Token-2022
balance finding of §40.6.

> *Reloaded status (clean side, 2026-11-18):* settled as option (a) by the
> maintainer's decisions of 2026-11-15 and built in PR #425. A token's balance
> is read from the associated token account derived from the wallet address,
> the mint and the token's `program_id`; when that account does not exist
> the balance is zero, and a node failure stays an error. Withdraws spend
> from the same account.

**D40.6B.3** *The SPL "max" withdraw (R40.6B.10 and both findings).*
Requirement: with `max`, the transaction moves the token account's whole
amount and the answer reports the amount the transaction moves. Options:
(a) that; (b) copy the reference. *Recommendation:* (a).

> *Reloaded status (clean side, 2026-11-18):* settled as option (a) and
> built in PR #425. With `max` the transfer carries the account's whole amount
> in the token's smallest units, and `total_amount` reports it. By the
> owner's choice of 2026-11-18, `AmountTooLow` names the coin's own minimum
> transaction amount as its threshold: one lamport for SOL and one smallest
> unit of the token's decimals for an SPL token (the `min_tx_amount` of
> §40.6A's status note), where the reference names one lamport for both.

**D40.6B.4** *Token-2022 (R40.6B.5, R40.6B.11).* Options: (a) until it is
supported, refuse a token whose `program_id` is the Token-2022 program at
activation with a structured error that says so; (b) support it: balance
and source account under the token's own program, the transfer through
that program, rent quoted for the real account size, and a stated rule for
mints with a transfer fee; (c) copy the reference (a node error at
activation, `InternalError` at withdraw). *Recommendation:* (a) for the
return of Solana, (b) as its own work package. The registry branch
publishes no Token-2022 entry, so (a) costs no coverage today.

> *Reloaded status (clean side, 2026-11-18):* settled as option (a) for the
> return of Solana and built in PR #425. A token whose `program_id` is the
> Token-2022 program is refused before any node is contacted, and nothing
> is registered: `enable_solana_with_tokens` answers
> `PlatformCoinCreationError` (`{ticker, error}`, the platform ticker, HTTP
> 500) and `enable_spl` answers `Internal` (string, HTTP 500), both with the
> text "<token>: Token-2022 tokens are not supported yet" (maintainer
> decision of 2026-11-18, the shapes Reloaded uses while TRON is switched
> off). Option (b) is a later package of the plan `solana-support`.

**D40.6B.5** *The `fee_details` object (R40.6B.12).* This repository
answers `{type, amount}`, the reference
`{type, fee_amount, rent_amount, total_amount}`, under the same `type`
value. Options: (a) the reference's four members for both Solana kinds;
(b) keep `amount`; (c) both sets of members in one object.
*Recommendation:* (a), because clients written for the reference read
those members and no Solana history has ever been stored in the other
shape. Choose (c) only if a client is found that reads `amount`, and then
check the stored-form reader, which tells the families' fee objects apart
by their members, not by `type`.

> *Reloaded status (clean side, 2026-11-18):* settled as option (a) and
> built in PR #425, for both Solana kinds and in the history decoder; the
> reader of stored fee objects tells the new shape apart by its
> `fee_amount` member, which no other family's fee object has.

**D40.6B.6** *A recipient that is a token account (second finding of
R40.6B.10).* Requirement: tokens must not land in an account owned by a
token account. Options: (a) accept owner addresses only, and refuse a `to`
that is an existing account owned by a token program with
`InvalidAddress`; (b) as (a), except that a `to` which is a token account
of the same mint is paid directly; (c) copy the reference.
*Recommendation:* (a) for the return of Solana; (b) can follow.

> *Reloaded status (clean side, 2026-11-18):* settled as option (a) and
> built in PR #425: a `to` that is an existing account owned by the classic
> Token or the Token-2022 program answers `InvalidAddress`, and no
> transaction is built.

**D40.6B.7** *Solana under a global-HD login (the consistency note of
§40.6B.3; continues D40.2).* Options: (a) follow chapter 05 R46 item 3 on
both activation surfaces: the legacy passphrase key, hence the address of
R40.6B.8's rule, and T40.8 is retired; (b) refuse on the compatibility
surface (T40.8) and activate on the primary one; (c) refuse on both, as
the reference does; (d) derive from the HD seed by SLIP-0010 under
SLIP-0044's Solana coin type, which gives new addresses, has no reference
to match at any public state, and needs its own design pass.
*Recommendation:* (a) now, since one passphrase then has one Solana
address under either login and the plan runs both surfaces over one
activation path; (d) only as a separate, later decision. Whichever is
chosen, chapter 05 R46 and T40.8 must be made to say the same thing.

> *Reloaded status (clean side, 2026-11-15):* by the maintainer's decision
> of 2026-11-15, Solana under a global-HD login is to be built with HD
> derivation in this cycle: the coin is to use an ed25519 key at its
> derivation path, with a typed refusal for a hardware wallet or a
> WalletConnect session. The path layout is still to be decided; chapter 05
> R46 and T40.8 are aligned in the change that implements it. Nothing of it
> is built yet.

> *Reloaded status (clean side, 2026-11-16):* built in PR #392: the layouts
> `bip44_change` (default) and `bip44`, chosen per request, nothing stored
> (§40.1A); chapter 05 R46 and T40.8 carry matching status notes.

**D40.6B.8** *The swap program (F40.6B.5).* Options: (a) target the public
program of F40.6B.5 when Solana swaps are designed; (b) design or deploy
another; (c) decide nothing now. *Recommendation:* (c). The lane is
wallet-first, no reference client exists to interoperate with, and the
program is by its own record unaudited and absent from mainnet. Revisit
when a reference client is public or the program is on mainnet.
R40.6B.16 holds meanwhile.

> *Reloaded status (clean side, 2026-11-15):* by the maintainer's decision
> of 2026-11-15, nothing is decided on the swap program; the Solana lane is
> wallet-only first.

### 40.6B.10 Acceptance tests

These rows are for the implementer once the section is cleared and the
decisions are taken; a row that depends on a decision names it. "Answers"
means a JSON answer, never an abnormal end.

*Activation forms.*

T40.6B.1 A `SOLANATOKEN` entry without `program_id`:
`TokenProtocolParseError`, HTTP 500, `{ticker, error}`, from the platform
method (inline) and from the token method. The same for a missing
`platform`, `decimals` or `mint_address`, and for `decimals` of 256.

T40.6B.2 A platform entry of type `SOLANA` with `protocol_data: {}` and
one with extra members inside `protocol_data` both activate. An entry
without `protocol_data` activates as well once both configuration forms
are accepted (R40.6B.3 item 1); the reference's refusal of it is not
tested for.

T40.6B.3 A token entry with top-level `decimals` of 9 and
`protocol_data.decimals` of 6: balances and withdraw amounts scale by 6. A
platform entry with top-level `decimals` of 6: SOL still scales by 9.

T40.6B.4 A platform entry with `derivation_path` present, absent, and set
to text that is not a path: the same address in all three cases.

T40.6B.5 The single-key vector of R40.6B.8: that passphrase gives that
address.

T40.6B.6 (D40.6B.7) A global-HD login: the outcome the decision selects,
the same on every surface the decision names.

T40.6B.7 (D40.6B.2) A token whose mint exists and for which the wallet has
no token account: activation succeeds with balance `"0"`, on both methods;
`my_balance` answers `"0"`.

T40.6B.8 (D40.6B.4) A token entry with the Token-2022 `program_id`: the
outcome the decision selects; under option (a), a structured refusal at
activation and no registered coin.

*Withdraw.*

T40.6B.9 SPL withdraw to a wallet that has no account for the token, with
enough SOL: the transaction creates the recipient's associated token
account and transfers; `rent_amount` is positive and `total_amount` is
`fee_amount` plus `rent_amount`. The same with SOL below fee plus rent:
`NotSufficientPlatformBalanceForFee`, HTTP 400,
`{coin, available, required}` with the platform ticker in `coin`.

T40.6B.10 SPL withdraw to a wallet that already has the account:
`rent_amount` is `"0"` and the transaction has the transfer only.

T40.6B.11 (D40.6B.3) SPL withdraw with `max` and no `amount`, from a
balance with a fractional part: the transfer instruction carries the token
account's whole amount, and `total_amount` equals it.

T40.6B.12 SOL withdraw: `max` gives balance less fee; an amount plus fee
above the balance gives `NotSufficientBalance`; an amount below one
lamport gives `AmountTooLow`; `to` that is not 32 bytes of base58 gives
`InvalidAddress`; a `fee` member changes nothing.

T40.6B.13 (D40.6B.5) `fee_details` of a SOL and of an SPL withdraw has the
members the decision selects; under option (a) exactly `type`,
`fee_amount`, `rent_amount`, `total_amount`, the last three decimal
strings in SOL.

T40.6B.14 (D40.6B.6) SPL withdraw whose `to` is an existing token account:
the outcome the decision selects; in no case a transaction that creates a
token account owned by `to`.

T40.6B.15 `send_raw_transaction` with the `tx_hex` of a withdraw answers
`{tx_hash}` equal to the withdraw's `tx_hash`.

*Per-method answers (R40.6B.14, D40.6B.1), each on SOL and on an SPL
token.*

T40.6B.16 `sign_message`, `verify_message`, `validateaddress`,
`convertaddress`, `get_raw_transaction`, `set_required_confirmations`,
`set_requires_notarization`, `show_priv_key`, `get_trade_fee`,
`sign_raw_transaction`, `min_trading_vol` and both forms of
`my_tx_history`: each answers. Where D40.6B.1 selects a refusal, the
answer is a structured error naming the coin.

T40.6B.17 (D40.6B.1 option (b)) `validateaddress` answers valid for a
32-byte address of 43 characters and for one of 44, and invalid for text
that does not decode to 32 bytes.

T40.6B.18 (D40.6B.1 option (b)) `sign_message` followed by
`verify_message` with the same message and the coin's address answers
`is_valid: true`; a changed message answers `false`.

T40.6B.19 `my_balance` on SOL and on a token answers `coin`, `balance`,
`unspendable_balance` and `address`; `get_enabled_coins` lists both in its
legacy and its versioned form.

T40.6B.20 `disable_coin` on a token answers and removes it from
`get_enabled_coins`; on a platform with no tokens likewise; on a platform
with active tokens and no `force_disable` it answers in an orderly way,
as chapter 55 D55.6 settles (the reference answers `passivized: true` and
lets the platform be activated again); with `force_disable` it answers and
removes the platform and its tokens.

T40.6B.21 `tx_history: true` at activation, on either surface: the request
answers and the coin is registered (T40.7 restated for both surfaces).

T40.6B.22 `setprice`, `buy`, `sell` and `trade_preimage` naming a Solana
coin whose entry has `wallet_only: false` answer a structured refusal (the
as-built note of §40.6A, restated for the configuration value).

### 40.6B.11 Not established; verification notes and provenance (section-scoped)

- *Not established.*
  - Nothing here was obtained by running the reference. The reference
    facts were read at the rows named; node answers were observed on the
    public network; this repository's column was read in this tree.
  - The abnormal-end outcome on the WASM target.
  - The exact state after an abnormally ended `disable_coin`; the table
    gives what the reading shows and no more.
  - Whether any background activity of an active Solana coin reaches a
    placeholder without a request. The reading covered the methods of
    §40.6B.5, not every path.
  - Anything about a reference build newer than `main (as of 2026-10-07)`.
    The registry branch and the swap-program branch are both newer than
    that row and both presuppose capabilities it lacks. If such a build
    exists it is not public: its activation, its key derivation under an
    HD login, its swap messages, its dex-fee form, its history and its
    configuration keys could not be established, and no statement in this
    section should be read as describing it.
  - Whether the devnet deployment of F40.6B.5 is the build of the interface
    description recorded here. The program's deployment record predates the
    branch's last commit, which changed program behavior and added an error
    value, and names a build that is not a commit of the public repository.
  - No history entry shape, no address under a global-HD login and no swap
    message field exist to be established (R40.6B.15, R40.6B.7, F40.6B.3).
  - Whether any client reads this repository's `fee_details.amount` for
    Solana (D40.6B.5).
  - One older, unmerged work-in-progress branch state of the historical
    record that touches ed25519 HD derivation was not read: it predates all
    three rows, never reached a release, and is not tabled.
- *Re-verifying the public-network facts (chapter 01 R6).* On a public
  devnet endpoint, `getAccountInfo` for the address of F40.6B.5 returns an
  executable account; on a public mainnet endpoint it returns none.
  `getTokenAccountBalance` for an address that has no account returns a
  JSON-RPC error, not a zero amount; `getTokenAccountsByOwner` for an owner
  with no account of an existing mint returns an empty list; the same call
  with a mint address that has no account returns a JSON-RPC error. All
  were observed on 2026-11-15.
- *Sibling-repository citations (chapter 01 R14).* (1) The public
  coins-registry repository `GLEECBTC/coins`, branch `solana-integration`;
  earliest commit containing the integration document, the node lists and
  the fixtures `83ed4b75564a1e097d774d64ac5343b161fcb44c` (2026-11-08).
  The branch carries no license file; its text is treated as reference-side
  material, and no sentence or example value of the document or the
  fixtures is carried here. Configuration key names are the interface;
  SLIP-0044's coin type is a public registry value. (2) The public
  swap-program repository `KomodoPlatform/satomic-swap` (the same
  references are served under `GLEECBTC/satomic-swap`), branch
  `fix/htlc-correctness`; earliest commit containing the rewritten program
  `4414761b74fc5868c41330db59bce9d18ba88e7c`, the interface description
  `5ccf5cf4629dcda23804ea2734548017e0528af9` and the devnet deployment
  record `72af9b6aa8032d7fd64993890455285502f4b2c0` (all 2026-10-21);
  license MIT by the program crate's package metadata (the repository root
  carries no license file at that branch). The instruction, account,
  argument, event and error names are admitted by chapter 01 R11's
  inter-operability class; the deployment facts by the observation above.
  (3) The same repository, branch `dev`; the first-generation instruction
  set first appears in `70d2fae63baad3a46e6957117fc8d4c97da0c06d`
  (2024-03-28); no license statement at that state, so F40.6B.4 describes
  function only. Neither repository is part of the forbidden corpus.
- *Public specifications cited (chapter 01 R12/R13).* Solana JSON-RPC API
  (`getHealth`, `getBalance`, `getAccountInfo`, `getTokenAccountsByOwner`,
  `getTokenAccountBalance`, `getLatestBlockhash`, `getFeeForMessage`,
  `getMinimumBalanceForRentExemption`, `sendTransaction`,
  `getGenesisHash`, `getSignaturesForAddress`); the SPL Token, Token-2022
  and Associated Token Account program specifications; RFC 8032 (ed25519);
  BIP-39 (the test mnemonic); SLIP-0044 (coin type); SLIP-0010; the Anchor
  framework's published instruction-discriminator convention.
- *States read for this section:* `v2.6.0-beta`, `v3.0.0-beta`,
  `main (as of 2026-10-07)`; `v2.1.0-beta` for F40.6B.4 and R40.6B.15's
  last sentence; presence or absence of a Solana coin, by file listing
  only, at `v2.2.0-beta`, `v2.3.0-beta`, `v2.4.0-beta`, `v2.5.0-beta`,
  `v2.5.1-beta`, `v2.5.2-beta` and `v2.5.2-bebdc9a`; the sibling rows
  `coins solana-integration (as of 2026-11-08)`,
  `satomic-swap dev (as of 2024-06-18)` and
  `satomic-swap fix/htlc-correctness (as of 2026-10-28)` (chapter 02
  §2.15A). No other state of the historical record was read for this
  section.
- *Cross-chapter check.* Read against this section: chapter 02 §2.15A
  (three rows added, cleared with this section); chapter 05 (R46 item 3
  and the hardware-source table: the consistency note of §40.6B.3);
  chapter 49 (the fee-policy note: agrees with R40.6B.9); chapter 55
  (§55.3 Solana rows and D55.6: agree); chapter 60 (row P1, R60.6.5,
  finding F6: agree, extended by R40.6B.14); chapter 21 §21.11B (the form
  followed for a sibling swap program). No cleared sentence of any chapter
  was edited.
- *Clearance:* passed the KDF Dirty Gate on 2026-11-15, measure 0.01, no
  findings (chapter 01 R37-R41); the attestation is in §40.12.

## 40.7 Tests

T40.1 Both `experimental::` methods are routed on native and WASM builds.

T40.2 A `SOLANATOKEN` configuration with `mint_address` and `program_id` parses;
one with an invalid base58 key yields `TokenProtocolParseError` (HTTP 500,
`{ticker, error}`).

T40.3 Unresponsive nodes with one inline token: HTTP 502, `Transport`, string
`error_data`. With no inline tokens: HTTP 500, `PlatformCoinCreationError`,
`{ticker, error}`.

T40.4 `experimental::enable_solana_token` with unresponsive nodes: HTTP 500,
`Transport`, string `error_data`.

T40.5 `PlatformCoinMismatch` (HTTP 400, no `error_data` key) for an inline token
whose `platform` differs.

T40.6 The platform result carries exactly `ticker`, `address`, `current_block`
(integer), `balance` and `tokens_balances` (ticker-keyed map), all balances as
strings.

T40.7 `tx_history: true` does not abort the request (R40.2.5).

T40.8 An HD-wallet context yields HTTP 500 `PlatformCoinCreationError`.

> **Reloaded status (clean side, 2026-11-16).** Retired for Reloaded;
> T40.1A.1-T40.1A.3 replace it. PR #392.

T40.9 (beta) While `SOLANA_ACTIVATION_AVAILABLE` is `false`, a well-formed
`enable_solana_with_tokens` request answers HTTP 500 `PlatformCoinCreationError`
and `enable_spl` answers HTTP 500 `Internal`, each with "Solana support is not
available in this build"; legacy `enable` / `electrum` answer the same text; no
Solana coin is registered.

T40.10 *Host-only naming and credentials (R40.5A.1).* A node client built from
a URL with a path segment and a query string that carry a key, pointed at a
closed port: the transport error of a request and the debug forms of the
client and its pool contain the host-only form and none of the path or query.
A unit test; it needs no activation and runs while Solana is switched off.

## 40.8 Deferred Work

D40.1 Solana transaction history.

D40.2 HD-wallet, hardware-wallet and WalletConnect Solana policies.

> **Reloaded status (clean side, 2026-11-16).** HD done (§40.1A, PR #392);
> hardware-wallet and WalletConnect signing remain deferred.

D40.3 Add the reference compatibility surface of §40.1-§40.3 (the two
`experimental::` methods on all targets and the `SOLANA` / `SOLANATOKEN`
coin-configuration forms) alongside reloaded's primary surface, and bind the
primary surface's own wire shapes in this chapter. Deferred to a dedicated
session; see `docs/plans/solana-support.md`.

> **Reloaded status (clean side, 2026-11-15).** The configuration forms are
> read since PR #361 (status note after R40.1.3); the primary surface's wire
> shapes are bound in §40.3A (PR #371). The `experimental::` methods remain.

> *Reloaded status (clean side, 2026-11-19):* the two `experimental::`
> methods are built for native targets (PR #426; status notes after R40.1.1,
> R40.2.5 and R40.3.4). Their WASM routing is the WASM package of
> `docs/plans/solana-support.md`.

> *Reloaded status (clean side, 2026-11-19, browser builds):* the WASM part
> is built as well (PR #430; status note after R40.1.1); the compatibility
> surface is now routed on all targets.

D40.4 Switching Solana back on (planned for the next beta; maintainer decision
2026-10-26): set `SOLANA_ACTIVATION_AVAILABLE` to `true`, re-enable the Solana
docker tests listed in `docs/DISABLED_TESTS.md` (group 6) and retire T40.9.
Part of `docs/plans/solana-support.md`. R40.5A.1 has to hold before the switch
is turned back on.

## 40.9 Baseline Verifications

V40.1 The GPLv2 baseline routes `enable_solana_with_tokens` and `enable_spl`
with the shapes listed in §40.6 items 3-5.

## 40.10 Acceptance criteria

- Both `experimental::` methods are routed on all targets with the request
  fields, results, errors, `error_data` shapes and HTTP statuses of §40.2-§40.3.
- The `SOLANA` / `SOLANATOKEN` coin-configuration shapes of R40.1.2-R40.1.3 are
  accepted.
- The unresponsive-node outcomes of R40.2.4 and R40.3.4 hold.
- SOL and SPL balances and the block height are read via Solana JSON-RPC
  (§40.4); SOL and SPL transfers can be built, signed, broadcast and decoded
  (§40.5).
- The Tests of §40.7 pass.

## 40.11 External References

- Solana JSON-RPC API (`getHealth`, `getBalance`, `getBlockHeight`,
  `getAccountInfo`, `getTokenAccountsByOwner`, `getTokenAccountBalance`).
- SPL Token, Token-2022 and Associated Token Account program specifications.
- Komodo DeFi Framework API documentation (generic mmrpc-2.0 envelope; no Solana
  page exists).

## 40.12 Provenance Footer

- *Inputs:* the project's own revision history and current tree (the
  shipped Solana coin module and SPL-token activation wiring, §40.6); the
  GPLv2 baseline dispatcher; the published Solana JSON-RPC method set, the SPL
  Token / Token-2022 / associated-token-account model, ed25519 keypairs, base58
  address encoding, and lamport units;
  [Chapter 55](55-coin-activation-wire-contract.md).
- *Permitted-input classes used:* baseline/as-built source (the shipped
  Solana/SPL coin module); external public specification (Solana JSON-RPC, SPL
  programs, ed25519/base58/lamport conventions); cross-chapter contracts
  (Chapters 36, 55); interop / wire-and-API-bound reuse (R29/R31) for the
  dictated method strings, JSON field names, coin-configuration keys and error
  discriminants.
- *Sibling-allowlist consultations:* [Chapter 36](36-tendermint-v2-activation-rpcs.md),
  [Chapter 55](55-coin-activation-wire-contract.md).
- *Forbidden corpus:* (addition of 2026-10-25, §40.6A, cleared by the
  Dirty Gate with [Chapter 60](60-trading-floors-and-refusal-contract.md)
  on 2026-10-25, attestation in its §60.12:
  also consulted at the same two tags, only to establish that the
  minimum trading volume is unimplemented for SOL and SPL tokens and that
  their minimum transaction amount is one lamport; no discretionary
  expression reproduced.) Otherwise consulted, under the chapter-01 two-team clean-room
  workflow, at the GLEEC KDF tags `v3.0.0-beta` (reference) and `v2.6.0-beta`
  (legacy comparison), for upstream wire parity of the Solana activation
  methods only -- specifically the method strings and targets of R40.1.1, the
  coin-configuration protocol shapes of R40.1.2-R40.1.3, the key-context
  restriction of R40.1.4, the request fields, results, failure ->
  `error_type` mapping, `error_data` shapes and HTTP statuses of §40.2-§40.3,
  and the JSON-RPC methods named in §40.4. No source text, private identifier,
  internal decomposition, or error/log string literal was carried into this
  chapter; the code-quality findings of §40.6 are restated as behaviour.
- *Clean-context addition (2026-10-28, forbidden corpus not consulted):*
  §40.5A (R40.5A.1 and its status note), T40.10 and the last sentence of D40.4
  are Reloaded-original. They rest on the maintainer decision of 2026-10-26
  (host-only node naming everywhere, except the RPC wire dump, which keeps its
  own secrets switch) and on this repository's own code. They take no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Dirty-side addition of 2026-11-15 (§40.6B; Spec Reader round 0.5.0-4;
  **KDF Dirty Gate: PASS on 2026-11-15, measure 0.01, no findings**):*
  §40.6B and this entry passed the gate under chapter 01 R40; the
  attestation below closes this item. *Forbidden corpus:* consulted
  under the chapter-01 two-team clean-room workflow at the rows
  `v3.0.0-beta`, `v2.6.0-beta` and `main (as of 2026-10-07)` of chapter 02
  §2.15A, for the observable behavior of an active Solana coin and SPL
  token only: the configuration keys read, the key source per login, the
  withdraw and broadcast contract with its `fee_details` object and error
  mapping, the answer or abnormal end of the wallet methods listed in
  §40.6B.5, and the absence of history and of swaps; and at the row
  `v2.1.0-beta` only to establish that an earlier release carried a partial
  swap proof of concept. *Reference-side material, source class "sibling
  public repository without a license statement":* the row
  `coins solana-integration (as of 2026-11-08)` (registry entries,
  integration document, registry tooling, fixtures) and the row
  `satomic-swap dev (as of 2024-06-18)`; read by the dirty-side author
  only, described by function, nothing quoted. *Sibling public repository
  whose program crate declares the MIT license in its package metadata
  (the repository root carries no license file at that branch):* the row
  `satomic-swap fix/htlc-correctness (as of 2026-10-28)`, for the published
  interface of a swap program (chapter 01 R3's published
  application-binary-interface class and R4), cited under R14 in
  §40.6B.11. *Public-network observation (chapter 01 R6):* devnet and
  mainnet JSON-RPC answers of 2026-11-15. *This repository's own tree
  (chapter 01 R1/R7):* the last column of §40.6B.5, the two findings on
  this repository's code, and the test vector of R40.6B.8, which was
  computed, not read. No source text, private identifier, internal
  decomposition, error or log string literal, test name or test constant
  was carried into §40.6B; findings are restated as behavior.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-15, measure
  0.01 on chapter 01 R35's discretionary-expression basis, judged per token,
  no findings (chapter 40: 0.01; chapter 02: 0.00; 0.00 against the
  historical record); the section passed two full passes, the second after
  five passages were reworded. Reference states (chapter 02 R13): no
  finding; every state name used has a row, each row's identifier is the
  state compared, and the three new rows are well-formed. Extent: the Solana
  coin and SPL token subsystem of the historical record (coin, token, node
  client, both activation paths, test file: 7 files) read in full at
  `v3.0.0-beta`; the same files at `v2.6.0-beta` and `main (as of
  2026-10-07)` by difference; the earlier Solana and SPL implementation at
  `v2.1.0-beta` (10 files: swap operations read, the remainder compared
  mechanically); the request dispatcher's method names and the
  coin-deactivation handler by targeted reading; every text file of those
  four trees mechanically; file listings at `v2.2.0-beta`, `v2.3.0-beta`,
  `v2.4.0-beta`, `v2.5.0-beta`, `v2.5.1-beta`, `v2.5.2-beta` and
  `v2.5.2-bebdc9a`; the sibling rows `coins solana-integration (as of
  2026-11-08)` (18 files changed by the branch; integration document read in
  full), `satomic-swap dev (as of 2024-06-18)` (16 files) and `satomic-swap
  fix/htlc-correctness (as of 2026-10-28)` (19 files; read-me read in full,
  program crate and interface description compared in full). Treatment of
  the siblings: the registry branch and the older swap-program state carry
  no license and were treated as protected expression; of the newer
  swap-program state the checked-in interface description is published
  interface, the program crate is MIT-declared expression admissible with
  the R14 citation the section carries, and the read-me, deployment
  document, tests and scripts were held to the protected-expression
  standard. Method: reading; mechanical comparison of shared word runs
  (three, four and five words) between the gated lines and each state;
  per-token forced-versus-free classification of every quoted token;
  recomputation of the six instruction discriminators and of the test
  vector; comparison of string literals and comments; the section embeds no
  code blocks, so the repository's clean-room gate tool was not run. Not
  covered: the accuracy of the behavioral statements, the public-network
  observations, and whether each R14 identifier is the earliest commit. The
  test vector's values occur nowhere in the compared material.
- *Status notes added 2026-11-15 -- after D40.6B.7 and D40.6B.8 (§40.6B).*
  Reloaded-original, clean side: they record the maintainer's decisions of
  2026-11-15 on Solana under a global-HD login and on the swap program; they
  take no forbidden-class input, so chapter 01 R38 is not engaged and no
  gate pass is required.
- *Clean-context addition (2026-11-15, forbidden corpus not consulted):* the
  Reloaded status notes after R40.1.3 and after §40.6 item 7 record the state
  of this repository's own code. They take no forbidden-class input.
- *Clean-context addition (2026-11-15, forbidden corpus not consulted):*
  §40.3A (R40.3A.1-R40.3A.5, T40.3A.1-T40.3A.3) and the status notes after
  §40.6's decision paragraph, after R40.6B.2 and after D40.3 bind Reloaded's
  own activation methods from this repository's code and the maintainer's
  decision of 2026-11-15 to map their failures as §40.2-§40.3 do. They take
  no forbidden-class input beyond the cleared rules they cite.
- *Clean-context addition (2026-11-16, forbidden corpus not consulted):*
  §40.1A (R40.1A.1-R40.1A.7, T40.1A.1-T40.1A.3) and the status notes after
  R40.1.4, T40.8, D40.2 and D40.6B.7 rest on the maintainer's decisions of
  2026-11-15, the public SLIP-0010, SLIP-0044 and BIP-39 specifications, and
  this repository's own code. The vectors were computed, not read.
- *Clean-context addition (2026-11-17, forbidden corpus not consulted):*
  the status notes after §40.6A's as-built note and after D40.6B.1 record
  this repository's own code and the maintainer's decisions of 2026-11-15.
- *Clean-context addition (2026-11-18, forbidden corpus not consulted):*
  the status note on `disable_coin` after D40.6B.1 records this repository's
  own code and chapter 43 §43.1E as cleared.
- *Clean-context addition (2026-11-18, forbidden corpus not consulted):*
  the status notes on the withdraw, the balance, Token-2022 and the
  mint-owner check (after R40.3A.3, after §40.6's gap list, after R40.6B.11,
  after the last finding of §40.6B.5 and after D40.6B.2 to D40.6B.6) record
  this repository's own code and the maintainer's decisions of 2026-11-15
  and 2026-11-18.
- *Clean-context addition (2026-11-19, forbidden corpus not consulted):*
  the status notes after R40.1.1, R40.1.5, R40.2.5, R40.3.4, R40.3A.3 and
  D40.3 record this repository's own code and the maintainer's decisions of
  2026-11-15, 2026-11-18 and 2026-11-19. They take no forbidden-class input
  beyond the cleared rules they cite.
- *Clean-context addition (2026-11-19, forbidden corpus not consulted):*
  the browser-build status notes after R40.1.1, R40.1.3 and D40.3 record
  this repository's own code. They take no forbidden-class input.
- *Scoped addition (2026-11-21, Spec Reader round 0.5.0-20) -- §40.2A
  (the shortest form, R40.2A.1 -- R40.2A.5, the error members, D40.2A.1,
  T40.2A.1 -- T40.2A.4) and the status note after R40.2.3; the section's
  own head note is its provenance record, and this item carries the
  clearance only.* **KDF Dirty Gate: PASS on 2026-11-21, measure 0.00, no
  findings** (chapter 01 R40; the attestation closes this item).
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-21, measure
  0.00, no findings, in a main pass and a delta pass after one
  informative note was deleted (the main pass measured 0.03, from that
  note only; the delta pass compared bytes: the status note after R40.2.3
  identical, §40.2A equal to the first-passed text without that note and
  one blank line). *Extent:* the Solana coin subsystem -- the SOL and SPL
  balance reads, the SOL and SPL withdraw amount and fee formation, the
  withdraw error members and the two Solana activation surfaces -- at the
  rows `v3.0.0-beta`, `v2.6.0-beta` and `main (as of 2026-10-07)`, seven
  files per state; on the candidate side, the two additions only.
  *Method:* reading plus mechanical comparison (every identifier-shaped
  token of the additions judged forced or free); the repository's
  clean-room gate tool was not applicable because the additions embed no
  code blocks. *Measure:* 0.00, on the discretionary-expression basis.
- *Clean-context addition (2026-11-21, forbidden corpus not consulted):*
  the status note after D40.2A.1 records this repository's own code and
  the maintainer's decision of 2026-11-21 on the text of withdraw sums. It
  takes no forbidden-class input beyond the cleared rules it cites.
