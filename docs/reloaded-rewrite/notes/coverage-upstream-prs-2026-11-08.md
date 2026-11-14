# Coverage note — three unmerged reference pull requests (2026-11-08)

**Kind:** informative coverage note, not a chapter; it binds nothing (see
the [notes README](README.md)). **Clearance:** the reference-derived
passages of this note (§0, §A, §B, §C.2) passed the KDF Dirty Gate on
2026-11-08, discretionary-similarity 0.05 (record in §D).

> **Purpose.** The maintainer asked, on 2026-11-08, whether the *topics*
> resolved by three pull requests opened against the reference shortly
> before it was archived are covered by this repository's code and CRD —
> not whether the parked branches of the same names in this repository
> should be revived. This note identifies the three reference states by
> their chapter 02 §2.15A row names, enumerates the behaviors each adds,
> and marks each topic `COVERED` (where), `PARTLY` (what is missing),
> `NOT COVERED`, or `DIVERGED BY DECISION` (which decision). Where a
> reference pull request was itself incomplete or contested, that is
> recorded as a behavior fact only.
>
> **Reference states read for this note** (chapter 02 §2.15A row names):
> `feat/tron-swap-support (as of 2026-04-23)`, `feat/tron-gasfree (as of
> 2026-05-04)` and its base `dev (as of 2026-03-16)`,
> `tendermint-priority-fee (as of 2025-07-24)` and its base `dev (as of
> 2025-07-18)`, `v3.0.0-beta` (one validation path only, §A.18 below),
> and the public pull-request index of the historical record (titles,
> dates, head branch names, open/merged/draft state, review counts).
> Chapter text written from the branch-tip rows describes the state at
> that row and nothing later; none of these rows is a released
> designation. References to chapter 21 §21.11B and chapter 18 §18.11
> point to gated sections not yet on `dev`; they resolve when those
> sections merge.

---

## 0. Identification of the three items

| Item | What the maintainer named | What the historical record holds | Row name |
| --- | --- | --- | --- |
| A | TRON swap support | A branch of that name whose single commit over `dev` adds **planning documents only** (seven plan/research notes and two review-reply notes; no source file). No pull request was ever opened from it, and no other branch or pull-request head of the record carries a TRON swap client (the index was searched by title and head name back to mid-2025: the only TRON pull requests are the two merged activation/withdraw ones of early 2026, a merged account-creation-fee fix, and item B). The remote tip is identical to the tabled row; there is **no later state**. | `feat/tron-swap-support (as of 2026-04-23)` (already tabled; analyzed in [Chapter 21](../21-tron-integration.md) §21.11B F21.B4, batch 0.4.0-6) |
| B | TRON GasFree fee delegation | A branch of that name that is the head of an **open, unmerged draft pull request** against `dev` (opened 2026-03-23, last pushed 2026-05-04, in conflict with `dev` at archive time). Nine commits over its base, about 4.6 k lines added across 49 files. Review state: no review verdict was ever recorded; zero inline review comments; one comment from outside the maintainer team (a wording request, §B.3.6). | `feat/tron-gasfree (as of 2026-05-04)` (row added by this pass) |
| C | Tendermint activation features (custom tokens, HD derivation path, WalletConnect / key-less signing, Trezor) | **No such unmerged pull request exists.** Every feature in that list was merged into the reference between 2024-05 and 2025-06 and is part of `v3.0.0-beta`; [Chapter 36](../36-tendermint-v2-activation-rpcs.md) (Dirty-Gate-passed) already binds them from that designation and `docs/plans/tendermint-activation-features.md` already tracks the implementation gap. The only **unmerged Tendermint** pull request in the record is a *priority transaction fee* draft (opened 2024-11-26, last pushed 2025-07-24, two inline review comments discussing the shape of a request field, in conflict with `dev`, never approved). It is enumerated in §C.2 as the nearest candidate, so the maintainer can decide whether it is the item meant. | `tendermint-priority-fee (as of 2025-07-24)` (row added by this pass) |

---

## A. TRON swap support — topics of the planning documents vs chapter 21 §21.11B (reference-derived; passed the KDF Dirty Gate on 2026-11-08)

The row adds no behavior; its documents plan one. The table lists the
subject areas those documents settle or raise (as topics, not as their
content) and where each stands in the gated CRD. Chapter 21 §21.11B
(batch 0.4.0-6, Dirty Gate PASS 2026-11-08, measure 0.01, not yet on
`dev` at the time of writing) is the comparison target; its decisions
are `D21.B1`–`D21.B9`.

| # | Topic (behavior area) | Coverage | Where / what is missing |
| --- | --- | --- | --- |
| A.1 | A Tron-VM build of the V1 HTLC contract with a 32-byte SHA-256 hash lock instead of the EVM contract's 20-byte lock | COVERED (as a decision) | §21.11B.2 (public contract, ABI and semantics), D21.B2 (contract choice), D21.B4 (hash arm). The documents reach the same choice; the chapter records it as Reloaded's decision because no released reference client exists (F21.B1). |
| A.2 | Contract tooling, Nile testnet deployment record, mainnet deployment as a release gate | COVERED (fact) / decision open | F21.B5 records the Nile deployment and the absence of any mainnet or Shasta deployment; D21.B2(a) leaves Reloaded-operated deployments to the maintainer. |
| A.3 | Address form in coin configuration: base58check written by operators, converted to the 20-byte payload at activation | COVERED (as a decision) | R21.B2 (both forms accepted), D21.B7 (documented form). |
| A.4 | Swap-contract binding through the EVM family's existing activation fields, no TRON-specific field | COVERED | R21.B2; the documents note the same (the activation surface already accepts the fields). |
| A.5 | Orderbook address derivation for `TRX` and `TRC20` orders (base58check display form) | COVERED | R21.B4, F21.B3. |
| A.6 | Separate transaction representation for signed TRON transactions inside the EVM family | not a behavior | Internal shape (placement at most); chapter 21 §21.6 already binds the protobuf wire form. |
| A.7 | Contract-call building, energy estimation by constant-call dry run, `fee_limit` cap policy | COVERED (as a decision) | D21.B5 (fee handling; the public TRON documentation is the source). |
| A.8 | Discovery of spend/refund through the indexed contract-event service and transaction-info receipts; typed failure when no event source is configured | COVERED (as a decision) | D21.B8; the safety requirement (never report "not spent" on an unavailable source) is stated there. |
| A.9 | Taker-fee send and validation for TRX and TRC20 | COVERED (as a decision) | D21.B9. |
| A.10 | Payment send, validation (reading stored payment state and comparing decoded arguments), spend, refund, secret extraction | COVERED (conditionally) | §21.11B.2 semantics, binding only under D21.B2(a). |
| A.11 | Trade-fee estimation for the legacy preimage RPC and for V2 callers alike | PARTLY | D21.B5 covers the legacy preimage; V2 swaps for TRON remain deferred (§21.11 D2), so the V2 side is out of scope by decision rather than missing. |
| A.12 | Secret-hash algorithm chosen per coin pair for a TRON pairing | COVERED | R21.B6 with [Chapter 51](../51-legacy-v1-swap-state-machine.md) R71/R72 (pair-based). The documents also select on the coin pair; this repository's parked branch rewrite had used a per-coin shape, which `docs/plans/tron-swap-support.md` already rejects. |
| A.13 | Watcher-reward contract variants for TRON | PARTLY (decision pending) | §21.11 D4 and plan question 5; the documents raise it as an open question and settle nothing. |
| A.14 | Confirmation depth and solidified-block finality | COVERED (as a decision) | R21.B5, D21.B8. |
| A.15 | Verification: a Nile proof of the hash lock before any client work, per-step tests, an end-to-end Nile swap before release | NOT COVERED on `dev` | Chapter 21 §21.11B binds no test rows for TRON swaps yet (the parked branch's T1–T6 exist only there). A lab/test row set is needed when swaps are scheduled (work item). |
| A.16 | Coins-file entries for native TRON coins carrying a swap contract | NOT COVERED | Coins-file configuration (coins repository), not CRD. |
| A.17 | TRC20 tokens that return no boolean (mainnet USDT) tolerated by the contract's transfer wrapper and balance checks | COVERED | §21.11B.2 contract notes. |
| A.18 | A pre-existing defect the documents assert in the EVM family's V1 **ERC20 payment validation** | finding, not a gap | See the code-quality finding below. |
| A.19 | A refactor of the chain RPC client layer | not a behavior | Internal decomposition; nothing to cover. |

> **Code-quality finding (informative; trigger: a reference document
> states the problem; passed the KDF Dirty Gate on 2026-11-08).** At
> `v3.0.0-beta` the EVM family's V1 payment validation for an ERC20
> payment compares the encoded payment *amount* against the expected
> trade amount **only when a watcher reward is part of the payment**;
> without a watcher reward the amount argument of the counterparty's
> payment call is not compared (the other arguments — swap id, token,
> receiver, hash lock, lock time — are). The planning documents of row
> A record this as a pre-existing gap to fix alongside TRON
> validation. **Reloaded already validates the ERC20 amount
> unconditionally** in its V1 payment validation (checked in this
> repository's tree on 2026-11-08), so no change is needed on our
> side; recorded here so that a future parity pass does not "fix"
> Reloaded towards the reference. Chapter 17 / 51 owners may wish to
> add a test row asserting the unconditional check.

**Item A recommendation.** Nothing in row A adds behavior beyond what
§21.11B already covers or defers to D21.B1–D21.B9. Add to the TRON swap
work (activation route, swap operations, lab rows): (i) a test-row set
for TRON swaps (A.15) once D21.B2 is decided; (ii) the coins-file
configuration item (A.16); (iii) the regression test of the finding
above (optional). No new chapter section is needed.

---

## B. TRON GasFree fee delegation — behaviors at `feat/tron-gasfree (as of 2026-05-04)` vs this repository's branch ch.49 (the GasFree chapter on branch `feat/tron-gasfree`, not the `dev` chapter 49) and `docs/plans/tron-gasfree.md` (reference-derived; passed the KDF Dirty Gate on 2026-11-08)

**Comparison target.** This repository has **no** GasFree text on `dev`.
The chapter compared against is the 807-line GasFree chapter on this
repository's own branch `feat/tron-gasfree` (numbered 49 there, to be
renumbered on revival), whose last commit message records a Dirty Gate
PASS (0.08); its requirements are cited below as "branch ch.49 §x / Rn /
D-x". The reference branch's own pull-request description is **stale
relative to its tip**: it lists the withdraw-path integration as not yet
done, while the tip carries it. All statements below are about the tip.

### B.1 Public interface the reference adds (wire-visible)

| # | Behavior | Coverage | Where / what is missing |
| --- | --- | --- | --- |
| B.1.1 | Platform activation (the mmrpc 2.0 EVM platform request, one-shot and task form) accepts an optional object `tron_gasless_provider` with `base_url` (host-only; a URL with a path is rejected), `api_key`, `api_secret`, `service_provider` (parsed as a TRON address at activation), `request_timeout_ms` (default 15 000) and `status_poll_interval_ms` (default 3 000). Present on a non-TRON platform → activation rejected with the invalid-payload error. The per-network path segment (`tron`, `nile`, `shasta`) is appended by the client. | PARTLY | Branch ch.49 §49.3.1 binds the object and host-only rule; the two default values are not bound. Code: NOT COVERED on `dev` (the parked branch has the module but no activation wiring). |
| B.1.2 | TRC20 token activation (one-shot and task form) accepts an optional `gasless` object `{enabled, transfer_max_fee}`; `enabled: false` behaves as absent; a negative cap is rejected; the cap is converted to token base units at activation; a token block without a platform provider, or on a non-TRON chain, is rejected with the invalid-payload error. | COVERED | Branch ch.49 §49.3.2, R1. Code: NOT COVERED on `dev`. |
| B.1.3 | **Where the custody address is surfaced.** `gasfree_address` (base58check) is added, omitted when not applicable, to: the legacy `my_balance` response; each address entry of HD balance reports and the single-address balance report (so it appears in the platform-with-tokens activation result for both the platform and the token address-info blocks, in the task-based token activation result and in the one-shot single-token activation result); and the responses of `get_new_address`, `account_balance`, `init_account_balance`, `init_scan_for_new_addresses` and `init_create_account`. It is populated only for a TRON coin whose platform has a provider configured. | PARTLY | Branch ch.49 §49.3.3 names "the balance response" only; the activation-result and HD address-management placements are not bound. The reference's own code notes that the field sits on shared cross-coin structures as an interim choice. |
| B.1.4 | Withdraw request gains `fee_method` (`native` \| `gasless` \| `auto`; absent = native) and `gasless` (`max_fee`, `deadline_seconds`, `fallback_to_native`, default false). `gasless` options with `native`/absent `fee_method` → the invalid-fee withdraw error (400). On a **non-TRON** coin, `fee_method` `gasless` or `auto` → the gasless-unavailable error (503). | COVERED | Branch ch.49 §49.3.4, R15. The non-TRON refusal's status is implicit there; the table of §B.3 makes it explicit. |
| B.1.5 | Withdraw result fee details for the gasless rail: a **new tagged variant** of the shared fee-details object (wire tag value `TronGasless`) with `coin`, `fee_method` (`gasless`), `provider_name` (`gasfree`), `gasfree_address`, `transfer_fee`, `activation_fee`, `total_token_fee`, `signed_max_fee` (optional), `trace_id` (null at the sign-only tip). | COVERED | Branch ch.49 §49.3.5; the tag value is not pinned there (pin it if parity on the wire is wanted). |
| B.1.6 | Withdraw result transaction payload (sign-only): an **unsigned** transaction artifact whose body is a relay object with `relay_type` (fixed value `tron_gasfree`), `chain_id` (decimal string), `coin`, optional `from` selector, `from_address`, `gasfree_address`, `verifying_contract`, `created_at` (RFC 3339, seconds), and `signed_authorization` {`token`, `service_provider`, `user`, `receiver`, `value`, `max_fee`, `deadline`, `version`, `nonce`, `sig`} with integers as decimal strings and the signature as 130 hex characters without `0x`. Unknown members are rejected on re-read. | COVERED | Branch ch.49 §49.8 step 5 (as an informative field list). |
| B.1.7 | Task-withdraw progress values added: `FetchingGaslessQuote`, `SigningGaslessAuthorization`, `SubmittingGaslessAuthorization`, `WaitingForGaslessProvider {trace_id, provider_state}`, `WaitingForGaslessSettlement {trace_id, tx_hash?}`; only the first two are emitted at the tip. | COVERED | Branch ch.49 §49.3.6 (values described, not pinned). |
| B.1.8 | Withdraw error family for the rail, nested under the shared withdraw error (outer `error_type` names the gasless family, inner `error_type` one of `Unavailable` 503, `PendingTransfer` 409, `MaxFeeExceeded` 400, `QuoteExpired` 400, `ProviderRejected` 502, `InvalidProviderResponse` 502, `TraceNotFound` 404). | COVERED (statuses) / PARTLY (shape) | Branch ch.49 §49.9 binds the statuses and declares the discriminants non-binding; the nested-object shape is wire-visible and should be bound if parity is wanted. |

### B.2 Dictated interop the reference implements

| # | Behavior | Coverage | Where / what is missing |
| --- | --- | --- | --- |
| B.2.1 | Custody-address derivation: TRON `CREATE2` with per-network controller, beacon **and proxy creation bytecode embedded for all three networks**; the bytes are taken from the public GasFree JavaScript SDK (a public repository), which is the source any implementation can fetch. Vectors: three Nile, two mainnet, one Shasta user→custody pairs. | COVERED (CRD) / NOT COVERED (code) | Branch ch.49 §49.5, R2, T1 bind exactly this. This repository's parked branch left the bytecode **empty** (the plan's blocker); the reference tip shows the bytes are obtainable from the public SDK, which answers `docs/plans/tron-gasfree.md` open question 1: a clean-channel fetch suffices, no Spec Reader dispatch is needed for the bytes. |
| B.2.2 | Provider REST client: path prefix `api/v1`; endpoints for supported tokens, account info, submit, trace; `Timestamp` and `Authorization: ApiKey {key}:{base64(HMAC-SHA256(method ‖ path ‖ timestamp))}` headers; a 2xx HTTP status with a non-200 envelope `code`, or a success without `data`, is an error; provider text in surfaced errors is whitespace-collapsed and bounded to 256 characters; supported-token list cached for the coin's lifetime; a per-request timeout; a proxy-mediated transport variant exists in shape only and answers not-implemented. | COVERED | Branch ch.49 §49.4.1–§49.4.3, R10, R11, D-proxy (the 256-character bound is not stated; minor). |
| B.2.3 | Provider payload parsing: integers accepted as JSON numbers or decimal strings; negative or empty rejected; `version` must equal 1 everywhere; `requestId` must be a version-4 UUID; the transfer-fee estimate accepted under both of the provider's two spellings; signatures and transaction hashes rejected with a `0x` prefix or a wrong length; unknown lifecycle `state`/`txnState` values rejected. | COVERED | Branch ch.49 §49.4.4–§49.4.7, R5, R6, R12, T5, T6. |
| B.2.4 | TIP-712 `PermitTransfer`: domain `GasFreeController` / `V1.0.0` / chain id (mainnet 728126428, Shasta 2494104990, Nile 3448148188) / controller; message field order `token, serviceProvider, user, receiver, value, maxFee, deadline, version, nonce`; signer must equal `user`; deadline must be in the future; 65-byte signature with `v` normalized to 27/28; raw signature redacted in debug output. Vectors: two domain separators (Nile, mainnet) and six permit hashes. | COVERED | Branch ch.49 §49.6, R3, R4, R6, R7, R9, T2. |
| B.2.5 | Shared EIP-712 encoder changes made for GasFree: fixed-size `bytes32` encoded as the raw 32 bytes (previously hashed — a correctness fix), new dynamic `bytes` type (hashed), strict `0x`-prefixed even-length hex, 20-byte address and ≤32-byte uint checks. | not needed for GasFree / informative | `PermitTransfer` uses neither `bytes32` nor `bytes`. This repository's own encoder already enforces a 32-byte `bytes32` and does not hash it; it has no dynamic `bytes` type (relevant only to WalletConnect/MetaMask typed-data requests that carry one — [Chapter 22](../22-walletconnect-v2.md), [Chapter 47](../47-metamask-integration.md) owners' call). |
| B.2.6 | Networking: a cross-platform "POST JSON with caller headers" transport where `Content-Type: application/json` cannot be overridden by the caller; the WASM transport now returns the browser-visible response headers instead of an empty map and lowercases header names. | COVERED (pointer) | Branch ch.49 §49.2 requires the capability and points to [Chapter 26](../26-cross-platform-and-wasm.md); chapter 26's transport table does not yet list it (add when the rail is scheduled). |

### B.3 Withdraw behavior at the tip

| # | Behavior | Coverage | Where / what is missing |
| --- | --- | --- | --- |
| B.3.1 | **Eligibility.** The rail applies only to a TRC20 token on a TRON platform with a provider and a token gasless block; `fee_method: gasless` on native TRX → unavailable (503). The provider binding is read from the token's platform coin. | COVERED | Branch ch.49 §49.8 step 2, R1. |
| B.3.2 | **`gasless` mode.** Gasless is attempted first; with `fallback_to_native: true` a *deterministic* unavailability (rail unavailable, token not enrolled, insufficient custody balance) falls back to the native rail; provider/transport failures still surface. | COVERED | Branch ch.49 §49.8 steps 1 and 6, T8. |
| B.3.3 | **`auto` mode.** The **native** route is tried **first**; only when it fails for a balance-type reason (insufficient token balance, insufficient platform balance for the fee, zero balance on a max request, amount below minimum) and the request is not a max request is the gasless rail tried; a gasless failure then re-raises the native route's error. | PARTLY — **direction inverted** | Branch ch.49 §49.3.4 and §49.8 step 1 describe `auto` as *gasless first, native as fallback*. The reference tip does the opposite. One of the two must be chosen (plan question 4); the maintainer's existing preference on the EVM fee path (fail rather than substitute silently) argues for binding whichever is chosen explicitly. |
| B.3.4 | **Quote.** Max-amount gasless withdraw rejected; zero amount rejected; default authorization window 300 s; `deadline_seconds: 0` rejected; effective cap = min(per-request `max_fee`, activation-time `transfer_max_fee`), or the quoted total when neither is set; quoted total > cap → `MaxFeeExceeded`; signed `maxFee` = effective cap; nonce taken from the provider's account info; the whole quote-and-sign runs under the sender address's nonce lock. | PARTLY | Branch ch.49 §49.8 step 3, R8, R14 cover the cap and max rules; the 300 s default and the zero-amount rule are not bound. |
| B.3.5 | **Preflight decision order.** Address mismatch → token not enrolled → token-decimal mismatch → pending transfer (the provider's submit-allowed flag false; evaluated *before* any on-chain read) → on-chain custody balance read → inactive-account total check (value + fees + frozen) → spendable check (on-chain − frozen ≥ value + fees). Activation fee counts only when the custody account is not yet active. | COVERED | Branch ch.49 §49.7, T7 (the evaluation order is not stated; informative). |
| B.3.6 | **Preflight outcome → withdraw error.** Token not enrolled → `Unavailable` (503); address mismatch and decimal mismatch → `InvalidProviderResponse` (502); pending → `PendingTransfer` (409); both insufficient-balance outcomes → the **shared** insufficient-balance withdraw error carrying `available` (custody spendable, or custody on-chain balance for an inactive account) and `required` (value + fees, plus frozen for an inactive account). | PARTLY | Branch ch.49 §49.7 names the decision categories but not the error each maps to; §49.9 lists the statuses. The single outside comment on the reference pull request asked for a *distinct* message for the custody-address shortfall instead of the generic one; Reloaded may choose either. |
| B.3.7 | **Provider-interaction failures → withdraw error.** Invalid provider response → `InvalidProviderResponse` (502); provider 4xx other than auth/forbidden/rate-limit → `ProviderRejected` (502); authentication, forbidden, rate-limit, upstream 5xx, transport and timeout → the shared withdraw **transport** error; invalid request, internal and not-implemented → the shared withdraw **internal** error. The finer provider-level categories (401/403/429/504) are **not** visible at the withdraw RPC. | PARTLY — inaccurate as written | Branch ch.49 §49.9 presents the provider-interaction status mapping as a public contract; at the reference tip it is internal and collapses as above. Rewrite §49.9's second family as internal categories, or bind the collapse. |
| B.3.8 | **Signing-key policy.** HD wallets sign with the key at the request's `from` path; the single-key policy signs with the activated key; hardware-wallet and WalletConnect policies have no local key and the permit signing fails with the shared **internal** error after the quote was fetched. | NOT COVERED | Branch ch.49 has no rule. Recommend a typed early refusal for key-less policies before any provider call (same spirit as [Chapter 47](../47-metamask-integration.md) R47.5.13a). |
| B.3.9 | **Sign-only.** The signed authorization is returned, not submitted; no provider submit, no trace polling, no on-chain settlement tracking; `trace_id` is null. | COVERED | Branch ch.49 §49.8 step 5, D-submit. |

### B.4 Not in the reference tip (its own to-do list, behavior categories)

| # | Missing at the tip | Coverage in branch ch.49 |
| --- | --- | --- |
| B.4.1 | Submitting the signed authorization to the provider (planned through the raw-transaction-send RPC), trace polling and settlement tracking with the reserved progress values | D-submit (COVERED as deferred) |
| B.4.2 | Recovery/sweep of a stranded balance at the custody address back to the user's account | NOT COVERED — no deferred item; recommend adding one (the custody address is a user-funded account that the wallet otherwise never spends from) |
| B.4.3 | The proxy-mediated authentication transport | D-proxy (COVERED as deferred) |
| B.4.4 | Fetching the provider list from the provider's configuration endpoint instead of the operator-supplied `service_provider` | D-provider-fetch (COVERED as deferred) |
| B.4.5 | Any end-to-end test on the Nile testnet or a WASM smoke test | NOT COVERED as a test row (branch ch.49 §49.11 has unit-level rows only); the reference tip has unit tests only (derivation vectors, HMAC vector, serde contracts, preflight decisions, encoder, URL resolution, header builders) |

**Item B recommendation.** The GasFree chapter and code pull requests
should (1) treat plan question 1 as answered — fetch the creation
bytecode and vectors from the public SDK, verify the derivation (B.2.1);
(2) before any revival, re-gate and amend branch ch.49 for B.3.3 (`auto`
direction), B.3.7 (error collapse), B.1.3 (where `gasfree_address`
appears), B.3.6 (preflight-to-error mapping), B.3.8 (key-less refusal),
B.4.2 (sweep), and the unbound defaults (B.1.1, B.3.4); (3) keep the
parity-or-not decision on the wire discriminants of B.1.5, B.1.7 and
B.1.8 explicit. A new chapter section is **not** needed beyond those
amendments; the branch chapter already has the right shape.

---

## C. Tendermint activation features — no unmerged reference pull request; coverage of the topics, plus the one unmerged Tendermint draft

### C.1 The maintainer's feature list vs chapter 36 and `docs/plans/tendermint-activation-features.md`

All of these are `v3.0.0-beta` behaviors already bound by chapter 36
(Dirty Gate PASS); the "code" column reflects chapter 36 §36.7's status
list (items 5, 6, 8, 9 deferred to the plan).

| # | Topic | CRD | Code | Note |
| --- | --- | --- | --- | --- |
| C.1.1 | `activation_params` public-key form (watch-only / Ledger-flagged activation from a caller-supplied key) | COVERED — ch.36 R36.1.3 (1), R36.1.4 step 3 | NOT COVERED | Interim: a request carrying `activation_params` is **rejected** with the platform-creation error — DIVERGED BY DECISION (maintainer, 2026-10-11, ch.36 §36.7). |
| C.1.2 | `activation_params` WalletConnect form, and the public-docs variant of it | COVERED — ch.36 R36.1.3 (2), (3) | NOT COVERED | Accepting both forms is DIVERGED BY DECISION (maintainer; the reference rejects the docs form). Malformed key → `InvalidRequest` instead of the reference's abort (ch.36 code-quality finding). |
| C.1.3 | HD-wallet mode honoring `path_to_address` | COVERED — ch.36 R36.1.2, R36.1.4 step 3 | NOT COVERED | ch.36 code-quality finding on the reference pairing the derived private key with the root public key; plan item 2 adopts the fix. |
| C.1.4 | Custom tokens (`protocol` member of `tokens_params[]` and of `enable_tendermint_token`), `CustomTokenError` on a duplicate ticker | COVERED — ch.36 R36.1.2, R36.1.4 step 4, R36.1.6, R36.2.1, R36.2.4 | NOT COVERED | Wallet-only for custom tokens is DIVERGED BY DECISION (ch.35 R35.2.7 analog, plan item 3). |
| C.1.5 | `tx_history: true` starts background history fetching | COVERED — ch.36 R36.1.2, R36.1.4 step 6 | NOT COVERED | Plan item 4. |
| C.1.6 | Hardware `priv_key_policy` (`Trezor`) is accepted and ignored by the reference | COVERED (parity) — ch.36 R36.1.2 + code-quality finding; D36.1 | matches reference | Plan item 5: whether to refuse with the device-policy error is an open maintainer decision. |
| C.1.7 | Task-managed single-token activation family | DEFERRED — ch.36 D36.2 | NOT COVERED | Plan item 6. |
| C.1.8 | Key-less activation policy reaching the **legacy (V1) HTLC** path (silent substitution of the node key as HTLC public key) | NOT COVERED in any chapter | latent (no key-less policy constructible today) | Recorded in the plan ("Latent hazard") with a remedy; chapter 18 §18.11 (batch 0.4.0-7 draft) does not reach it, and ch.22 RP11.1 / ch.47 R47.5.13a cover other paths only. Needs a rule in ch.18 or ch.51 when C.1.1/C.1.2 are implemented. |

### C.2 The one unmerged Tendermint draft: priority transaction fees (`tendermint-priority-fee (as of 2025-07-24)`) (reference-derived; passed the KDF Dirty Gate on 2026-11-08)

Behaviors the draft adds (three commits over its base; about 260 lines
changed; unit tests only; review state: two inline review comments
discussing whether the new withdraw-fee shape should extend the
existing Cosmos fee shape instead, no approval, conflicts with `dev`):

| # | Behavior | Coverage | Note |
| --- | --- | --- | --- |
| C.2.1 | `TENDERMINT` `protocol_data` gains optional `priority_gas_prices`: an array of exactly three gas prices (low, average, high) for the platform coin, intended to be taken from the public Cosmos chain registry | NOT COVERED | ch.36 §36.3.1 lists `gas_price` only; this repository's Tendermint protocol parser has no such member. |
| C.2.2 | Withdraw `fee` gains a Cosmos **priority** form `{type, gas_limit, gas_price_option}` with option values `Low` \| `Average` \| `High` \| `Default`, alongside the existing explicit gas-price form; the chosen level selects the matching configured price, falling back to the coin's default gas price when the array is absent or the level is `Default` | NOT COVERED | [Chapter 49](../49-withdrawal-task-path.md) (withdrawal task path) and ch.18 bind the existing Cosmos fee form only. |
| C.2.3 | The swap-transaction fee-policy RPCs (`get_swap_transaction_fee_policy` / `set_swap_transaction_fee_policy`) accept Tendermint platform coins: `Low`/`Medium`/`High` map to the three configured prices, `Internal`/`Unsupported` to the default price; the default policy for a Tendermint coin is `Internal`; the error text for unsupported coins is generalized from "EIP-1559" wording | NOT COVERED | This repository has no swap-fee-policy operations for Tendermint ([Chapter 35](../35-evm-v2-activation-rpcs.md) covers the EVM side only). |
| C.2.4 | Swap transactions (create HTLC, claim, dex-fee send, trade-fee simulations) use the selected priority gas price in the simulation-based fee formula (`ceil(gas_used × 1.5 × gas_price)`), including the key-less (external signing) branch | NOT COVERED | ch.18 §18.11 (batch 0.4.0-7 draft) R18.11.6–R18.11.10 bind the default-price formula; a priority price would be a parameter of it. |
| C.2.5 | Delegation, undelegation and reward-claim transactions and IBC transfers accept the priority fee form | NOT COVERED | ch.18 §18.6A binds the staking RPCs with the existing fee form only. |

**Item C recommendation.** For the maintainer's list (C.1.1–C.1.7):
nothing to add to the CRD; the implementation gap is already the plan's
content — fold it into the Tendermint activation-features work or keep
the plan. C.1.8 needs a **new rule** (ch.18 §18.11 or ch.51) binding an
early typed refusal for a key-less policy on the legacy HTLC path,
platform coin and tokens alike; it should land with, or before,
C.1.1/C.1.2. For the priority-fee draft (C.2): the maintainer decides
whether it is wanted at all; if yes, it is a small addition to ch.36
§36.3.1 (one optional `protocol_data` member), the `dev` chapter 49
(withdrawal task path; one fee form) and ch.18 §18.11 (a parameter of
the fee formula), plus the fee-policy RPC extension — no new chapter.

---

## D. Provenance footer (note-scoped)

- *Inputs:* the reference states named in the preamble, read by the
  dirty-side specification author (chapter 01 R37) on 2026-11-08: the
  file lists, diffs and unit-test vectors of the two branch-tip rows
  against their bases; the pull-request index of the historical record
  (titles, head names, dates, state, review counts; the two pull-request
  descriptions and their comments were read for behavior facts and
  review state only, and nothing from them is quoted); one
  payment-validation path of `v3.0.0-beta` for §A.18; the planning
  documents of row A for their subject areas only. From this repository:
  chapter 21 §21.11B (Spec Reader round 0.4.0-6, gated, not yet on
  `dev`), chapter 36, chapter 18 §18.11 (round 0.4.0-7 draft), chapter
  49 (both the `dev` chapter and the GasFree chapter on branch
  `feat/tron-gasfree`), chapters 22, 26, 35, 47, 51, 55;
  `docs/plans/tron-swap-support.md`, `tron-gasfree.md`,
  `tendermint-activation-features.md`, `tron-support.md`; and this
  repository's tree (EIP-712 encoder, withdraw request type, task-status
  enumeration, V1 payment validation, Tendermint protocol parser) for
  the "our side" columns.
- *Permitted-input classes used:* public facts about a publicly
  reachable repository (chapter 01 R6: names, identifiers, dates, open/
  merged/draft state, comment counts); the public GasFree protocol,
  GasFree SDK, TIP-712, EIP-712, the TRON `CREATE2` rule and the public
  TRON developer documentation (R3 and R6) wherever a GasFree or TRON
  behavior in §B or §A is a public-protocol fact; this repository's own
  chapters and tree.
- *Sibling-allowlist consultations:* none beyond the chapter-21
  citations already recorded in §21.11B.6.
- *Forbidden corpus:* **consulted**, for the topic list of §A (subject
  areas only), the behavior enumerations of §A.18, §B and §C.2 and the
  identification facts of §0. Only functional behavior, public RPC field
  names and wire-visible values, dictated interop and review-state facts
  cross into this note; no private identifier, helper decomposition,
  control-flow transcription, error or log string literal, test body, or
  quoted pull-request or review text does. Wire-visible discriminants
  (fee-method values, task-status values, error discriminants, the
  fee-details tag, the relay type value) are recorded as public
  interface of the reference's RPC surface; whether Reloaded adopts them
  is a decision, not a requirement. *Clearance record (chapter 01
  R38–R40):* Cleared by the KDF Dirty Gate on 2026-11-08: PASS,
  discretionary-similarity 0.05. Extent: the complete change sets of the
  rows feat/tron-gasfree (as of 2026-05-04) over dev (as of 2026-03-16)
  (49 files) and tendermint-priority-fee (as of 2025-07-24) over dev (as
  of 2025-07-18) (9 files), the single-commit documentation change set
  of feat/tron-swap-support (as of 2026-04-23) (9 files), one
  payment-validation path of v3.0.0-beta, and the public descriptions
  and comment threads of the two draft pull requests whose heads are the
  two branch-tip rows; no later state of any of the three branches
  exists. Method: reading of the candidate against the compared
  material, mechanical identifier/date/token scans of the candidate with
  per-token forced-vs-free classification, and git/API metadata
  verification of the four §2.15A rows. Measure: 0.05 on the
  discretionary-expression basis, against the standard that any verbatim
  or near-verbatim discretionary expression fails.
