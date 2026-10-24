# Plan: wire parity of every RPC with the public KDF API reference

**Status:** parked (coin activation shipped in PR #17; the rest not started) — **Last reviewed:** 2026-10-18

> **Context:** written 2026-10-08 and updated 2026-10-11
> on `fix/activation-api-parity`, after coin activation was brought to parity on
> that branch. This plan extends the same work to every other RPC and makes the
> rule permanent in the CRD. Nothing here is executed yet.

## Why

The Komodo Wallet Desktop crashed while activating coins against Reloaded. The
crash was a wallet bug, but its trigger was Reloaded answering differently from
the public API reference for the same failure: two unreachable EVM nodes
produced `PlatformCoinCreationError` with an object `error_data` and HTTP 500,
where the reference documents `Transport` with a string `error_data`. Clients
parse `error_type` and `error_data`; a different discriminant or a different
JSON type is a breaking change for them even when the human-readable message
says the same thing.

The CRD did not catch this. Chapter 35 listed the right discriminants and their
HTTP statuses, but did not bind which failure maps to which discriminant, nor
the shape of `error_data`. Most other chapters bind even less. Activation is
fixed on `fix/activation-api-parity` (CRD ch.35, 36, 39, 40, 41, 46, 48 and the
new ch.55). This plan covers the rest of the surface.

## What "wire parity" means here

For every RPC method, Reloaded is compared with the reference on seven
dimensions. These are the same dimensions used for activation:

| # | Dimension | What is compared |
| --- | --- | --- |
| W1 | Request | field names, types, required vs optional, defaults, accepted alternative forms, rejection of malformed input |
| W2 | Success response | field names, types, presence rules (e.g. fields that depend on a request flag), map vs array, envelope (`result`, `id`) |
| W3 | Error discriminants | the set of `error_type` values a caller can observe |
| W4 | Failure mapping | which failure produces which `error_type` (e.g. unreachable node, unknown coin, already active) |
| W5 | `error_data` shape | absent / string / integer / object (field names and types), per `error_type` |
| W6 | HTTP status | per success and per `error_type`, and for task-status responses |
| W7 | Numeric encoding | decimal strings, `*_rat` / `*_fraction` companions, integers, per the rational number note |

The human-readable `error` text, `error_path` and `error_trace` are not part of
the contract.

## Sources and how they are used (clean-room)

| Source | Role | Who may read it |
| --- | --- | --- |
| Public KDF API reference (komodoplatform.com/en/docs/komodo-defi-framework/api/...: legacy, v20, v20-dev, common_structures incl. error_types, rational_number_note) | Governs wherever it is explicit (field table and example agree, or an uncontradicted example). Permitted input under CRD ch.01 R4/R6. | anyone |
| GLEEC KDF behavior | Decides wherever the reference is silent, ambiguous or self-contradictory, including every HTTP status code (the reference states none). Maintainer decision of 2026-10-08. Reference version: the latest GLEEC release tag, named explicitly in each chapter (`v3.0.0-beta` for activation). | KDF Spec Reader and KDF Dirty Gate only |
| Maintainer decision | Where the reference is explicit but GLEEC behaves differently, the activation pass took each case to the maintainer. The resulting pattern: response values follow the docs; a request form only one side accepts is accepted in both forms when that cannot break either kind of client; anything else is decided case by case. New cases of this kind still go to the maintainer. | -- |
| GLEEC release binaries run as black boxes | Cross-check of what a real node returns, without reading source (R6 public observation). | anyone |
| Reloaded code | What we do today. | anyone |

Every chapter change goes through the existing workflow: the Spec Reader writes,
the Dirty Gate passes the chapter, and the Coder implements from the gated
chapter only.

---

## Part 1 — Re-check every RPC method

### 1.1 Inventory

Build one inventory from three lists and reconcile them:

1. `docs/reloaded-rewrite/rpc-method-census.md` (232 method strings, all
   surfaces);
2. Reloaded's dispatchers: the v2 dispatcher, its `task::`, `stream::`,
   `gui_storage::`, `lightning::` and `experimental::` routers, and the legacy
   dispatcher;
3. the public reference's method index (sitemap and per-section index pages).

Each method gets one of: *documented and routed*, *documented but not routed*
(a gap), *routed but not documented* (W1–W7 then come from GLEEC alone), or
*neither* (dropped from the inventory).

### 1.2 The parity register

Record the result in a new file,
`docs/reloaded-rewrite/rpc-wire-parity-register.md`, one row per method. The
census gives the method names; the register adds the parity status:

| Column | Content |
| --- | --- |
| method | wire method string |
| surface | legacy / v2 / task / stream / ... |
| governing chapter | CRD chapter and R-IDs that bind W1–W7 |
| reference page | URL of the public reference page, or "undocumented" |
| W1 … W7 | `=` match, `D` deviation, `S` docs silent → GLEEC, `C` docs contradict themselves → GLEEC, `X` deliberate Reloaded divergence |
| status | open / fixed in `<commit>` / divergence recorded in `<doc>` |

The register is the deliverable of Part 1 and the list the maintainer reviews.

### 1.3 How each method is checked

For each batch (§1.5):

1. **Docs extraction.** A subagent with web access but no repository access
   extracts the reference pages verbatim: tables, every example JSON, and a
   list of contradictions within the docs. This is the same method used for
   activation; the extracts stay in the session scratchpad, not in the repo.
2. **Reloaded survey.** A clean subagent maps each method's request type,
   success type, error enum (each variant's serde tag, `error_data` shape and
   HTTP status) and the `From`/`map_err` sites that decide W4. Output is
   file:line evidence.
3. **Comparison.** The orchestrator fills the register rows. Every `S` and `C`
   cell becomes a Spec Reader question; the docs alone decide every `D` cell.
4. **Black-box cross-check (optional, recommended for W4–W6).** Run the same
   failing requests against a GLEEC release binary and a Reloaded build in the
   same regtest or offline setup (unknown coin, already active coin, invalid
   params, unreachable node URL) and diff `error_type`, the JSON type of
   `error_data` and the HTTP status. This is public observation, needs no
   corpus access, and catches mapping errors the static survey misses. The
   harness must not import anything from `~/kdf-swaplab` (Coder-forbidden), so it
   lives under `tools/` as its own small script set.
5. **Spec Reader pass** per governing chapter for every `S`/`C` cell and for
   per-method error tables, then the Dirty Gate.
6. **Coder pass** per gated chapter: code changes plus wire-shape unit tests
   (§1.6).

### 1.4 Deviations already known or suspected (to be confirmed in Part 1)

These came up incidentally during the activation work. They are candidates,
not confirmed deviations, until the batch that owns them checks them.

| Candidate | Where | Dimension |
| --- | --- | --- |
| `task::withdraw::status` reports a failed task as the bare inner error rather than the full error object the other task families put in `details` | withdraw task (ch.49) | W2/W5 |
| Legacy (v1) error bodies are `{"error": "<file:line chain> <message>"}` and are always HTTP 500, for user errors too | legacy dispatcher (ch.43) | W5/W6 |
| `&`, `<`, `>` in responses are HTML-escaped before sending, including inside error strings (e.g. a node URL with a query string) | RPC response writer (ch.43) | W2/W5 |
| The `id` echo: the reference shows `"id": null` when the request had no `id`, and documents echoing a non-null `id` | envelope (ch.43) | W2 |
| `UnexpectedDerivationMethod` carries a display string in one enum and a bare variant name in another | coin errors used outside activation (my_balance, withdraw, addresses) | W5 |
| Methods documented with numeric `*_rat` / `*_fraction` companions (orders, trade preimage, max volumes, order status) | legacy and v2 trading methods | W7 |

### 1.5 Batches and order

Ordered by client impact. The wallet hot path comes first because a wrong
`error_type` or `error_data` type there breaks wallets at runtime, as the
activation crash showed. W5 (shape) and W3/W4 (discriminants and mapping) are
fixed before W6 (HTTP status), which in turn comes before request leniency
(W1) and cosmetic response details.

| Order | Batch | Methods (examples) | Governing chapters |
| --- | --- | --- | --- |
| 1 | Wallet hot path | `my_balance`, `withdraw` + `task::withdraw::*`, `send_raw_transaction`, `my_tx_history` (v1 and v2), `get_enabled_coins`, `disable_coin`, `get_public_key`, `get_public_key_hash`, `validateaddress`, `convertaddress` | 43, 49, 38, 39, 20, 53 |
| 2 | HD wallet and keys | `get_new_address`, `account_balance` + task variants, `scan_for_new_addresses`, `get_wallet_names`, `get_mnemonic`, `change_mnemonic_password`, `show_priv_key`, key export | 05, 07 |
| 3 | Trading (legacy and v2) | `setprice`, `buy`, `sell`, `cancel_order`, `cancel_all_orders`, `my_orders`, `order_status`, `update_maker_order`, `orderbook`, `orderbook_depth`, `best_orders`, `trade_preimage`, `max_taker_vol`, `max_maker_vol`, `min_trading_vol` | 08, 11, 12, 32 |
| 4 | Swaps | `my_swap_status`, `my_recent_swaps`, `active_swaps`, `recover_funds_of_swap`, `import_swaps`, `stop`/`kickstart` behavior, v2 swap status methods | 13, 51, 52 |
| 5 | Streaming and events | `stream::*::enable`, `stream::disable` | 10 |
| 6 | Lightning (non-activation) | `lightning::*` | 41 |
| 7 | NFT | `get_nft_list`, `get_nft_metadata`, `refresh_nft_metadata`, `withdraw_nft`, `clear_nft_db`, ... | 19 |
| 8 | Staking and experimental | `experimental::staking::*`, liquidity routing, 1inch | 23 |
| 9 | GUI storage, MetaMask, WalletConnect, Trezor (non-activation) | `gui_storage::*`, `task::connect_metamask::*`, `wc_*`, Trezor helpers | 24, 47, 22, 50 |
| 10 | Daemon and misc | `version`, `stop`, `metrics`, `get_my_peer_id`, `ban_pubkey`, `list_banned_pubkeys`, `get_directly_connected_peers`, `sign_message`, `verify_message`, ... | 43, 28 |

Each batch is one branch, one PR, and is merged before the next starts. A batch
that turns up a deviation in a shared component (the envelope, the legacy error
body, the response writer) fixes it once in ch.43, and later batches inherit
the fix.

### 1.6 What each fix must ship with

- The governing chapter binds, per method, a table with columns
  `error_type | failure class | error_data shape | HTTP status | placement`,
  plus the request field table and the success shape. This is the format the
  activation chapters now use.
- For every changed mapping, a unit test that serializes the error and asserts
  `error_type`, the JSON type of `error_data` (and its fields, for objects) and
  the HTTP status. A test that only checks the Rust variant is not enough: the
  activation crash was in the serialized shape.
- A register row moved to `fixed in <commit>`.
- `CHANGELOG.md`: a line per batch listing wire-visible changes, so integrators
  see them.

### 1.7 Effort and risks

- **Effort.** Activation (about 60 methods counting task families) took one
  docs pass, one code survey, four parallel Spec Reader dispatches and their
  gates, and the Coder work. The remaining ~170 methods are simpler on average
  (fewer task families, fewer coin-specific failure paths). Estimate: 8–10
  batches of comparable size to one activation chapter each.
- **Changing a wire shape breaks someone either way.** A client written against
  Reloaded's current output may depend on the deviation. Each batch therefore
  lists its wire-visible changes in the PR description and in `CHANGELOG.md`.
  When an existing integration is known to depend on the old shape, the change
  follows `docs/COMPAT_SWITCHES.md` (see Part 2).
- **The reference contains errors.** The activation pass alone found more than
  80 contradictions or omissions in the docs (tables contradicting their own
  examples, copy-pasted structure tables, invalid JSON). Where the docs are
  wrong and GLEEC is right, Reloaded follows GLEEC and the chapter records a
  *public-docs erratum* note (§2.3). Reporting errata upstream to the docs
  project is an outward-facing action and is left to the maintainer.

---

## Part 2 — A normative wire-compatibility rule in the CRD

### 2.1 The rule

Proposed text, to be placed as new requirements in **ch.43 (Daemon, RPC
Dispatcher & Misc Maintenance)**. Ch.43 already owns the request envelope and
the dispatcher, so it is the natural home for a rule that applies to every
routed method. It goes in a new section, **§43.8 Public API reference wire
compatibility**, with these requirement IDs:

- **R43.8.1 (Wire compatibility).** Every RPC method the daemon routes MUST be
  wire-compatible with the public Komodo DeFi Framework API reference on W1–W7
  (request, success response, error discriminants, failure mapping,
  `error_data` shape, HTTP status, numeric encoding). The human-readable
  `error` text, `error_path` and `error_trace` are not part of the contract.
- **R43.8.2 (Explicit docs govern).** Where the reference is explicit (a field
  table and its example agree, or an example stands uncontradicted), the
  reference governs.
- **R43.8.3 (Silent or contradictory docs).** Where the reference is silent,
  ambiguous or contradicts itself, the behavior of the GLEEC KDF reference
  version named in the governing chapter governs. It is established through the
  Spec Reader and Dirty Gate workflow, never by reading the corpus directly.
  Where v2.6.0-beta differs for netid 8762, the chapter records both and
  selects by the existing compatibility boundary (AGENTS.md §3).
- **R43.8.4 (Per-method binding).** The chapter that governs a method MUST bind
  W1–W7 for it, including a per-method error table
  (`error_type | failure class | error_data shape | HTTP status | placement`).
  A chapter that lists discriminants without the mapping and the shape does
  not satisfy this rule.
- **R43.8.5 (Deliberate divergence).** A deliberate difference from R43.8.2 or
  R43.8.3 is allowed only when recorded as described in §2.2.
- **R43.8.6 (Tests).** Every `error_type` a method can return MUST have a unit
  test asserting its serialized `error_type`, the JSON type of its
  `error_data` (and fields, for objects) and its HTTP status.

Two smaller companion changes:

- **Ch.00, new R7** (document-set discipline): a substantive chapter that binds
  an RPC method MUST satisfy R43.8.4 and cite the reference page by URL in its
  External References.
- **Ch.01 R3** gains a row naming the public API reference of the
  inter-operated framework as a permitted external specification. This is a
  clarification: it is already covered by R4 and R6, and ch.35, 36, 46 and 48
  already cite it. Ch.01 R25 requires the commit to announce the change and
  the reason; R26 is satisfied because the change only makes an existing
  permission explicit.

### 2.2 How a deliberate divergence is recorded

Each document has one job. A divergence is recorded in every place whose job it
touches:

| Document | Records | When |
| --- | --- | --- |
| Governing CRD chapter | a `Deliberate divergence (binding)` note next to the R-ID: what differs, on which of W1–W7, why, and the opt-back mechanism | always |
| `docs/COMPAT_SWITCHES.md` rule + `docs/GLEEC_COMPATIBILITY.md` row | the opt-back setting and its GLEEC-compatible value | when the divergence can break an existing integration (which is almost always the case for W1–W7) |
| `RELOADED_VS_GLEEC.md` | the public catalogue entry describing the difference for users | always, for a shipped divergence |
| `docs/reloaded-rewrite/rpc-wire-parity-register.md` | the `X` cell and a pointer to the chapter note | always |

`docs/reloaded-rewrite/UPSTREAM-PARITY-GAPS.md` is not for divergences. It
tracks unintended gaps: a method that is documented but not routed, or a
confirmed deviation not yet fixed. A gap leaves the tracker when it is fixed. A
divergence never enters it.

Every case where the public docs and GLEEC disagree is also listed in plain
language in `docs/API_REFERENCE_DEVIATIONS.md` (one table per method family:
what the docs say, what GLEEC does, what Reloaded does, why, the CRD rule), and
described in a short code comment at the site that implements it. The
activation pass created that page; each batch extends it.

Two related, non-divergence notes are also recorded in the governing chapter:

- **Public-docs erratum (informative):** the docs are wrong and Reloaded follows
  GLEEC. No switch is needed, because Reloaded matches GLEEC.
- **Undocumented method (informative):** the method is not in the reference and
  W1–W7 come from GLEEC alone.

### 2.3 How the rule is enforced in review

1. **PR checklist.** `docs/PR_REVIEW_CHECKLIST.md` gains an item: "Any change
   to an RPC request, response or error enum: the governing chapter's
   per-method table is updated (R43.8.4), wire-shape tests cover each changed
   `error_type` (R43.8.6), and any divergence is recorded per CRD ch.43 §43.8
   / this plan §2.2."
2. **Definition of done.** `AGENTS.md` §8 gains the same condition, so it binds
   AI contributors as a strict rule (consistent with how AGENTS.md treats
   compatibility switches).
3. **Spec Reader role.** `.github/agents/kdf-spec-reader.agent.md` gains a
   requirement to produce the R43.8.4 table for every RPC method in a chapter
   it authors, and to mark each cell as docs-explicit or GLEEC-decided.
4. **CI guard (mechanical part).** A test next to
   `mm2src/mm2_main/src/rpc/dispatcher/dispatcher_resolution_guard.rs` checks
   that every routed method string has a row in the parity register, so a new
   route cannot land without a parity decision. It checks presence, not
   correctness. Correctness is the reviewer's job, helped by the wire-shape
   tests of R43.8.6.
5. **Census and register stay in sync.** The census remains the method-name
   reference; the register adds parity status. A PR that adds a route updates
   both, and the guard catches a miss.

---

## Order of execution

1. Land `fix/activation-api-parity` (activation parity, ch.55, and the
   activation rows that seed the register). Activation work that the branch
   deliberately deferred is tracked in its own plans and is not part of this
   plan's batches: `docs/plans/solana-support.md`,
   `docs/plans/tendermint-activation-features.md`,
   `docs/plans/lightning-activation-features.md`, legacy EVM `enable` under an
   HD wallet (`UPSTREAM-PARITY-GAPS.md`), custom tokens not being wallet-only
   (kdf-reloaded/kdf#16), the held Z-coin Light-mode Electrum policy (ch.39),
   the decided but not yet implemented per-ticker serialization of
   concurrent activations (ch.55 R55.1.12), and the decided but not yet
   implemented GLEEC behavior for a Z-coin sync date in the future (ch.39
   §39.10, Reloaded status item 2; lands with §39.11).
2. Part 2 first, rules only: §43.8, ch.00 R7, the ch.01 R3 row, the checklist,
   the AGENTS.md item, the Spec Reader role change. Commit them with a ch.01 R25
   announcement. The CI guard lands in the same PR, with the register
   initialized: activation rows filled, every other method marked `open`.
   Seeding every row as `open` lets the guard pass from day one.
3. Part 1 batches 1–10 in the order of §1.5, one PR each. Each PR moves its
   register rows out of `open`.
4. When no `open` rows remain, tighten the CI guard to also reject `open`
   rows for methods whose batch is closed.

## Out of scope

- Changing the public reference itself. Errata are recorded in our chapters;
  reporting them upstream is the maintainer's call.
- P2P and swap-negotiation wire formats. They are covered by their own
  chapters and the compatibility matrix in AGENTS.md §3; this plan is about the
  JSON-RPC surface.
- New features beyond what the reference documents. A documented method that
  Reloaded does not route is a gap, and its batch decides whether it is
  implemented or recorded as a deliberate omission.
