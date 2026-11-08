# Chapter 43 -- Daemon, RPC Dispatcher & Misc Maintenance

**Status:** driving-spec (as-built; documents shipped reloaded behaviour).

> **One-sentence claim:** the project shall expose a single local JSON-RPC
> endpoint that routes both legacy flat-method requests and versioned
> ("mmrpc") namespaced requests, authenticates every non-public method with a
> strength-checked RPC password, restricts non-public methods to loopback when
> configured local-only, never panics the daemon in response to a request, and
> shuts the daemon down gracefully on an OS termination signal.

> **Treatment:** **T-DOC.** Reloaded ships the dispatcher (legacy + namespaced
> routing, public-method classification, password auth) and the daemon
> lifecycle. This chapter documents that shipped behaviour and binds its security
> obligations. It deliberately does **not** re-specify the individual feature RPCs
> (those live in their own chapters); it specifies only the dispatch envelope,
> classification, auth, and lifecycle.

## 43.0 Executive Summary

All client interaction goes through one local HTTP JSON-RPC endpoint. Two request
shapes are accepted: a **legacy** flat shape (a `method` plus inline params) and
a **versioned** shape carrying an `mmrpc` version field, a `method`, a `params`
object, and an optional `id`. The dispatcher classifies each method as *public*
(no auth) or *protected* (requires the RPC password), routes legacy names through
the legacy table and namespaced names (`task::`, `stream::`, `gui_storage::`,
`lightning::`-style, `experimental::`) through the versioned table, and returns a
structured error -- never a panic -- for bad input.

> **Binding scope (R36).** Requirements bind observable behaviour, the request/
> response envelope field names, the method-classification contract, and the
> security obligations. The exact set of feature methods is specified in their
> own chapters and is *informative* here. Private types, helper decomposition,
> and diagnostic wording are informative.

## 43.1 Request envelope & routing

R43.1.1 The endpoint shall accept a **legacy** request shape (a `method` field
plus inline parameters) and dispatch it through the legacy method table.

R43.1.2 The endpoint shall accept a **versioned** request shape carrying:
- `mmrpc` -- the RPC version;
- `method` -- the method string (which may be namespaced, e.g.
  `task::<feature>::init`, `stream::<feature>::enable`, `gui_storage::<op>`,
  `experimental::<...>`);
- `params` -- the method parameters object;
- `userpass` -- the RPC password (see §43.3);
- `id` (optional) -- echoed back in the response.

R43.1.3 An unrecognised `mmrpc` version shall not be fatal: the endpoint shall
answer with a structured, well-formed version-mismatch error response rather
than failing to produce a valid response at all. The error response's own
version tag defaults to the latest known version so the envelope stays
well-formed; the triggering request is refused (as a version-mismatch error),
not processed under the fallback version.

R43.1.4 A response shall carry the matching version and `id`, and either a result
or a structured error object.

> **Cross-reference (informative).** The coin-activation methods' error
> envelope, the encoding of `error_data`, the two error placements
> (mmrpc-level and task-status-level), the HTTP-status policy and the legacy
> error body are bound in [Chapter 55](55-coin-activation-wire-contract.md)
> §55.1, which also restates the dispatcher errors of this section that
> activation callers observe.

## 43.1A Order-management methods on the two request surfaces (reference behavior)

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-08,
> measure 0.02 (attestation in §43.7).**
> This section, the two acceptance bullets of §43.6 that cite it, the scoped
> record in §43.7 and the pointer added to the Reloaded-status note of
> chapter 55 §55.1 were added on 2026-11-08 by the KDF Spec Reader under the
> two-team procedure of chapter 01 §1.4A, from
> forbidden-class input: the reference lineage at the rows `v2.6.0-beta` and
> `v3.0.0-beta` of chapter 02 §2.15A (identical for every fact taken); the
> method tables alone were also checked at the rows `dev (as of 2026-03-27)`
> and `main (as of 2026-10-07)` and are identical there. The clearances of
> the rest of this chapter are
> not affected: §43.0–§43.6 as they stood before this addition rest on this
> repository's own tree and took no forbidden-class input.

*Scope.* §43.1 binds the two request shapes; this section binds **which of
the order-management and swap-history methods exist on each surface**, how
the reference answers a legacy-only method called under the versioned
(`mmrpc`) envelope, and the versioned request, response and error shapes of
the three order methods that exist on both surfaces. It was prompted by a
wallet in the field that sends `setprice`, `cancel_order` and `my_orders`
with `"mmrpc": "2.0"` and receives `NoSuchMethod` from Reloaded; the question
was whether that is a Reloaded gap. It is not: the reference answers the same
(R43.1A.4, R43.1A.9). The semantics of the methods themselves stay where they
are bound — order creation, update and cancellation in [Chapter 11](11-order-match-cancellation.md),
[Chapter 12](12-order-match-state-store.md) and [Chapter 59](59-legacy-ordermatch-negotiation.md);
the orderbook and best-orders content, including the address member and the
shielded-coin case, in [Chapter 32](32-orderbook-p2p-and-trie.md); swap
records in [Chapter 51](51-legacy-v1-swap-state-machine.md) and
[Chapter 52](52-swap-v2-state-machine.md).

**R43.1A.1** *Envelope selection is keyed on the `mmrpc` key alone, and the
versioned surface never falls back to the legacy one.* A request whose
top-level `mmrpc` key is absent or null is a legacy request and is looked up
in the legacy method table. A request that carries `mmrpc` is a versioned
request and is looked up in the versioned method table **only**: a versioned
request naming a method that only the legacy table knows is answered exactly
like any unknown versioned method (R43.1A.3). The reverse direction has a
one-way fallback in the reference: a legacy request whose method the legacy
table does not know is re-dispatched once on the versioned table as an
mmrpc 2.0 request, after the legacy authentication of §43.3 has already
passed, and its answer — a result, a typed error, or `NoSuchMethod` when
neither table knows the name — is rendered in the mmrpc 2.0 envelope
(chapter 55 R55.1.1–R55.1.2) rather than in the legacy body. Reloaded does
not implement this fallback (compatibility note below; chapter 55 §55.1
Reloaded-status note); D43.1A.2 decides whether it should.

> *Reloaded status (clean side, 2026-11-08):* D43.1A.2 is resolved as
> option (i). Since PR #312 Reloaded implements the fallback, and
> R43.1A.1a–R43.1A.1e bind it. The last sentence of R43.1A.1 and the
> conditional wording of the fallback-detail lead and of R43.1A.1e describe
> the state before that change.

> **Fallback detail (R43.1A.1a--R43.1A.1e; added 2026-11-08, round
> 0.4.0-8b) -- passed the KDF Dirty Gate on 2026-11-08, measure 0.01
> (attestation in §43.7).** The five sub-rules below refine the one-way
> legacy-to-versioned fallback that R43.1A.1 names, as the reference behaves
> at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A
> (identical at both for every fact taken). They do not alter the cleared
> text of R43.1A.1--R43.1A.9. They are what option (i) of D43.1A.2 would
> bind for Reloaded, and they are the mechanism chapter 13 T11 refers to as
> the compatibility fallback into mmrpc 2.0. Which Reloaded answer the
> fallback yields today is in item 2 of the compatibility note below.

**R43.1A.1a** *Trigger.* The fallback has exactly one trigger: the legacy
access checks (the local-only check of §43.2, the ban check and the legacy
authentication of §43.3, in that order) have passed and the legacy method
table has no entry for the request's `method` value -- including the case
that the top-level `method` key is absent or is not a JSON string. Nothing
else triggers it: a legacy request refused by the local-only or ban check,
one that fails legacy authentication, and one whose method the legacy table
knows but whose handler fails or reports an error are each answered with the
legacy error body (chapter 55 R55.1.10: HTTP 500, `{"error": <string>}`) and
are never re-dispatched. The re-dispatch happens at most once per request:
the converted request carries `mmrpc`, and the versioned surface never falls
back to the legacy one (R43.1A.1).

**R43.1A.1b** *Conversion.* The converted request is the legacy body **as
received**, with the single change that its top-level `mmrpc` key is set to
`"2.0"`; no member is dropped, renamed, moved or synthesized. That object is
then decoded under the full strictness of the versioned envelope (R43.1A.2):
it decodes only if its remaining top-level keys are a subset of `userpass`,
`method`, `params` and `id`, with `method` a string, `userpass` absent or a
string, `id` absent or a non-negative integer, and `params` of any JSON type
(absent is treated as null). Any other inline member -- in particular
legacy-style parameters placed next to `method` -- makes the converted request
undecodable, and it is answered `InvalidRequest` (string payload, HTTP 400)
before any method lookup. The method's parameters come only from the body's
`params` member; a method that needs parameters and finds `params` null or
ill-shaped answers `InvalidRequest` from its own parameter decoding.
Consequence: the fallback serves exactly those legacy requests that are
already shaped as versioned requests minus the `mmrpc` key; a genuinely
legacy-shaped body never reaches a versioned handler through it.

**R43.1A.1c** *Answer envelope.* Every answer produced through the fallback
is the mmrpc 2.0 envelope (chapter 55 R55.1.1--R55.1.2) with `mmrpc` equal to
`"2.0"`, never the legacy body. `id` is echoed when the body carries a
non-negative integer `id` and is `null` otherwise, on every outcome; an `id`
of any other JSON type is reported as `null` in a dispatcher-level error and,
by R43.1A.1b, makes the converted request undecodable, so a handler is never
reached with it. HTTP statuses: a result -- 200; a method's typed error --
the status the method's own error table binds for that `error_type` (chapter
55 R55.1.5); `NoSuchMethod`, when neither table knows the name -- 400, no
`error_data`; `InvalidRequest`, from the conversion or from a method's
parameter decoding -- 400, string payload.

**R43.1A.1d** *Access checks on the re-dispatch.* The converted request runs
the versioned dispatcher's own local-only, ban and authentication checks
again, in that order, after the envelope has decoded and before the method
lookup -- the same checks a request that carried `mmrpc` from the start
faces (R43.1A.3). The legacy checks it has already passed test the same
public-method set, the same loopback condition, the same ban list and the
same credential key (`userpass` compared with the configured
`rpc_password`), so the second run cannot fail where the first passed; the
403 discriminants of R43.1A.3 are therefore not observable through the
fallback, and the only dispatcher-level errors the fallback adds are
`InvalidRequest` and `NoSuchMethod`. An unknown method name is never in the
public set, so a legacy request naming one is **protected** at the legacy
layer: without a string `userpass`, or with a wrong one, it is refused before
the fallback with the legacy error body (HTTP 500), and a wrong password
counts against the caller's rate-limit ban exactly as for a known protected
legacy method. Two cases reach the fallback without a password, both
answered in the mmrpc 2.0 envelope: a legacy request whose `method` key is
absent or not a string (R43.1A.1a) is treated as public by both layers and
answers `InvalidRequest` (400); and a legacy request naming a method of the
public set (§43.2) that neither table serves answers `NoSuchMethod` (400).

**R43.1A.1e** *Shared envelope decoding (informative for Reloaded).* The
converted request passes through the same versioned envelope decoder as a
native versioned request, so whatever top-level keys that decoder accepts
apply to it too; in the reference that decoder accepts only the five keys of
R43.1A.2. Should Reloaded adopt the fallback (D43.1A.2 option (i)), two
consequences follow from its `rpc_pass` alias (R43.1A.2; compatibility note
item 3): a legacy body carrying `rpc_pass` would decode under Reloaded's
versioned envelope where the reference answers `InvalidRequest`; but because
the legacy authentication of R43.1A.1d runs first and, in this repository's
dev branch as of 2026-11-08, Reloaded's legacy layer tests `userpass` alone,
a protected unknown name sent with `rpc_pass` only is refused before the
fallback in both implementations unless Reloaded's legacy layer also accepts
the alias. Which keys Reloaded's legacy layer authenticates is a Reloaded
decision, not a reference fact.

**R43.1A.2** *Versioned envelope strictness.* The versioned request object
admits exactly the top-level keys `mmrpc`, `userpass`, `method`, `params` and
`id`. Any other top-level key makes the request undecodable, which answers
`InvalidRequest` (string payload, HTTP 400) before any method lookup. `params`
MAY be absent, in which case it is treated as null and a method that needs
parameters answers `InvalidRequest` from its own decoding. Consequence for
clients: a legacy-shaped request (parameters inline next to `method`) with an
added `"mmrpc": "2.0"` is refused as `InvalidRequest`, not routed to the
legacy handler. *(Reloaded additionally accepts the key `rpc_pass` as an
alias of `userpass`; the reference does not. Reloaded extension, informative;
see the compatibility note.)*

**R43.1A.3** *Unknown method under the versioned envelope.* The answer is
the mmrpc error envelope of chapter 55 R55.1.2 with `error_type` =
`NoSuchMethod`, no `error_data`, HTTP 400, `mmrpc` echoing the request's
version and `id` echoed (or `null`). The dispatcher's own `error_type` tokens
and statuses are: `NoSuchMethod` (no payload), `InvalidRequest` (string) and
`InvalidMmRpcVersion` (string) — HTTP 400; `LocalHostOnly` (no payload),
`UserpassIsNotSet` (no payload), `UserpassIsInvalid` (payload: the
rate-limiter detail) and `Banned` (no payload) — HTTP 403. The local-only
check of §43.2, the ban check and the authentication of §43.3 run **before**
the method lookup, so an unauthenticated or non-loopback call of an unknown
protected method answers 403, not 400; only `orderbook` among the methods of
this section is public and reaches the lookup without a password.

**R43.1A.4** *Surface table.* At both release rows the reference routes the
order-management and swap-history methods as follows. "yes/yes" means the
same wire name is registered on both tables with the shapes this section
binds; "yes/no" means the method is reachable **only** through a legacy
request and a versioned request for it answers `NoSuchMethod` (R43.1A.3).

| Wire method | Legacy surface | Versioned surface (`mmrpc: "2.0"`) | Notes |
| --- | --- | --- | --- |
| `setprice` | yes | **no** | request and result shapes are the legacy ones (chapters 11, 12, 59) |
| `buy`, `sell` | yes | **no** | |
| `update_maker_order` | yes | **no** | |
| `cancel_order` | yes | **no** | |
| `cancel_all_orders` | yes | **no** | |
| `my_orders` | yes | **no** | takes no parameters on the legacy surface |
| `order_status` | yes | **no** | |
| `orderbook_depth` | yes | **no** | |
| `orderbook` | yes | yes | public (no password) on both surfaces; shapes in R43.1A.6 |
| `best_orders` | yes | yes | R43.1A.5 |
| `my_recent_swaps` | yes | yes | R43.1A.7 |
| `trade_preimage`, `my_swap_status`, `active_swaps` | yes | yes | outside this section; bound by chapters 08, 51 and 52 |

The six "yes/no" rows — the methods the field wallet calls — were never
registered on the versioned table in the reference lineage: not at
`v2.6.0-beta`, not at `v3.0.0-beta`, and not at the later `dev (as of
2026-03-27)` and `main (as of 2026-10-07)` rows. A client that wants them
MUST send a legacy request (no `mmrpc` key, parameters inline).

**R43.1A.5** *`best_orders` on the versioned surface.* Request `params`:
`coin` (string); `action` (`"buy"` or `"sell"`); `request_by` (object with
`type` and `value`: `type` = `"volume"` with `value` a decimal amount, given
as a decimal string or number, or `type` = `"number"` with `value` a positive
integer count of orders); `exclude_mine` (boolean, default false — when true
the caller's own orders are left out). Response `result`: `orders` — an
object keyed by ticker whose values are lists of versioned orderbook entries
(R43.1A.8); `original_tickers` — an object keyed by orderbook ticker whose
values are the lists of original tickers that orderbook ticker aggregates.
Errors: `CoinIsWalletOnly` (string, 400), `P2PError` (string, 500),
`CtxError` (string, 500). *Differences from the legacy form:* the legacy
request is `coin`, `action`, `volume` (by-volume only; no by-number query, no
own-order exclusion); the legacy response is a top-level object with `result`
(ticker → list of **legacy** entries, R43.1A.8) and `original_tickers`; a
legacy failure is the legacy error body (chapter 55 R55.1.10).

**R43.1A.6** *`orderbook` on the versioned surface.* Request `params`: `base`
and `rel` (strings). Response `result`: `asks` and `bids` — lists whose items
are a versioned orderbook entry (R43.1A.8) extended with
`base_max_volume_aggr` and `rel_max_volume_aggr`, the running totals of the
list up to and including that item, as multi-representation numbers; `base`,
`rel` (strings); `net_id` (integer); `num_asks`, `num_bids` (integers);
`timestamp` (seconds); `total_asks_base_vol`, `total_asks_rel_vol`,
`total_bids_base_vol`, `total_bids_rel_vol` (multi-representation numbers).
Both lists are ordered best price first (bids by descending price). Errors:
`BaseRelSame` (no payload, 400), `BaseRelSameOrderbookTickersAndProtocols`
(no payload, 400), `CoinConfigNotFound` (string, 400), `CoinIsWalletOnly`
(string, 400), `P2PSubscribeError` (string, 500), `Internal` (string, 500).
*Differences from the legacy form:* the legacy response object is the body
itself (no `result` wrapper) and spells the counts and network as `numasks`,
`numbids`, `netid`, carries `askdepth` and `biddepth`, and gives each total
as a decimal member with `_fraction` and `_rat` companion members; its entries
are the legacy entries of R43.1A.8; a legacy failure is the legacy error body.
`orderbook` is public on both surfaces (§43.2).

**R43.1A.7** *`my_recent_swaps` on the versioned surface.* Request `params`
(flat object): `limit` (integer, default 10); `page_number` (positive integer,
default 1); `from_uuid` (UUID string, optional — the page then starts after
that swap); `my_coin`, `other_coin` (strings, optional); `from_timestamp`,
`to_timestamp` (integers, optional). Response `result`: `swaps` — a list of
tagged records `{"swap_type": <tag>, "swap_data": <record>}` where the tag is
one of `MakerV1`, `TakerV1`, `MakerV2`, `TakerV2` and the record is the
stored swap record of that generation; `from_uuid` (UUID string or null);
`skipped`, `limit`, `total`, `page_number`, `total_pages`, `found_records`
(integers). A swap the filter matched whose stored record cannot be loaded is
omitted from `swaps` but still counted in `found_records`; it is not an
error. Errors: `FromUuidSwapNotFound` (UUID string, 400),
`InvalidTimeStampRange` (no payload, 400), `DbError` (string, 500).
*Differences from the legacy form:* the legacy request takes the same
members; the legacy response wraps the same paging members in `result`, but
each `swaps` item is the **untagged** record — for a v1 swap the projection
the legacy swap-status method returns, for a v2 swap the v2 record — and a
legacy failure is the legacy error body.

**R43.1A.8** *The two orderbook-entry shapes.* The **versioned entry** has the
members `coin` (string), `address` (the tagged address object of chapter 32:
`address_type` `"Transparent"` with `address_data` a string, or
`address_type` `"Shielded"` without data), `price`, `pubkey` (string), `uuid`
(string), `is_mine` (boolean), `base_max_volume`, `base_min_volume`,
`rel_max_volume`, `rel_min_volume`, and `conf_settings` (object with
`base_confs`, `base_nota`, `rel_confs`, `rel_nota`, or null). Every number in
it is a **multi-representation object** with the members `decimal` (decimal
string), `rational` and `fraction` (the workspace rational and fraction
encodings). The **legacy entry** gives `address` as a plain string, each
number as a decimal member with `_rat` and `_fraction` companion members
(`price`, `price_rat`, `price_fraction`; `maxvolume`, `max_volume_rat`,
`max_volume_fraction`; `min_volume` and companions; `base_max_volume`,
`base_min_volume`, `rel_max_volume`, `rel_min_volume` each with companions),
and additionally carries `age` and `zcredits`, which the versioned entry
drops. Which address a shielded-coin order shows on each surface is bound by
chapter 32.

**R43.1A.9** *Reference states.* Everything in R43.1A.1–R43.1A.8 is identical
at the rows `v2.6.0-beta` and `v3.0.0-beta`; the method tables of R43.1A.4
are also identical at `dev (as of 2026-03-27)` and `main (as of 2026-10-07)`.
The reference therefore never offered `setprice`, `cancel_order`,
`my_orders`, `update_maker_order`, `cancel_all_orders` or `order_status`
under the versioned envelope, and a client that calls them that way fails
against the reference exactly as it fails against Reloaded.

> **Compatibility (this repository, 2026-11-08; not corpus-sourced).**
> Checked against this repository's dev branch as of 2026-11-08:
>
> 1. *Method tables.* Reloaded's versioned table carries the same three order
>    methods (`orderbook`, `best_orders`, `my_recent_swaps`) and its legacy
>    table the same legacy-only set as R43.1A.4; a versioned `setprice`,
>    `cancel_order` or `my_orders` answers `NoSuchMethod`, HTTP 400, as
>    R43.1A.3 binds. **Parity; no gap.** The field wallet's calls are
>    malformed for both implementations.
> 2. *Fallback.* Until PR #312, Reloaded answered a legacy request for an
>    unknown method with the legacy error body (HTTP 500) and did not retry it
>    on the versioned table (R43.1A.1). Since PR #312 (D43.1A.2 option (i),
>    the maintainer's decision of 2026-11-08), it retries such a request as
>    R43.1A.1a–R43.1A.1e bind. **Parity.**
> 3. *Envelope alias.* Reloaded's versioned envelope accepts `rpc_pass` as an
>    alias of `userpass` (R43.1A.2). A Reloaded extension; harmless to
>    reference clients, which never send it.
>    Through the fallback (R43.1A.1e), Reloaded's legacy layer still tests
>    `userpass` alone, so a protected name sent with `rpc_pass` only is
>    refused before the fallback, as in the reference. A legacy body that
>    names a public method neither table serves, with `rpc_pass` and no
>    `userpass`, decodes under the alias and answers `NoSuchMethod` where the
>    reference answers `InvalidRequest`.
> 4. *`orderbook` error token.* Until PR #312, Reloaded's versioned
>    `orderbook` named its internal-failure class `InternalError` where the
>    reference names it `Internal` (R43.1A.6). It now names it `Internal`
>    (D43.1A.3, the maintainer's decision of 2026-11-08). **Parity.**
> 5. *`best_orders` error set.* Reloaded's versioned `best_orders` has no
>    `CtxError` case (R43.1A.5); it never raises that class, so its observable
>    token set is a subset of the reference's. Not a divergence.
> 6. *Shapes.* The versioned request, response and entry members of
>    R43.1A.5–R43.1A.8 and the `my_recent_swaps` tags and error tokens of
>    R43.1A.7 match.

**D43.1A.1** *Offer the legacy-only order methods under the versioned
envelope?* The reference does not (R43.1A.4), so any such offer is a Reloaded
extension with no reference shape to follow. Options: (i) parity — keep
answering `NoSuchMethod` and have the wallet send legacy requests
(recommended; it is the only option whose behavior a reference client can
rely on, and plan `rpc-api-reference-wire-parity` Part 2 makes an undocumented
extension a deviation); (ii) register versioned aliases that take the legacy
parameter set inside `params` and return the legacy result under `result`,
with the legacy string failure mapped to one typed `error_type` — a
documented deliberate divergence under that plan's §2.2; (iii) an inverse
fallback that re-dispatches a versioned request for a legacy-only name on the
legacy table — rejected on principle, because it would make the versioned
envelope's strictness (R43.1A.2) and typed errors (R43.1A.3) unreliable.
*Resolved 2026-11-08 by the maintainer's decision: option (i). The
legacy-only order methods stay legacy-only, and no versioned aliases are
added.*

**D43.1A.2** *Implement the reference's legacy-to-versioned fallback
(R43.1A.1)?* Options: (i) parity — a legacy request for an unknown legacy
name is retried on the versioned table and its answer rendered in the mmrpc
2.0 envelope, so that a client can call every versioned method without
`mmrpc`; (ii) keep the present behavior (legacy error body, HTTP 500) as a
documented divergence. The wallet case of this section is not affected
either way (its requests carry `mmrpc`). *Resolved 2026-11-08 by the
maintainer's decision: option (i), PR #312.*

**D43.1A.3** *Rename Reloaded's versioned `orderbook` internal-failure token
to `Internal`?* A one-token change that restores the closed discriminant set
of R43.1A.6; recommended, since no client contract depends on the Reloaded
spelling. *Resolved 2026-11-08 by the maintainer's decision: renamed, PR
#312.*

## 43.2 Public vs protected method classification

R43.2.1 A fixed set of **public** methods shall be callable without
authentication. The public set shall include at least the read-only / discovery
methods (orderbook, ticker/price lookups, metrics, peer/coin discovery, help, and
public swap-status/stats queries). Methods not in the public set are
**protected**.

R43.2.2 When the daemon is configured **local-only**, a protected method invoked
from a non-loopback client shall be refused; public methods remain reachable.
(Two of the public methods additionally perform their own caller checks -- a peer
public-key check and a passphrase check -- beyond the table classification.)

R43.2.3 The `version` query shall be answerable without authentication.

## 43.3 RPC authentication & password policy (security obligation)

R43.3.1 Every protected method shall require the caller to present the configured
RPC password (`userpass` / `rpc_password`); a missing or wrong password shall be
rejected before the method runs.

R43.3.2 The configured `rpc_password` shall be validated at startup against a
strength policy and the daemon shall refuse to start (or refuse to accept the
config) if it fails. The policy shall require **all** of:
- it is a non-empty string;
- length within a bounded range (at least 8 and at most 32 characters);
- contains at least one digit, one lowercase letter, one uppercase letter, and
  one non-alphanumeric (special) character;
- does **not** contain the literal substring "password" (case-insensitive); and
- does not exceed the allowed number of consecutive repeated characters.

> **Security note.** R43.3.2 is a hardening invariant. Any change to the auth
> path must preserve at least this strength floor; weakening it (shorter minimum,
> dropping a character-class requirement, or removing the repeated-character cap)
> is a regression. Verified: the shipped policy enforces all of the above.

> **Code-quality finding (informative, resolved 2026-08-27).** The
> request-time authentication comparison of R43.3.1 used to substitute an
> empty string for the configured password whenever `rpc_password` was
> absent, rather than refusing every comparison outright: a caller who
> explicitly supplied an empty (present, non-null) credential satisfied that
> comparison whenever `rpc_password` was unconfigured, gaining access to
> every protected method without ever having been given a password -- which
> did not honour R43.3.1's "a missing or wrong password shall be rejected"
> floor, nor Chapter 45 R45.5.2's requirement that an absent `rpc_password`
> "shall not silently grant unauthenticated access." The sibling case --
> `rpc_password` explicitly configured as an empty string -- was already
> refused at startup, so the gap was specific to the *absent* case. Closed
> by making the authentication comparison itself reject every caller
> credential, including an explicitly empty one, whenever no `rpc_password`
> is configured -- routed through the same rate-limited invalid-password
> path a wrong password already takes, so an unconfigured node gives a
> caller no signal distinguishing "no password set" from "wrong password."
> The startup strength-policy validation of R43.3.2 still only runs when
> `rpc_password` is present (a genuinely absent `rpc_password` still only
> logs a diagnostic at startup and does not refuse to start) -- this is now
> a pure availability/UX question, not a security gap, since no request can
> authenticate against an unconfigured password regardless. Code:
> `mm2src/mm2_main/src/rpc/dispatcher/dispatcher.rs` (`auth`).

R43.3.3 The RPC password and passphrase shall never be logged or echoed in
responses or errors.

## 43.4 No-panic-via-RPC (security obligation)

R43.4.1 No RPC request -- malformed JSON, wrong types, out-of-range values,
unknown method, or unknown version -- shall be able to panic or crash the daemon.
Invalid input shall produce a structured JSON error response and the daemon shall
continue serving. Method handlers in the RPC path shall avoid unwrap/expect on
caller-controlled data.

## 43.5 Daemon lifecycle & graceful shutdown

R43.5.1 The daemon shall install OS termination-signal handling and, on receiving
a termination signal, shall initiate a graceful shutdown: stop accepting new
requests, allow in-flight work a bounded opportunity to finish, persist necessary
state, and exit cleanly.

R43.5.2 Background subsystems (P2P, swap drivers, streaming, coin event loops)
shall be wound down as part of shutdown rather than abandoned.

## 43.6 Acceptance criteria

- A legacy request and a versioned (`mmrpc`) request both route correctly; an
  unknown `mmrpc` version yields a structured version-mismatch error response
  (its own envelope defaulting to the latest known version) rather than a
  raw or invalid response (§43.1).
- A public method succeeds without a password; a protected method fails without
  the correct password and, in local-only mode, fails from a non-loopback client
  (§43.2).
- Startup rejects an `rpc_password` that violates any clause of the strength
  policy (§43.3.2).
- A battery of malformed RPC requests yields structured errors and leaves the
  daemon running (§43.4).
- A termination signal triggers a clean, bounded shutdown of the daemon and its
  subsystems (§43.5).
- *(§43.1A; passed the KDF Dirty Gate on 2026-11-08.)* A versioned (`mmrpc: "2.0"`)
  request for each of `setprice`, `buy`, `sell`, `update_maker_order`,
  `cancel_order`, `cancel_all_orders`, `my_orders`, `order_status` and
  `orderbook_depth` answers the mmrpc error envelope with `error_type`
  `NoSuchMethod`, no `error_data`, HTTP 400 and the request's `id` echoed;
  the same request without `mmrpc` and with its parameters inline succeeds
  (R43.1A.3, R43.1A.4). A versioned request with a legacy-shaped body plus
  `"mmrpc": "2.0"` answers `InvalidRequest`, HTTP 400 (R43.1A.2).
- *(§43.1A; passed the KDF Dirty Gate on 2026-11-08.)* Versioned `orderbook`,
  `best_orders` and `my_recent_swaps` answer the member sets of
  R43.1A.5–R43.1A.8 under `result`, with every number a
  `decimal`/`rational`/`fraction` object and every `my_recent_swaps` item
  tagged `swap_type`/`swap_data`; their legacy forms answer the legacy member
  sets of the same rules; each error table's `error_type` tokens carry the
  bound HTTP statuses, `orderbook` reporting `Internal` for its internal
  failure class (R43.1A.6; D43.1A.3).
- *(§43.1A fallback detail R43.1A.1a--R43.1A.1e; passed the KDF Dirty Gate on
  2026-11-08 -- reference behavior; binds Reloaded only if D43.1A.2 is decided
  as option (i).)* *(Decided: option (i), PR #312.)* A legacy request (no `mmrpc`) for a versioned-only method
  whose body has only `userpass`, `method`, `params` and `id` answers that
  method's result or typed error in the mmrpc 2.0 envelope with `mmrpc`
  `"2.0"`, the method's bound HTTP status and the request's `id` echoed
  (R43.1A.1b, R43.1A.1c); the same request with one extra inline member
  answers `InvalidRequest`, HTTP 400, in the mmrpc 2.0 envelope (R43.1A.1b);
  the same request without `userpass` answers the legacy error body, HTTP 500,
  and is not re-dispatched (R43.1A.1d); a legacy request for a name neither
  table knows answers `NoSuchMethod`, HTTP 400, in the mmrpc 2.0 envelope with
  `id` echoed or `null` (R43.1A.1c); a known legacy method whose handler fails
  answers the legacy error body, HTTP 500, and is not re-dispatched
  (R43.1A.1a).

## 43.7 Provenance Footer

- *Inputs:* the project's own revision history and current tree (the
  shipped dispatcher -- legacy and namespaced routing, public-method
  classification, password authentication -- and the daemon lifecycle
  this chapter documents as-built and binds the security obligations of,
  by public behaviour and wire-envelope shape only, no code
  transcribed); the 2026-08-26 external audit's KDF-001 finding (the
  auth-bypass gap this chapter's §43.3 code-quality finding records and
  the fix it now describes as resolved); [Chapter 45](45-startup-configuration-and-environment-tolerance.md)
  (the startup-tolerance contract R45.5.2/R45.7.1 this chapter's R43.3.1/
  R43.4.1 security obligations are bound alongside).
- *Permitted-input classes used:* baseline/as-built source (the shipped
  dispatcher and daemon lifecycle); R7 (independent work, for the
  security-fix description); cross-chapter contracts (Chapter 45).
- *Sibling-allowlist consultations:* [Chapter 45](45-startup-configuration-and-environment-tolerance.md).
- *Forbidden corpus:* not consulted for §43.0–§43.6 as they stood before
  2026-11-08, except as the scoped addition below records.
- *Scoped addition (2026-11-08) -- §43.1A (R43.1A.1--R43.1A.9, the
  compatibility note, D43.1A.1--D43.1A.3), the two §43.6 acceptance bullets
  that cite it, and the pointer added to the Reloaded-status note of chapter
  55 §55.1.* **KDF Dirty Gate: PASS on 2026-11-08, measure 0.02, no
  findings** (chapter 01 R40; the attestation closes this item). *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, identical at both for every fact taken,
  with the method tables alone also checked at the rows `dev (as of
  2026-03-27)` and `main (as of 2026-10-07)`, consulted **only** to
  establish: which key of a request selects the legacy or the versioned
  surface, and which one-way fallback exists between them; which top-level
  keys the versioned envelope admits and how a request with others is
  answered; the dispatcher's `error_type` tokens, their payload kinds and
  HTTP statuses, and the order of the access checks relative to the method
  lookup; which order-management and swap-history wire names each surface
  routes; and the request, response, entry and error member sets, payload
  kinds and HTTP statuses of the versioned `best_orders`, `orderbook` and
  `my_recent_swaps` together with their legacy counterparts' member sets.
  What crosses is behavior, public wire method names and dictated request,
  response and error members; no private identifier, function body, helper
  decomposition, error, log or comment text was reproduced. The compatibility
  note rests on the dispatcher, ordermatch and swap-RPC code of this
  repository's dev branch as of 2026-11-08; D43.1A.1--D43.1A.3 are
  Reloaded-original. No row was added to chapter 02 §2.15A: every state read
  already had one, and each row's identifier and date were re-confirmed by
  read-only inspection of the historical record's reference metadata before
  it was named. *Dirty Gate clearance (chapter 01 T1, R40):* PASS,
  2026-11-08. Extent: the forbidden corpus was compared for the subject
  matter of §43.1A — the request-envelope and method-routing surface of both
  request shapes, the versioned protocol envelope and its typed dispatcher
  errors and access-check ordering, the orderbook and best-orders RPC
  surface, the swap-history RPC surface on both request shapes, and the
  paging and multi-representation-number wire shapes — at the row
  v3.0.0-beta as the primary state (twelve source files examined, nine in
  full and three in the regions carrying this subject matter), and the same
  twelve files at the rows v2.6.0-beta, dev (as of 2026-03-27) and main (as
  of 2026-10-07) by comparison against the primary state; the two branch
  rows are identical to the primary over the whole set, and the v2.6.0-beta
  row differs from it only outside every fact this section takes. Method: by
  reading at the primary row, combined with mechanical version-control
  comparison of the same file set between the primary row and each of the
  other three rows; the repository's clean-room gate tool was not run
  because the candidate embeds no code blocks. Measure:
  discretionary-expression similarity 0.02 on the R35 basis
  (dictated-interop and public-surface tokens excluded), read against the
  standard that a candidate passes only when every corpus-matching token is
  forced by a public or serialized interface and no function body,
  control-flow transcription, private non-wire identifier, discretionary
  error or log wording, internal module tree or test name is present; none
  was found.
- *Scoped addition (2026-11-08, second pass, round 0.4.0-8b) -- the
  fallback-detail sub-block R43.1A.1a--R43.1A.1e under R43.1A.1 and the §43.6
  acceptance bullet that cites it.* **KDF Dirty Gate: PASS on 2026-11-08,
  measure 0.01, no findings** (chapter 01 R40; the attestation closes this
  item). *Inputs:* the forbidden corpus at the rows `v2.6.0-beta` and
  `v3.0.0-beta` of chapter 02 §2.15A, the latter read as the primary state and
  the former checked by mechanical comparison of the same files (identical for
  every fact taken), consulted **only** to establish: what triggers the
  legacy-to-versioned re-dispatch and what does not; which members of the
  legacy body form the converted request and under which strictness it is
  decoded; the version tag, the `id` echo and the HTTP status of each fallback
  outcome; which access checks run again on the re-dispatch and which of them
  are observable; and whether an unknown legacy name is treated as protected.
  What crosses is behavior and the dictated envelope members; no private
  identifier, function body, helper decomposition, error, log or comment text
  was reproduced. The statements about Reloaded in R43.1A.1e rest on the
  dispatcher and envelope code of this repository's dev branch as of
  2026-11-08. No row was added to chapter 02 §2.15A: both states read already
  had one. *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-08,
  second pass. Extent: the forbidden corpus was compared for the subject
  matter of the R43.1A.1a–R43.1A.1e sub-block — the request entry layer that
  selects between the two request shapes and performs the one-way
  legacy-to-versioned re-dispatch, the legacy dispatcher's access checks and
  method table, the versioned dispatcher's entry and access checks, the
  versioned protocol envelope and its typed dispatcher errors, the request
  rate-limit/ban mechanism, and the legacy error-body encoding — at the row
  v3.0.0-beta as the primary state (seven source files examined: six in full
  and one in the region carrying the legacy error-body encoding), and the same
  seven files at the row v2.6.0-beta by mechanical version-control comparison
  against the primary state; over that set the two rows differ only in
  copyright-notice lines and in one region outside every fact the sub-block
  takes. Method: by reading at the primary row, combined with mechanical
  version-control comparison of the same file set between the two rows, and a
  mechanical five-word-shingle overlap check of the candidate passages against
  the compared files (zero shared shingles); the repository's clean-room gate
  tool was not run because the candidate embeds no code blocks. Measure:
  discretionary-expression similarity 0.01 on the R35 basis (dictated-interop
  and public-surface tokens excluded), read against the standard that a
  candidate passes only when every corpus-matching token is forced by a public
  or serialized interface and no function body, control-flow transcription,
  private non-wire identifier, discretionary error or log wording, internal
  module tree or test name is present; none was found.
