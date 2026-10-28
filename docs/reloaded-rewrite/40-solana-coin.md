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

R40.1.4 **Keys and addresses.** The Solana keypair shall be the ed25519 keypair
whose 32-byte seed is the wallet's single-key (Iguana) secp256k1 private-key
secret; the platform address is the base58 encoding of its public key. Only the
single-key context is supported: an HD-wallet, hardware-wallet or WalletConnect
context fails platform activation (`PlatformCoinCreationError`). A token's
address is the associated token account of the platform address for
(`mint_address`, `program_id`), per the SPL associated-token-account derivation.

R40.1.5 **Node selection.** Each request that needs the chain selects, in list
order, the first node whose Solana JSON-RPC `getHealth` succeeds and uses it;
when none succeeds the operation fails (mapped per §40.2 / §40.3).

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

> **Status (2026-10-28).** Not implemented. In this repository a transport
> failure embeds the lower layer's text, which carries the node's full URL,
> and the node client derives a debug form that prints the URL.

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

> **Decision recorded (informative).** The baseline-inherited
> `enable_solana_with_tokens` / `enable_spl` methods stay routed as the primary
> surface (see the maintainer decision at the top of this chapter); in this
> beta they refuse every request while `SOLANA_ACTIVATION_AVAILABLE` is
> `false`, and the decision applies again when Solana returns. Items 1-6
> above are therefore read as "the compatibility surface is not yet added", not
> as defects of the primary surface; item 6's error-mapping differences apply
> to the compatibility surface and are re-evaluated for the primary surface in
> the Solana plan. Release scope is tracked in `UPSTREAM-PARITY-GAPS.md`.

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

D40.3 Add the reference compatibility surface of §40.1-§40.3 (the two
`experimental::` methods on all targets and the `SOLANA` / `SOLANATOKEN`
coin-configuration forms) alongside reloaded's primary surface, and bind the
primary surface's own wire shapes in this chapter. Deferred to a dedicated
session; see `docs/plans/solana-support.md`.

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
