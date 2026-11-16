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

> *Reloaded status (clean side, 2026-11-15):* the resolution above describes
> KDF Reloaded v0.4.0. For v0.5.0 the maintainer scheduled the documented
> extension of `docs/plans/mmrpc-v2-completion.md`: every method that only the
> legacy table knows, except `electrum`, `enable`, `help`, `show_priv_key`,
> `sim_panic`, `stats_swap_status` and `get_trade_fee` (D43.1B.1), gains a
> versioned form under the same wire name (40 methods), built in five phases (order management; trading; wallet and
> coins; swaps; daemon and network). The table of R43.1A.4 keeps the
> reference's surfaces; the Reloaded surface of each phase is recorded here
> as the phase merges, as a deliberate divergence under the plan's §1.6 and
> `rpc-api-reference-wire-parity.md` §2.2, with no compatibility switch. The
> legacy forms, the fallback of R43.1A.1a–R43.1A.1e and the rejection of
> option (iii) are unchanged.
>
> *Shape rules of the extension (the maintainer's decisions of 2026-11-15):*
> (1) a versioned form takes the legacy members unchanged inside `params`,
> with the same names, types, optionality and defaults; a member is redesigned
> only if a versioned form later gains a capability the legacy form lacks.
> (2) The legacy result goes under `result` unchanged; for `order_status`
> that is the object with `type`, `order` and, for a history order,
> `cancellation_reason`. (3) An unknown member inside `params` is accepted and
> ignored, as the legacy form accepts it; only the envelope keys of R43.1A.2
> stay strict. Reason: a client built against a later form of a method keeps
> working against an older node, and the envelope check already refuses a
> malformed request before the method is reached. This rule is meant to be
> stated in a later reference documentation of the versioned forms.
> (4) Failures are typed: each failure class a handler can produce has an
> `error_type` with an HTTP status, 4xx for a request the client got wrong
> and 5xx for a failure of the node; the per-method tables live in the
> chapters that govern the methods.
>
> *Phase 1 (order management; this repository, 2026-11):* `setprice`,
> `update_maker_order`, `cancel_order`, `cancel_all_orders`, `my_orders` and
> `order_status` are routed on both surfaces. Their legacy answers are
> unchanged. Per-method shapes and error tables: §43.1R below
> (Reloaded-original, clean side).
>
> *Phase 2 (trading; this repository, 2026-11):* `buy`, `sell`,
> `orderbook_depth` and `orders_history_by_filter` are routed on both surfaces;
> `get_trade_fee` stays legacy-only (D43.1B.1). Per-method shapes and error
> tables: §43.1R (dated amendment).
>
> *Phase 3 (wallet and coins; this repository, 2026-11):* `my_balance`,
> `disable_coin`, `send_raw_transaction`, `validateaddress`, `convertaddress`,
> `convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`,
> `set_required_confirmations` and `set_requires_notarization` are routed on
> both surfaces. Per-method shapes and error tables: §43.1R (dated amendment).

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

## 43.1B Documented API forms of the legacy-only methods (reference documentation)

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-10,
> measure 0.01 (attestation in §43.7).**
> This section was written on 2026-11-10 by the KDF Spec Reader (Spec Reader
> round 0.4.0-13) under the two-team procedure of chapter 01 §1.4A, from three
> inputs: the published reference API documentation at
> `https://gleec.com/komodo/docs/komodo-defi-framework/api/` (the method
> index, the v2.0 protocol overview page, the site map, and the legacy page of
> every name below; fetched 2026-11-10; a reference-side input that only the
> two roles of chapter 01 R37 read); the versioned method tables of the
> reference lineage at the rows `v2.6.0-beta`, `v3.0.0-beta`, `dev (as of
> 2026-03-27)` and `main (as of 2026-10-07)` of chapter 02 §2.15A (read
> read-only; no untabled state was read); and this repository's own tree.

*Scope.* §43.1A binds what the reference **implementation** does with the
order-management methods on the two request surfaces. This section records
what the reference **documentation** says about the 44 wire names that
Reloaded's legacy table routes and its versioned table does not — the
inventory of plan `mmrpc-v2-completion` §1.1, groups A–E plus the five names
that plan keeps legacy-only — so that a versioned form Reloaded adds for any of
them (that plan's subject) is designed against the documented client contract
and not against a guess. For each name it answers: whether a versioned
(`mmrpc: "2.0"`) request shape is documented, whether a typed error form is
documented, whether a versioned success shape is documented, whether the
documentation agrees with R43.1A.9, and what the documentation says a
versioned request for a legacy-only name answers. Member names are the API
contract itself and are listed; the documentation's descriptive text is not
reproduced. The semantics of the methods stay where they are bound (chapters
11, 12, 59 for orders; 51, 52 for swaps; 55 §55.4 for `electrum` and `enable`;
28 and 32 for the peer and gossip queries).

**R43.1B.1** *Documentation surface.* The reference documentation indexes
every method in a three-column table — legacy, v2.0 (release), v2.0 (dev) —
and documents each v2.0 method on its own page under a v2.0 path. Of the 44
names, 41 appear in the legacy column and **none** appears in either v2.0
column; `help` has a legacy page but no index row; `stats_swap_status` and
`sim_panic` have neither a row nor a page. No page under a v2.0 path documents
any of the 44 names, and no request example on any of their pages carries an
`mmrpc` member. The documentation therefore defines **no versioned request
shape, no typed error form and no versioned success shape** for any of the
44 names: for every name the documented form is the legacy one of R43.1.1.

**R43.1B.2** *Documented legacy request form.* For 41 of the 42 documented
names the request is the legacy top-level shape: `userpass`, `method` and the
method's parameters **inline** next to them (the table below lists the
members). The one documented exception is `recover_funds_of_swap`, whose single
argument `uuid` is documented **inside a `params` object** of a legacy request
(no `mmrpc` member). That nesting is what the reference implementation reads at
both release rows, and what Reloaded reads (compatibility note below); it is
the only legacy-only name whose documented legacy body already has the member
placement of a versioned request.

**R43.1B.3** *Documented error form.* Where a page documents a failure at all,
it is the legacy error body of chapter 55 R55.1.10 — a top-level `error`
string — with no `error_type`, no `error_data` and no HTTP status stated for
any of the 44 names. Two pages document additional top-level members next to
`error`: `disable_coin` answers `swaps` (list of swap identifiers) when an
active swap blocks the deactivation and `orders` (object with `matching` and
`cancelled` lists) when a matching order does; `import_swaps` reports
per-record failures inside its **success** body (R43.1B.4), not as an error.
Most pages document no failure case.

**R43.1B.4** *Documented success form.* Every documented success body is a
legacy body (no `mmrpc` member, no `id` echo). Two layouts occur, and the
table below records which applies to each name: a `result` wrapper holding the
payload, or a **bare** body whose top-level members are the payload itself
(`my_balance`, `send_raw_transaction`, `order_status`, `metrics`,
`show_priv_key`, `electrum`, `enable`). The documentation's per-method
response tables omit the `result` wrapper for several names whose examples
show it (`update_maker_order`, `buy`, `sell`, `cancel_all_orders`,
`get_trade_fee`, `orders_history_by_filter`, `all_swaps_uuids_by_filter`,
`recover_funds_of_swap`, `set_required_confirmations`,
`set_requires_notarization`, `unban_pubkeys`); the examples are taken as the
documented shape. Numbers are documented in the legacy encodings — a decimal
string with `_rat` and, where present, `_fraction` companion members — never
as the multi-representation object of R43.1A.8.

**R43.1B.5** *What the documentation says a versioned request to a legacy-only
name answers.* Nothing. The documentation describes the versioned protocol
once, on its overview page: a request carries `mmrpc` (exactly `"2.0"`),
`userpass` (optional for public methods), `method`, `params` (optional) and
`id` (optional, echoed when present and non-null); a success carries `mmrpc`,
`result` and `id`; an error carries `mmrpc`, `error`, `error_path`,
`error_trace`, `error_type`, `error_data` and `id`. It states that legacy
methods are being migrated to the versioned protocol and remain backward
compatible, and that only some methods support it. It names no dispatcher
error token — `NoSuchMethod`, `InvalidRequest` and `InvalidMmRpcVersion`
appear nowhere in it as dispatcher answers — and gives no HTTP status for
any envelope. The implementation facts of R43.1A.2–R43.1A.3 (`NoSuchMethod`,
no `error_data`, HTTP 400, `mmrpc` and `id` echoed) are therefore
**undocumented but not contradicted**; the documented envelope members match
those rules.

**R43.1B.6** *Reference states and agreement with R43.1A.9.* The versioned
method table of the reference lineage carries none of the 44 names at any of
the rows `v2.6.0-beta`, `v3.0.0-beta`, `dev (as of 2026-03-27)` and `main
(as of 2026-10-07)` (the same 184-name set at all four rows). R43.1A.9 is
thereby extended from the order methods it names to all 44: the reference
never offered any of them under the versioned envelope. Documentation and
implementation **agree** on this for every name; the documentation's two
undocumented names (`stats_swap_status`, `sim_panic`) and its unindexed page
(`help`) are documentation gaps, not disagreements.

**R43.1B.7** *Consequence for Reloaded.* Because no versioned form is
documented for any of the 44 names, there is no documented versioned contract
Reloaded must keep for them: every versioned form Reloaded adds under plan
`mmrpc-v2-completion` is Reloaded's own design, to be recorded as a deliberate
divergence under plan `rpc-api-reference-wire-parity` §2.2. What the
documentation **does** dictate is the legacy request members (the names,
optionality and value forms of the table below), which that plan's §1.2 moves
into `params` unchanged, and the legacy success payloads it places under
`result`; the table below is the documented inventory for both.

*Documented forms of the 44 names.* "legacy only" means the documented request
is the legacy shape of R43.1.1 with the listed members inline; `?` marks a
member the documentation calls optional; "none" in the error column means the
page documents no failure case; "agrees" refers to R43.1A.9 as extended by
R43.1B.6.

| Method or group | Documented request form (members) | Documented error form | Documented success form | Agrees with R43.1A.9 |
| --- | --- | --- | --- | --- |
| `setprice` | legacy only: `base`, `rel`, `price`, `volume` (ignored when `max` is true), `min_volume`?, `max`?, `cancel_previous`? (default true), `base_confs`?, `base_nota`?, `rel_confs`?, `rel_nota`?, `save_in_history`? (default true); `price`, `volume`, `min_volume` as decimal string, rational or fraction object | legacy `error` string | `result` = order object: `base`, `rel`, `price`, `price_rat`, `max_base_vol`, `max_base_vol_rat`, `min_base_vol`, `min_base_vol_rat`, `created_at`, `updated_at`, `matches`, `started_swaps`, `uuid`, `conf_settings`, `base_orderbook_ticker`, `rel_orderbook_ticker` | agrees |
| `update_maker_order` | legacy only: `uuid`, `new_price`?, `volume_delta`?, `min_volume`?, `max`?, `base_confs`?, `base_nota`?, `rel_confs`?, `rel_nota`? | legacy `error` string | `result` = order object as for `setprice` without the two orderbook-ticker members | agrees |
| `cancel_order` | legacy only: `uuid` | legacy `error` string | `result` string | agrees |
| `cancel_all_orders` | legacy only: `cancel_by` (object; its variants are documented on the shared order-structures page) | none | `result` with `cancelled` and `currently_matching` (lists of order identifiers) | agrees |
| `my_orders` | legacy only: no parameters | none | `result` with `maker_orders` and `taker_orders` (objects keyed by order identifier) | agrees |
| `order_status` | legacy only: `uuid` | legacy `error` string | bare: `type`, `order`, `base_orderbook_ticker`, `rel_orderbook_ticker`; a cancelled order's example adds `cancellation_reason` | agrees |
| `buy`, `sell` | legacy only: `base`, `rel`, `price`, `volume`, `min_volume`?, `match_by`?, `order_type`?, `base_confs`?, `base_nota`?, `rel_confs`?, `rel_nota`?, `save_in_history`? (default true) | legacy `error` string | `result` = taker request object: `action`, `base`, `base_amount`, `base_amount_rat`, `rel`, `rel_amount`, `rel_amount_rat`, `method`, `dest_pub_key`, `sender_pubkey`, `uuid`, `match_by`, `conf_settings`, `base_orderbook_ticker`, `rel_orderbook_ticker` | agrees |
| `orderbook_depth` | legacy only: `pairs` (list of two-string pairs) | none | `result` = list of `{pair, depth: {asks, bids}}`; order of pairs not guaranteed | agrees |
| `max_taker_vol` | legacy only: `coin` | none | `result` = fraction object (`numer`, `denom`) | agrees |
| `min_trading_vol` | legacy only: `coin` | none | `result` with `coin`, `min_trading_vol`, `min_trading_vol_rat`, `min_trading_vol_fraction` | agrees |
| `get_trade_fee` | legacy only: `coin`; the page marks the method deprecated in favor of the versioned `trade_preimage` | none | `result` with `coin`, `amount`, `amount_rat`, `amount_fraction` | agrees |
| `orders_history_by_filter` | legacy only, every filter optional: `order_type`, `initial_action`, `base`, `rel`, `from_price`, `to_price`, `from_volume`, `to_volume`, `from_timestamp`, `to_timestamp`, `was_taker`, `status`, `include_details` (default false) | none | `result` with `orders`, `details` (empty unless `include_details`), `found_records`, `warnings` (items `uuid`, `warning`) | agrees |
| `all_swaps_uuids_by_filter` | legacy only, every filter optional: `my_coin`, `other_coin`, `from_timestamp`, `to_timestamp` (seconds) | none | `result` with `uuids`, `my_coin`, `other_coin`, `from_timestamp`, `to_timestamp`, `records_found` | agrees |
| `import_swaps` | legacy only: `swaps` (list of swap records) | none as an error body | `result` with `imported` (list of identifiers) and `skipped` (object: identifier → failure text) | agrees |
| `recover_funds_of_swap` | legacy only, but `uuid` documented **inside `params`** (R43.1B.2) | legacy `error` string | `result` with `action` (`SpentOtherPayment` or `RefundedMyPayment`), `coin`, `tx_hash`, `tx_hex` | agrees |
| `stats_swap_status` | not documented | — | — | agrees (absent from the versioned table) |
| `my_balance` | legacy only: `coin` | none | bare: `address`, `balance`, `unspendable_balance`, `coin` | agrees |
| `disable_coin` | legacy only: `coin` | legacy `error` string, plus `swaps` or `orders` (`matching`, `cancelled`) in the two blocked cases (R43.1B.3) | `result` with `coin`, `cancelled_orders` | agrees |
| `send_raw_transaction` | legacy only: `coin`, `tx_hex` | none | bare: `tx_hash` | agrees |
| `validateaddress` | legacy only: `coin`, `address` | none | `result` with `is_valid` and, when invalid, `reason` | agrees |
| `convertaddress` | legacy only: `coin`, `from`, `to_address_format` (object; variants on the shared structures page) | none | `result` with `address` | agrees |
| `convert_utxo_address` | legacy only: `address`, `to_coin` | legacy `error` string | `result` string | agrees |
| `kmd_rewards_info` | legacy only: no parameters | none | `result` = list of `{tx_hash, height?, output_index, amount, locktime, accrued_rewards, accrue_start_at?, accrue_stop_at?}`, `accrued_rewards` being either `Accrued` (decimal string) or `NotAccruedReason` (string) | agrees |
| `coins_needed_for_kick_start` | legacy only: no parameters | none | `result` = list of tickers | agrees |
| `set_required_confirmations` | legacy only: `coin`, `confirmations` | none | `result` with `coin`, `confirmations` | agrees |
| `set_requires_notarization` | legacy only: `coin`, `requires_notarization` | none | `result` with `coin`, `requires_notarization` | agrees |
| `version` | legacy only: no parameters | none | `result` string | agrees |
| `stop` | legacy only: no parameters | none | no response body documented | agrees |
| `metrics` | legacy only: no parameters; documented as callable without the password (consistent with R43.2.1) | none | bare: `metrics` = list of `{type, key, labels, value \| count, min, max}` | agrees |
| `get_my_peer_id` | legacy only: no parameters | none | `result` string (peer identifier) | agrees |
| `get_directly_connected_peers` | legacy only: no parameters | none | `result` = object: peer identifier → list of multiaddresses | agrees |
| `get_gossip_mesh`, `get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh` | legacy only: no parameters | none | `result` = the topic/peer map or list the example shows (the response tables name the content only loosely) | agrees |
| `ban_pubkey` | legacy only: `pubkey`, `reason`?, `duration_min`? (minutes; absent = until restart) | legacy `error` string | `result` string | agrees |
| `list_banned_pubkeys` | legacy only: no parameters | none | `result` = object: public key (hex) → `{type, caused_by_swap?, caused_by_event?, reason?}`, `type` being `Manual` or `FailedSwap` | agrees |
| `unban_pubkeys` | legacy only: `unban_by` (object: `type` = `All` or `Few`, `data` = list of public keys when `Few`) | none | `result` with `still_banned`, `unbanned`, `were_not_banned` | agrees |
| `electrum`, `enable` | legacy only, on a combined coin-activation page; members bound by [Chapter 55](55-coin-activation-wire-contract.md) §55.4 | legacy `error` string | bare body (`result`, `address`, `balance`, `coin`, confirmation members; chapter 55) | agrees |
| `help` | legacy only: no parameters; example without `userpass`; page present, no index row | none | free text | agrees |
| `show_priv_key` | legacy only: `coin`; the page marks the method deprecated in favor of the versioned `get_private_keys` | none | bare: `coin`, `priv_key` | agrees |
| `sim_panic` | not documented | — | — | agrees (absent from the versioned table) |

*Finding (documentation versus implementation).* On the question this section
asks — does a versioned form exist — the documentation and the implementation
**disagree nowhere**: 42 names are documented in legacy form only, 2 are not
documented, and the versioned method table carries none of the 44 at any
tabled row (R43.1B.1, R43.1B.6). Three observations fall short of a
disagreement and are recorded for the plan: (1) the documentation is silent on
the dispatcher's answer to a versioned request for a legacy-only name
(R43.1B.5), so clients learn `NoSuchMethod`/HTTP 400 only from behavior;
(2) `recover_funds_of_swap` is documented, implemented and matched by Reloaded
with its argument under `params` (R43.1B.2), unlike every other legacy-only
name; (3) the documentation marks `get_trade_fee` and `show_priv_key`
deprecated in favor of versioned methods that already exist
(`trade_preimage`, `get_private_keys`), which bears on whether a versioned
alias of either is wanted (D43.1B.1). The response-table-versus-example
inconsistencies of R43.1B.4 are internal to the documentation; this round did
not verify the affected success layouts against the implementation beyond
what §43.1A already binds.

> **Compatibility (this repository, 2026-11-10; not corpus-sourced).**
> Reloaded's legacy `recover_funds_of_swap` reads its `uuid` from a `params`
> object, as the documentation shows and the reference does (R43.1B.2).
> **Parity.** Reloaded's legacy table routes `help` and `metrics`; whether
> they sit in its public set is bound by R43.2.1 and was not re-checked here.
> No other Reloaded behavior was checked in this round; the legacy table's
> routing of all 44 names is the premise of plan `mmrpc-v2-completion` §1.1
> and was not re-verified here.

*Input for plan `mmrpc-v2-completion`.* (a) **No** name of the 44 has a
documented versioned contract; every versioned form is Reloaded's own design
(R43.1B.7), to be recorded under plan `rpc-api-reference-wire-parity` §2.2.
(b) The documented legacy request members of the table are the inventory that
plan's §1.2 step 1 carries into `params`; `recover_funds_of_swap` already has
them there. (c) The documented legacy success layouts of the table (wrapped or
bare) are the inventory for that plan's §1.2 step 2. (d) The documentation
deprecates `get_trade_fee` (for `trade_preimage`) and `show_priv_key` (for
`get_private_keys`); the plan's §1.4 already keeps `show_priv_key` legacy-only
on the same ground, and D43.1B.1 asks the same of `get_trade_fee`. (e) Nothing
in the documentation binds the typed error tokens of the new forms; the plan's
§1.2 step 3 (reuse a versioned sibling's token where one exists, else
`Internal`) is unconstrained by it.

**D43.1B.1** *Keep `get_trade_fee` legacy-only?* The documentation marks it
deprecated in favor of the versioned `trade_preimage` (R43.1B.1 table), which
already answers the fee question on the versioned surface with a bound
contract (chapter 08). Options: (i) move `get_trade_fee` from group B to the
plan's §1.4 "legacy-only for good" list, as `show_priv_key` already is, so no
second versioned fee method is added (recommended: it mirrors the ground on
which the plan keeps `show_priv_key` legacy-only); (ii) keep it in group B and
add a versioned form anyway, for clients that call the simpler single-coin
query.

## 43.1C Conventions of the documented versioned surface (reference documentation)

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-15,
> measure 0.02 (attestation in §43.7).**
> This section was written on 2026-11-15 by the KDF Spec Reader (Spec Reader
> round 0.5.0-2) under the two-team procedure of chapter 01 §1.4A, from three
> inputs: the published reference API documentation, requested at
> `https://komodoplatform.com/en/docs/komodo-defi-framework/api/` (fetched
> 2026-11-15; the pages are listed in §43.7; a reference-side input that only
> the two roles of chapter 01 R37 read); the reference lineage at the rows
> `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A (read read-only; no
> untabled state was read); and this repository's own tree. *Host note:* that
> host answered every documentation address requested in this round with a
> permanent redirect to the same path under `https://gleec.com/komodo/docs/`,
> the host §43.1B names, so the two host names lead to one set of pages and
> there are no two copies to compare.

*Scope.* §43.1B established that the documentation defines no versioned form
of the legacy-only names, so every versioned form Reloaded adds for them (plan
`mmrpc-v2-completion`) is Reloaded's own design. This section records the
**conventions** the documented versioned (`mmrpc: "2.0"`) surface follows
where a method does exist in both forms, so that a new form can be shaped like
its documented siblings: how a legacy request relates to the versioned
`params` (R43.1C.1–R43.1C.3), how numbers travel (R43.1C.4–R43.1C.5), how a
success is wrapped and how an object that carries a kind is shaped
(R43.1C.6–R43.1C.7), how errors are shaped and documented (R43.1C.8), and what
the documentation says about the relation between the two forms of one method
(R43.1C.9). It prescribes no design; the open choices are D43.1C.1–D43.1C.5.
Each statement says what it rests on: **[doc]** — the documentation;
**[impl]** — the dispatcher behavior at the two release rows, identical at
both unless stated otherwise; a citation of another rule means that rule's
own inputs. Method names, member names and tag values are the API contract
and are listed; the documentation's descriptive text and its example values
are not reproduced. The semantics of each method stay in the chapter that
owns it.

**R43.1C.1** *Which names have both forms.* Three sets have to be kept apart.
**[doc]** The method index carries **eight** names in both its legacy column
and its v2.0 column, each with a page on either side: `active_swaps`,
`best_orders`, `get_enabled_coins`, `my_recent_swaps`, `my_tx_history`,
`orderbook`, `trade_preimage` and `withdraw`. **[impl]** Both method tables
route those eight and a ninth, `my_swap_status`, whose versioned form the
documentation does not mention (its only page is the legacy one). *This
repository* routes twelve names on both tables: the nine, plus
`max_taker_vol` and `min_trading_vol` — legacy-only in the documentation and
in the reference, so their versioned forms are Reloaded's own — and
`peer_connection_healthcheck` — versioned-only in the documentation and in
the reference, so its legacy form is Reloaded's own. For those three there is
no documented or reference pair to learn from; they are listed in the table
of R43.1C.2 for completeness and described in the compatibility note. Two
further relations are by replacement under another name: the documentation
points `show_priv_key` to `get_private_keys` and `get_trade_fee` to
`trade_preimage` (§43.1B). One index entry is at odds with the
implementation: `consolidate_utxos` is indexed and exemplified as a legacy
method with inline parameters, but **[impl]** only the versioned table routes
it, so the documented legacy request is refused as R43.1A.1b describes.

**R43.1C.2** *Request members, pair by pair.* `?` marks an optional member.
"Relation" classes the step from the legacy request to the versioned
`params`; the answer column states only how the versioned answer differs
from the legacy one.

| Method | Legacy request members | Versioned `params` members | Relation | Versioned answer against legacy answer | Rests on |
| --- | --- | --- | --- | --- | --- |
| `active_swaps` | `include_status`? (boolean, default false) | the same member | unchanged | **[impl]** the legacy body is bare (its page shows one example wrapped in `result` and one bare), the versioned payload is the value of `result`; each status becomes a tagged swap record (R43.1C.7) and both protocol generations are covered, where the legacy map holds untagged first-generation records only; `statuses` is an empty object, not null, when statuses were not asked for | doc; impl |
| `orderbook` | `base`, `rel` | the same two | unchanged | R43.1A.6: `result` wrapper, respelled count and network members, multi-form numbers, tagged address | doc; R43.1A.6 |
| `trade_preimage` | `base`, `rel`, `swap_method` (`buy`, `sell` or `setprice`), `price`, `volume`?, `max`? | the same six | unchanged; **[impl]** the two forms accept one member set and produce one payload | identical payload, legacy number encoding included; both forms place it under `result` | doc; impl |
| `withdraw` | `coin`, `to`, `amount`, `max`?, `fee`? | those five, and `memo`?, `from`?, `ibc_source_channel`?, `broadcast`? (default false) | kept, with optional additions; **[impl]** the legacy form accepts the added members too, although its page omits them; at `v3.0.0-beta` only, both forms also accept an undocumented `expiration_seconds`? | identical members; the legacy body is bare, the versioned one is the value of `result` | doc; impl |
| `my_recent_swaps` | `limit` (default 10), `page_number` (default 1), `from_uuid`?, `my_coin`?, `other_coin`?, `from_timestamp`?, `to_timestamp`? | the same seven, still flat | unchanged; the filter and paging members are **not** regrouped into nested objects | R43.1A.7: the same paging members; each item becomes a tagged swap record | doc; R43.1A.7 |
| `my_swap_status` | `uuid`, inside a `params` object of the legacy request | `uuid` | unchanged (the legacy form already nests its argument, as `recover_funds_of_swap` does, R43.1B.2) | legacy: the untagged record under `result`; versioned: the tagged swap record as the value of `result` | doc (legacy form); impl (versioned form, undocumented) |
| `get_enabled_coins` | none | none; `params` may be omitted | unchanged | legacy: `result` is a list of items with `ticker` and `address`; versioned: `result` is an object whose `coins` list carries `ticker` only | doc; impl |
| `best_orders` | `coin`, `action`, `volume` | `coin`, `action`, `request_by` (object: `type` = `volume` or `number`, and `value`), `exclude_mine`? (default false) | **redesigned**: `volume` gives way to a tagged object that adds a second query mode; one optional member is added | R43.1A.5 | doc; R43.1A.5 |
| `my_tx_history` | `coin`, `limit` (default 10), `max`? (default false), `from_id`?, `page_number`? | `coin`, `limit` (default 10), `paging_options`? (object with the single key `PageNumber` or `FromId`; the first page when absent), `target`? (object tagged by `type`: `iguana` when absent, `account_id` with `account_id` beside it, or `address_id` with `account_id`, `chain` and `address_id` beside it) | **redesigned**: the two paging members merge into one tagged object, `max` is dropped, `target` is added for HD wallets | the versioned answer adds `coin` and `target` and echoes `paging_options` where the legacy answer has `from_id` and `page_number` | doc; impl |
| `max_taker_vol` | `coin`; **[impl]** also `trade_with`?, undocumented | none in the documentation or the reference | no pair | — | doc; impl |
| `min_trading_vol` | `coin` | none in the documentation or the reference | no pair | — | doc; impl |
| `peer_connection_healthcheck` | none in the documentation or the reference | `peer_address` | no pair | — (the versioned `result` is a boolean) | doc; impl |

**R43.1C.3** *The request pattern.* Read off the nine pairs of the reference:

1. *Same question, same members.* Where the versioned form asks what the
   legacy form asks, its `params` holds exactly the legacy members — the same
   names, value types, optionality and defaults (seven pairs of nine). No pair
   renames or regroups a member for the sake of style; the legacy spelling
   survives even where a versioned-only sibling spells the same idea
   differently.
2. *A member changes only for a new capability.* Each of the two redesigns
   replaces a legacy member where the versioned form offers a selection that
   member cannot express (a second query mode; the choice of an HD account or
   address), and the replacement is a tagged object. Every member that is
   added — in the redesigns and in `withdraw` — is optional, with a default
   that reproduces the legacy behavior, so each legacy request has exactly one
   versioned equivalent.
3. *Nested groups belong to versioned-only methods.* **[doc]** List methods
   that exist only on the versioned surface take a flat `limit` with a nested
   `paging_options` and a nested `filter` (the three `lightning::` list
   methods; `z_coin_tx_history` without a filter), or a flat `limit`,
   `page_number` and `max` with a nested `filters` (`get_nft_list`). The
   ported `my_recent_swaps` keeps its flat members. The versioned surface thus
   has no single paging or filter convention, and porting a method did not
   introduce one.
4. *Strictness.* **[impl]** The `params` object of each of the nine versioned
   forms accepts and ignores a member it does not know; only the envelope
   refuses unknown keys (R43.1A.2). A member of the wrong type and a missing
   required member answer `InvalidRequest` (chapter 55 R55.1.7). The
   documentation says nothing about unknown members.


**R43.1C.4** *Numbers in versioned requests.* **[doc]** The versioned pages
type a trading quantity (`price` and `volume` of `trade_preimage`) as a
decimal string or a rational, and a wallet amount (`amount` of `withdraw`, the
`amount` inside its `fee` object) as a decimal string; several request
examples pass a JSON number in those places (`amount` of `withdraw`, `value`
of `best_orders`). No versioned page fetched documents a fraction object in a
request; the legacy order pages do (§43.1B table). **[impl]** A trading
quantity that is a direct member of a request — a price, a volume, an order
amount — is read by one rule on both surfaces and may take any of four JSON
forms: a decimal string, a JSON number, the rational encoding, or a fraction
object with `numer` and `denom`. The by-volume `value` of `best_orders` is
bound by R43.1A.5 as a decimal string or a number. A wallet amount is a
plain decimal, not that multi-form quantity, so the rational and the
fraction are not forms of it. Counts, limits, page numbers, timestamps and
indices are JSON integers. A versioned form therefore neither narrows nor
widens the number forms of its legacy form.

**R43.1C.5** *Numbers in versioned answers.* The documented versioned surface
uses three encodings, one per method:

- (a) the multi-form object of R43.1A.8 (`decimal`, `rational`, `fraction`):
  the entries and totals of `orderbook` and `best_orders`, and the
  versioned-only volume queries `max_maker_vol` (`volume`, `balance`,
  `locked_by_swaps`) and `get_locked_amount` (`locked_amount`);
- (b) the legacy companion members beside the decimal (`<name>`,
  `<name>_rat`, `<name>_fraction`): `trade_preimage`, unchanged from its
  legacy form (the `amount` of each fee object and the optional `volume`),
  and the `dst_amount` object of `1inch_v6_0_classic_swap_quote` in the
  index's v2.0 (dev) column;
- (c) a lone decimal string: `withdraw`, `my_tx_history`, the balances of
  `task::account_balance::status`, the amounts inside swap records, and
  every amount inside an `error_data` record.

**[doc]**, with R43.1A.8 for (a). The pattern: (a) appears where an answer was
designed for the versioned surface around order-book quantities; a form
ported with its payload keeps (b); wallet-side answers carry (c) on both
surfaces (chapter 55 R55.1.9 for activation). The legacy forms of the same
methods use (b) for trading quantities and (c) for wallet amounts; one legacy
answer, `max_taker_vol`, gives a fraction object alone. On the pages fetched
no versioned answer to a request gives an exact form without a decimal
beside it; the events of the two order streams of R43.1C.7, which are not
answers to a request, carry their quantities as a rational alone.

**R43.1C.6** *Success envelope.* **[doc]** A versioned success carries
`mmrpc`, `result` and `id` (R43.1B.5); **[impl]** exactly as chapter 55
R55.1.1 binds. `result` is the whole payload, of whatever JSON type the
method answers: an object (most methods), a list (`get_private_keys`), a
boolean (`peer_connection_healthcheck`) or a string
(`task::account_balance::cancel`). Against the legacy form: a legacy body
that is bare becomes the value of `result` with its members unchanged
(`withdraw`); a legacy body that already wraps its payload in `result` is not
wrapped twice (`trade_preimage`, `my_swap_status`).
`start_simple_market_maker_bot` and `stop_simple_market_maker_bot` answer an
object under `result` that itself has a `result` member; both are
versioned-only methods, not ported wrappers. Three versioned pages lag behind
the implementation and are not conventions: the response table of `orderbook`
still lists the legacy spellings of the count and network members where its
example and R43.1A.6 have the versioned ones; the response table of
`my_tx_history` repeats the legacy `from_id` and `page_number` rows where its
examples have `paging_options`; and the `my_recent_swaps` page shows untagged
items and legacy error bodies where R43.1A.7 binds tagged items and typed
errors. Two versioned success examples lack `mmrpc`; the implementation never
omits it.

**R43.1C.7** *Objects that carry a kind.* The documented versioned surface
shapes such an object in four ways.

| Style | Shape | Where it is used |
| --- | --- | --- |
| (a) adjacent pair named for the object | `<noun>_type` holds the tag and `<noun>_data` the payload; the payload member is absent for a tag that has none | swap records — `swap_type`, `swap_data`, tags `MakerV1`, `TakerV1`, `MakerV2`, `TakerV2` (`active_swaps` and the events of `stream::swap_status::enable`; **[impl]** also `my_recent_swaps` and `my_swap_status`); the events of `stream::order_status::enable` — `order_type`, `order_data`, tags **[impl]** `MakerMatch`, `TakerMatch`, `MakerConnected`, `TakerConnected`; the events of `stream::orderbook::enable` — `order_type`, `order_data`, tags **[impl]** `NewOrUpdatedItem`, `RemovedItem`; the address of an order-book entry — `address_type`, `address_data` (R43.1A.8); the error pair — `error_type`, `error_data` (chapter 55 R55.1.2) |
| (b) `type` beside a payload member named for its content | the tag is `type` (or `status`); the payload sits in one sibling member | the `get_token_info` answer (`type`, `info`) and its request member `protocol` (`type`, `protocol_data`); the `sign_raw_transaction` request (`type`, `tx`); `request_by` of `best_orders` (`type`, `value`); a task status (`status`, `details`) |
| (c) `type` among the object's own members | the tag and the payload members share one object | `fee` of `withdraw` and `fee_details` of its answer; `target` of `my_tx_history` |
| (d) an object with one key | the key is the tag, its value the payload | `paging_options` |

*What a ported form does with a kind.* The legacy swap record states its kind
as a `type` member (`Maker` or `Taker`) among the record's own members. Every
versioned form that answers a swap record lifts the kind out into
`swap_type`, puts the record under `swap_data`, and extends the tag with the
protocol generation. It is the only case in which the versioned surface
re-shaped a kind that a legacy answer carries, and it chose style (a); a
kind-carrying member inside a payload that was ported unchanged keeps its
legacy style (`fee_details` of `withdraw`, style (c)). **[doc]** by example
only — no response table names the pair — **[impl]**, and R43.1A.7.

*Orders.* **[doc]** No documented versioned method answers one of the node's
own maker or taker orders: the index has no versioned counterpart of
`order_status`, `my_orders`, `setprice`, `buy`, `sell` or
`update_maker_order` (R43.1B.1). The legacy `order_status` answers a bare
object — no `result` wrapper — with `type` (`Maker` or `Taker`) and `order`,
and with `cancellation_reason` beside them for an order answered from
history (§43.1B table); in shape it is style (b) without an envelope. Its
closest versioned analogues are, nearest first: (1) `my_swap_status` — like
it a status query by `uuid` for a record of one of several kinds — whose
versioned answer is the style (a) pair as the value of `result` (**[impl]**;
undocumented); (2) the events of the order-status stream — the one documented
versioned object that carries order data under a discriminator, in style (a),
though its tags name stages of a match and not the maker or taker role;
(3) the order-book entry of R43.1A.8 — an order as other nodes see it, with
no kind member at all. The member name `order_type` already has three
meanings across the two surfaces: the event tag of the two streams above; the
fill policy of a taker order (an object whose `type` is `GoodTillCancelled`
or `FillOrKill`); and the role (`Maker` or `Taker`) in the filter and in the
summary rows of `orders_history_by_filter`.

**R43.1C.8** *Errors.* **[doc]** A versioned error carries `mmrpc`, `error`,
`error_path`, `error_trace`, `error_type`, `error_data` and `id` (R43.1B.5),
and the documented examples of a token without payload leave `error_data`
out — as chapter 55 R55.1.2–R55.1.3 bind. The `error_data` kinds documented
are a string, a record and an integer. The documentation states **no HTTP
status** on any page fetched. **[impl]** Each `error_type` has the status
that its method's error set gives it (chapter 55 R55.1.5), and the sets of
the shared names show the spread: a caller-side refusal answers 400 in
`trade_preimage`, `my_swap_status` and `my_tx_history`; an unknown coin is
`NoSuchCoin` with 400 in `trade_preimage` and with 404 in `withdraw`, and
`CoinIsNotActive` with 404 in `my_tx_history`; `Transport` is 500 in
`trade_preimage` and 502 in `withdraw`; storage and internal failures are 500
throughout. The legacy form of each of these methods answers the legacy error
body with HTTP 500 for every one of those cases (R43.1A.1a), so the versioned
form is the first place where a client can tell a refusal from a node
failure.

*How the documentation lists a method's errors* differs from page to page:
(1) a subsection per token, with a table of its `error_data` members and an
example (`trade_preimage`; the protocol overview page, for `withdraw`); (2) a
table of tokens with one-line meanings, mostly followed by examples
(`get_private_keys`, `get_raw_transaction`, `get_token_info`, the
stream-enabling methods); (3) a plain list of tokens (`best_orders`, the
`withdraw` page); (4) nothing (`orderbook`, `active_swaps`,
`get_enabled_coins`, `peer_connection_healthcheck`, and the `my_tx_history`
page — four failure cases of its error set are shown on the overview page
instead). Beside the method pages a shared catalog page lists tokens grouped
into error families, under headings that are not wire members, without
payload kinds, without statuses and without naming the method that raises a
family. It lists the seven dispatcher tokens of R43.1A.3, which refines
R43.1B.5: the dispatcher tokens are documented as a set, though still not as
the answer to any particular request. It also lists families for failures of
methods that are legacy-only: cancelling one order (three tokens — the order
is unknown, the order is being matched at that moment, the order context
is unavailable), cancelling all orders (one catch-all token), building a maker
order and building a taker order (seven tokens each, most of them an amount
or a price below its floor), and the storage of the node's own orders.
**[impl]** None of those tokens reaches a client as an `error_type` of a
legacy-only method at either release row — those methods answer the legacy
error body — so they are not an interface this project must match, and they
are described here rather than named (chapter 01 R11).

*Conventions that hold across the documented sets:* a request that cannot be
decoded answers `InvalidRequest` with a string, which method pages list among
the method's own errors; `NoSuchCoin` carries a record with `coin` wherever
the pages fetched show it; an amount inside a record is a decimal string.
*Conventions that do not hold:* the internal-failure token is `Internal` in
some sets (`orderbook`, R43.1A.6; **[impl]** `active_swaps`,
`get_enabled_coins`, `peer_connection_healthcheck`) and `InternalError` in
others (`trade_preimage`, `withdraw`, `max_maker_vol`,
`get_raw_transaction`); and one token may carry different payload kinds in
different sets (`CoinIsWalletOnly` is a string in `best_orders` and
`orderbook`, R43.1A.5–R43.1A.6, and a record with `coin` in
`trade_preimage`). A token's payload kind and status are therefore facts of
one method's set, not of the token.

**R43.1C.9** *What the documentation says about the two forms of one method.*
**[doc]** In general: the index and the two overview pages between them say
that the legacy methods are being carried over to the versioned protocol
while staying backward compatible, and that only part of them has a
versioned form so far (R43.1B.5). No page fetched names a date or a release
after which a legacy form stops working. Per method: the legacy pages of
`active_swaps`, `get_enabled_coins`, `my_tx_history` and `trade_preimage`
mark the legacy form deprecated and name the versioned form of the same name
as its successor. The reasons given are that the legacy `active_swaps`
reports a swap of the newer protocol generation by identifier only, that the
legacy `my_tx_history` does not serve a coin whose wallet is an HD wallet,
and that the versioned `get_enabled_coins` leaves addresses out because it
is meant for HD wallets, the legacy form remaining the documented way to
read an address when the HD mode is off; `trade_preimage` gets no reason.
The legacy pages of `best_orders`, `orderbook`, `my_recent_swaps`,
`withdraw` and `my_swap_status` carry no such note. The `get_private_keys`
page says it replaces `show_priv_key`. Two statements point the other way:
the versioned `my_tx_history` page sends the caller to the *legacy* method
for every coin but the shielded ones, against the deprecation note on the
legacy page; and the versioned `withdraw` and `sign_raw_transaction` pages
name `send_raw_transaction` as the broadcasting step — a method that has a
legacy form only (§43.1B) — so the documented versioned workflow itself ends
in a legacy request. **[impl]** For `withdraw` and `trade_preimage` the two
forms are one operation: the legacy form accepts the member set of the
versioned form and answers the same payload (bare for `withdraw`, under
`result` for `trade_preimage`), with a typed error rendered as the legacy
error string.

*Not established.* (1) The answer members of versioned-only methods were read
from the documentation alone, except where §43.1A or this section cites the
implementation. (2) Only the two release rows were read; the rows `dev (as of
2026-03-27)` and `main (as of 2026-10-07)` were not read in this round. (3)
The pages not fetched (§43.7) may hold further statements about single
methods; the index was read whole, so no same-name pair is missing from
R43.1C.1. (4) Whether a JSON number in a documented request example is meant
as contract is stated nowhere; the implementation accepts it (R43.1C.4). (5)
The members of the stream events beyond the tag pair were not recorded, and
chapter 10 does not bind them. (6) The statuses of R43.1C.8 are those of the
named sets only; no status was read for a versioned-only method outside
them. (7) Whether the by-volume `value` of `best_orders` also takes the
rational and the fraction form was not established (R43.1C.4).

> **Compatibility (this repository, 2026-11-15; not corpus-sourced).**
> Checked against this repository's tree at the base of this round:
>
> 1. *Tables.* Twelve names are routed on both tables (R43.1C.1), as the
>    method census records. Two of the 44 names of §43.1B, `max_taker_vol`
>    and `min_trading_vol`, are thus already versioned here; the count of
>    §43.1B holds for the reference documentation and tables it describes.
> 2. *Reloaded's two own versioned forms.* The versioned `max_taker_vol` and
>    `min_trading_vol` take the legacy members unchanged inside `params`
>    (`coin`; `trade_with`? on `max_taker_vol`) and answer, under `result`,
>    `coin` with `amount`, `amount_fraction` and `amount_rat` — encoding (b)
>    of R43.1C.5 under one member name for both, which differs from either
>    legacy answer (`max_taker_vol`: a fraction object under `result` with
>    `coin` beside it; `min_trading_vol`: `coin` and `min_trading_vol` with
>    its two companions under `result`). Their typed errors are `NoSuchCoin`
>    (record with `coin`, 400) and `Internal` (string, 500). They follow
>    item 1 of R43.1C.3 for the request and redesign the answer.
> 3. *Reloaded's legacy `peer_connection_healthcheck`.* `peer_address`
>    inline; the boolean under `result`; a failure is the legacy error body.
> 4. *Unknown `params` members.* Reloaded's versioned forms of the twelve
>    names ignore them, as the reference does (R43.1C.3 item 4). Plan
>    `mmrpc-v2-completion` §1.2 step 1 and the test list of its §1.6 state a
>    refusal with `InvalidRequest` as today's behavior of every versioned
>    method; neither implementation behaves so for these names (D43.1C.5).
> 5. *Number forms and tags.* The four request forms of a trading quantity
>    (R43.1C.4) and the tag pairs and tag values of style (a) in R43.1C.7
>    match. **Parity.**
>
> Nothing else of Reloaded's behavior was checked in this round.

**D43.1C.1** *Request shape of a new versioned form (plan
`mmrpc-v2-completion` D2).* Options: (i) the legacy members unchanged inside
`params` — what seven of the nine reference pairs do (R43.1C.3 item 1) and
what Reloaded's two own forms do (compatibility note, item 2); (ii) a request
redesigned per method — which the documented surface does only where the
versioned form gains a capability (R43.1C.3 item 2), so a redesign without a
new capability has no documented precedent; (iii) option (i) as the rule,
with a tagged object replacing a member only where the new form gains a
capability — the documented pattern taken whole. The number forms of
R43.1C.4 are the same under every option, because both surfaces read a
quantity alike.

> *Reloaded status (clean side, 2026-11-15):* resolved by the maintainer's
> decision as option (i): the legacy members unchanged inside `params`; a
> member is redesigned only where a versioned form gains a capability the
> legacy form lacks, and none is planned. Applied in §43.1R.

**D43.1C.2** *Answer shape of a versioned `order_status`.* Options: (i) the
legacy object as the value of `result` — `type`, `order`, and
`cancellation_reason` when present — which applies the "bare body becomes
`result`" step of R43.1C.6 without a redesign, as `withdraw` does, and
already has the form of style (b); (ii) the style (a) pair as the value of
`result` — what the reference does for the nearest analogue,
`my_swap_status`, and what the documentation shows for swap records and
order events. Option (ii) needs three further decisions that the
documentation does not settle: the member names (`order_type` with
`order_data` would give `order_type` a fourth meaning, R43.1C.7), the tag
values (the role alone, or the role with a qualifier, as the swap tags add
the generation), and the place of `cancellation_reason` (beside the pair or
inside the payload). The documented surface holds a precedent for either
option: (ii) is the closer one by kind of method, (i) by least change.
Whichever is taken also bears on the order objects that a versioned
`my_orders`, `setprice`, `buy`, `sell` and `update_maker_order` would
answer, which carry no kind member in their legacy forms.

> *Reloaded status (clean side, 2026-11-15):* resolved by the maintainer's
> decision as option (i): the legacy `order_status` object unchanged as the
> value of `result` (`type`, `order`, and `cancellation_reason` when present).
> Applied in §43.1R.

**D43.1C.3** *Number encoding of quantities in the new answers.* Options:
(i) each legacy encoding as it stands — encoding (b), and the lone fraction
of `max_taker_vol` — the `trade_preimage` precedent; (ii) the multi-form
object (a) for order prices and volumes — the precedent of `orderbook`,
`best_orders` and `max_maker_vol`, and the one encoding of a quantity that
the documentation defines as a shared structure of its own; (iii) the
companion members under one common member name — what Reloaded's two own
forms do. A mixed outcome is itself what the documented surface shows
(R43.1C.5).

> *Reloaded status (clean side, 2026-11-15):* resolved by the maintainer's
> decision as option (i): each legacy number encoding as it stands. Applied in
> §43.1R (R43.1R.3).

**D43.1C.4** *Token spelling, payload kind and status for failures that the
new forms share with existing sets.* The documented sets disagree on the
internal-failure token and give one token two payload kinds (R43.1C.8), and
an unknown coin answers 400 in one set and 404 in another. Options: (i) one
spelling, one payload kind and one status per token across all new forms
(plan `mmrpc-v2-completion` §1.2 step 3 names `Internal`), accepting that
some sibling sets differ; (ii) each new form follows the set of its nearest
versioned sibling, so that a trade-related form would use `InternalError`
and `NoSuchCoin` as a record with status 400, as `trade_preimage` does.

> *Reloaded status (clean side, 2026-11-15):* resolved by the maintainer's
> decision as option (ii): each new form follows the set of its nearest
> documented sibling; for the order and trading forms that is `trade_preimage`
> (`InternalError`, `NoSuchCoin` as a record with `coin`, caller-side refusals
> 400). The statuses of §43.1R stay as built: 400 for every caller-side
> refusal, including the three state conflicts and an unknown order
> identifier, 500 for a failure of the node, 503 for the clock refusal. A
> redesign of the error answers of the whole versioned surface (one spelling
> and one payload kind per failure, HTTP statuses where HTTP has a fitting
> one) is a plan for a later version: `docs/plans/rpc-error-redesign.md`.
>
> *Reloaded status (clean side, 2026-11-15, second note):* the rule is applied
> per group. The wallet and coin forms of plan phase 3 (`my_balance`,
> `disable_coin`, `send_raw_transaction`, `validateaddress`, `convertaddress`,
> `convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`,
> `set_required_confirmations`, `set_requires_notarization`) follow their
> nearest documented sibling, `withdraw`, with the statuses R43.1C.8 records
> for the documented `withdraw`: `NoSuchCoin` as a record with `coin` at 404,
> `Transport` at 502, `InternalError` 500,
> caller-side refusals 400 (the maintainer's decision of 2026-11-15). The
> trading groups keep 400 and 500. The same error name therefore carries
> different statuses in the two groups, as it does on the documented surface
> (R43.1C.8); `docs/plans/rpc-error-redesign.md` is where that is unified.
> Reloaded's own versioned `withdraw` answers a transport failure with 500
> today; the difference to the documented 502 is recorded for a later
> alignment of `withdraw` itself, after a reference-behavior round.

**D43.1C.5** *Unknown members of `params` in the new forms.* The reference
and Reloaded ignore them in every existing form of the shared names
(R43.1C.3 item 4; compatibility note, item 4). Options: (i) ignore them, as
every sibling does — the premise of plan `mmrpc-v2-completion` §1.2 step 1
and the matching test of its §1.6 then need correcting; (ii) refuse them
with `InvalidRequest` in the new forms only — stricter than any reference
sibling and a divergence of its own, under which a client that copies a
legacy body's extra members into `params` fails where a sibling form would
not.

> *Reloaded status (clean side, 2026-11-15):* resolved by the maintainer's
> decision as option (i): the new forms accept and ignore an unknown member
> inside `params`; only the envelope keys stay strict (R43.1A.2). Reason: a
> client built against a later form of a method keeps working against an
> older node, and no sibling form refuses such a member. The rule is kept
> visible for a later reference documentation of the versioned forms. Plan
> `mmrpc-v2-completion` §1.2 step 1 and §1.6 are corrected accordingly; applied
> in §43.1R (R43.1R.2).

> *Reloaded status (clean side, 2026-11-15):* resolved by the maintainer's
> decision as option (i). `get_trade_fee` stays legacy-only for good and is
> listed in plan `mmrpc-v2-completion` §1.4 with this reason; group B of that
> plan is `buy`, `sell`, `orderbook_depth` and `orders_history_by_filter`.

## 43.1R Reloaded versioned forms of the legacy-only order, trading and wallet methods (Reloaded-original, clean side)

*Added 2026-11-15. This section binds a KDF Reloaded extension (D43.1A.1 status
note of 2026-11-15; plan `docs/plans/mmrpc-v2-completion.md`). §43.1B records
that the reference documentation defines no versioned form for these names
(R43.1B.1, R43.1B.7), so the shapes here are Reloaded's own design, built from
the documented legacy members (R43.1B table). It takes no
forbidden-class input: every shape below is this repository's legacy shape
moved into the versioned envelope, and every failure class is one this
repository's handlers produce. The shapes follow the conventions §43.1C
records for the documented versioned surface: the request pattern of R43.1C.3
(item 1 and item 4), the success envelope of R43.1C.6, and for the failures
shared with existing sets the set of the nearest versioned sibling,
`trade_preimage` (D43.1C.4 option (ii): `InternalError`, `NoSuchCoin` as a
record with `coin`, caller-side refusals 400, R43.1C.8). A later alignment is
recorded here as a dated amendment.*

*Amended 2026-11-15 (phase 2):* the trading methods `buy`, `sell`,
`orderbook_depth` and `orders_history_by_filter` are added to every rule and
table below; `orderbook_depth` follows the error set of its nearest documented
sibling, the versioned `orderbook` (R43.1A.6), the other three that of
`trade_preimage`.

*Amended 2026-11-15 (phase 3):* the wallet and coin methods `my_balance`,
`disable_coin`, `send_raw_transaction`, `validateaddress`, `convertaddress`,
`convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`,
`set_required_confirmations` and `set_requires_notarization` are added. They
follow the error set of their nearest documented sibling, the versioned
`withdraw` as R43.1C.8 documents it: `NoSuchCoin` at 404 and `Transport` at
502, where the order and trading forms answer 400 and 500. (Reloaded's own
versioned `withdraw` answers a transport failure with 500 today; that
difference is recorded at D43.1C.4 for a later alignment.) The same error name thus carries
different statuses in the two groups, as it does on the documented surface
(D43.1C.4 second status note).

**R43.1R.1** *Surface.* `setprice`, `update_maker_order`, `cancel_order`,
`cancel_all_orders`, `my_orders`, `order_status`, `buy`, `sell`,
`orderbook_depth`, `orders_history_by_filter`, `my_balance`, `disable_coin`,
`send_raw_transaction`, `validateaddress`, `convertaddress`,
`convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`,
`set_required_confirmations` and `set_requires_notarization` are routed on the
versioned table as well as the legacy table. The legacy forms and their
success bodies are unchanged; a legacy error keeps its text behind one
`file:line]` token of the handler (the inner trace tokens of chained errors
are no longer printed; the text is not part of the contract). Tested against
bodies and texts derived from the handlers of dev d4928e1fb. The
versioned forms run under the envelope, local-only, ban and authentication
checks of every versioned method (R43.1A.2, R43.1A.3); none is public.

**R43.1R.2** *Request (W1).* `params` carries the legacy members unchanged.
An unknown member inside `params` is ignored; `params` that do not decode
answer `InvalidRequest` (string, 400).

| Method | `params` members (type; required unless noted) |
| --- | --- |
| `setprice` | `base` string; `rel` string; `price` number; `max` bool (default `false`); `volume` number (default 0); `min_volume` number (optional); `cancel_previous` bool (default `true`); `base_confs` integer, `base_nota` bool, `rel_confs` integer, `rel_nota` bool (optional); `save_in_history` bool (default `true`); `timeout_in_minutes` integer (optional) |
| `update_maker_order` | `uuid` string; all optional: `new_price` number, `max` bool, `volume_delta` number, `min_volume` number, `base_confs` integer, `base_nota` bool, `rel_confs` integer, `rel_nota` bool |
| `cancel_order` | `uuid` string |
| `cancel_all_orders` | `cancel_by` object `{"type": "All" | "Pair" | "Coin", "data": ...}` as on the legacy surface |
| `my_orders` | none; `params` absent, `null`, `{}` or any object |
| `order_status` | `uuid` string |
| `buy`, `sell` | `base` string; `rel` string; `price` number; `volume` number; optional: `min_volume` number, `timeout` integer, `base_confs` integer, `base_nota` bool, `rel_confs` integer, `rel_nota` bool; `match_by` object (default any), `order_type` object (default good-till-cancelled), `save_in_history` bool (default `true`) |
| `orderbook_depth` | `pairs` list of `[base, rel]` string pairs |
| `orders_history_by_filter` | all optional: `order_type`, `initial_action`, `base`, `rel`, `from_price`, `to_price`, `from_volume`, `to_volume`, `from_timestamp`, `to_timestamp`, `was_taker`, `status`, `include_details` (default `false`) |
| `my_balance`, `disable_coin` | `coin` string |
| `send_raw_transaction` | `coin` string; `tx_hex` string, or `tx_json` object for coins that broadcast a structured transaction (one of the two) |
| `validateaddress` | `coin` string; `address` string |
| `convertaddress` | `coin` string; `from` string; `to_address_format` object |
| `convert_utxo_address` | `address` string; `to_coin` string |
| `kmd_rewards_info`, `coins_needed_for_kick_start` | none; `params` absent, `null`, `{}` or any object |
| `set_required_confirmations` | `coin` string; `confirmations` integer |
| `set_requires_notarization` | `coin` string; `requires_notarization` bool |

Numbers accept the forms the legacy member accepts (decimal string, number,
or the rational forms of chapter 08).

**R43.1R.3** *Success (W2, W7).* HTTP 200, `mmrpc: "2.0"`, `id` echoed,
`result` = the content the legacy body carries under `result`, with the legacy
number encoding: `setprice`, `update_maker_order` the maker-order object;
`cancel_order` the string `"success"`; `cancel_all_orders`
`{"cancelled": [uuid...], "currently_matching": [uuid...]}`; `my_orders`
`{"maker_orders": {...}, "taker_orders": {...}}`; `buy`, `sell` the taker
request object; `orderbook_depth` the list of `{pair, depth: {asks, bids}}`;
`orders_history_by_filter` `{orders, details, found_records, warnings}`;
`my_balance` the legacy bare object unchanged (`coin`, `balance`,
`unspendable_balance`, `address`); `send_raw_transaction` the bare `{tx_hash}`;
`disable_coin` `{coin, cancelled_orders}`; `validateaddress` `{is_valid,
reason?}` (an invalid address is a success with `is_valid: false`);
`convertaddress` `{address}`; `convert_utxo_address` the address string;
`kmd_rewards_info` the list; `coins_needed_for_kick_start` the ticker list;
`set_required_confirmations` `{coin, confirmations}`;
`set_requires_notarization` `{coin, requires_notarization}`;
`order_status` the legacy
bare object unchanged (`type`, `order`, the orderbook-ticker members where the
legacy body has them, and `cancellation_reason` for a history order; R43.1B
table row `order_status`).

**R43.1R.4** *Errors (W3–W6).* The mmrpc error envelope of chapter 55
R55.1.2. A request the client got wrong answers 400, a failure of the node
500; a refusal while the node's own clock is off (`ClockOutOfRange`) 503. A
refusal while a coin's Electrum servers disagree answers 400, as `withdraw`
does (chapter 38). The wallet and coin forms (phase 3) follow the documented
`withdraw` set instead (R43.1C.8): an unknown coin 404, a transport failure
502. One token may cover
several legacy texts; the text is not part of the contract.

| `error_type` | `error_data` | Status | Failure class | Methods |
| --- | --- | --- | --- | --- |
| `NoSuchCoin` | object `{coin}` | 400 | base or rel coin not found or inactive | setprice, buy, sell |
| `CoinIsWalletOnly` | object `{coin}` | 400 | base or rel coin is wallet-only | setprice, buy, sell |
| `CoinIsWalletOnly` | string | 400 | a requested pair has a wallet-only coin (the `orderbook` set, R43.1A.6) | orderbook_depth |
| `TradingNotSupported` | object `{coin}` | 400 | coin cannot trade | setprice, buy, sell |
| `SwapKeyMismatch` | object `{coin, reason}` | 400 | coin cannot be swapped under this login | setprice, update_maker_order, buy, sell |
| `ChainDisagreement` | string | 400 | Electrum servers disagree about the chain (chapter 38) | setprice, buy, sell |
| `BaseEqualRel` | none | 400 | same coin on both sides | setprice, buy, sell |
| `CoinsNotActivated` | object `{base, rel}` | 400 | order's coins not activated | update_maker_order |
| `VolumeTooLow` | object (volume, threshold; coin where known) | 400 | max, min, rel, base or spend-safety volume below floor; amount below minimum | setprice, update_maker_order, buy, sell |
| `PriceTooLow` | object `{price, threshold}` | 400 | price below floor | setprice, update_maker_order, buy, sell |
| `MaxBaseVolBelowMinBaseVol` | object `{min, max}` | 400 | max volume below min volume | setprice, update_maker_order, buy, sell |
| `NotSufficientBalance` | object (coin, available, required; locked where known) | 400 | not enough balance for the swap or its fee preimage | setprice, update_maker_order, buy, sell |
| `NotSufficientBaseCoinBalance` | object | 400 | not enough platform-coin balance | setprice, update_maker_order, buy, sell |
| `IbcRoutingMinBalance` | object `{coin, required, current}` | 400 | IBC routing minimum (feature-gated) | setprice |
| `NewVolumeNotPositive` | object `{volume}` | 400 | new volume not above zero | update_maker_order |
| `NewVolumeBelowReserved` | object `{volume, reserved}` | 400 | new volume below the amount reserved by matches | update_maker_order |
| `UUIDNotFound` | object `{uuid}` | 400 | no order with this uuid | update_maker_order, cancel_order, order_status |
| `OrderBeingMatched` | object `{uuid}` | 400 | order has ongoing matches | update_maker_order, cancel_order |
| `OrderDeleted` | object `{uuid}` | 400 | order removed while the update ran | update_maker_order |
| `OrderStateChanged` | none | 400 | order changed after the checks; retry | update_maker_order |
| `ClockOutOfRange` | string | 503 | the computer's clock is off by more than the limit (chapter 45 R45.6C.8); retry when it is right | setprice, buy, sell |
| `FeeCapBelowBaseFee` | as chapter 35 | 500 | EVM fee estimate below base fee | setprice, update_maker_order, buy, sell |
| `Transport` | string | 500 | node or server unreachable or invalid answer | setprice, update_maker_order, buy, sell |
| `P2PSubscribeError` | string | 500 | order-book relay request or subscription failed (the `orderbook` set) | orderbook_depth |
| `DbError` | string | 500 | order storage failure | setprice, update_maker_order, order_status, buy, sell, orders_history_by_filter |
| `CannotRetrieveOrderMatchContext` | none | 500 | ordermatch context unavailable | cancel_order |
| `Internal` | string | 500 | any other failure of the node (the `orderbook` set) | orderbook_depth |
| `InternalError` | string | 500 | any other failure of the node | all others |
| `NoSuchCoin` | object `{coin}` | 404 | coin not found or not activated (the documented `withdraw` set) | my_balance, disable_coin, send_raw_transaction, validateaddress, convertaddress, convert_utxo_address (target coin), kmd_rewards_info (KMD), set_required_confirmations, set_requires_notarization |
| `Transport` | string | 502 | node or server unreachable, invalid answer, broadcast refused by the network, and a transaction the coin cannot decode (the coin's broadcast gives one text for all three; the documented `withdraw` set) | my_balance, send_raw_transaction, kmd_rewards_info |
| `InvalidParam` | string | 400 | request carries neither `tx_hex` nor `tx_json`, or the wrong type; address conversion refused by the coin | send_raw_transaction, convertaddress |
| `InvalidAddress` | string | 400 | address does not parse | convert_utxo_address |
| `UnsupportedError` | string | 400 | target coin is not a UTXO coin | convert_utxo_address |
| `ActiveSwapsBlockDeactivation` | object `{coin, swaps}` | 400 | running swaps use the coin (the legacy body carries `swaps` beside `error`) | disable_coin |
| `MatchingOrdersBlockDeactivation` | object `{coin, matching, cancelled}` | 400 | orders of the coin are being matched (the legacy body carries `orders` beside `error`) | disable_coin |
| `FeeCapBelowBaseFee` | as chapter 35 | 500 | EVM fee estimate below base fee (balance read) | my_balance |

**T43.1R** *Tests.* Each versioned form: success in the 2.0 envelope with `id`
echoed and `result` equal to the legacy content member by member; an unknown
member inside `params` ignored; a legacy-shaped body with `"mmrpc": "2.0"`
answers `InvalidRequest`; every token above has a wire-form test (token,
`error_data` JSON type, status); the legacy bodies of the six methods are
unchanged against recorded bodies.


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

> **Status note (2026-11-10, clean-side record; the maintainer's decision of 2026-11-10).**
> The legacy method table of this repository routed a test helper,
> `sim_panic`, in every build; on request it panicked the process, which
> contradicted R43.4.1 and the chapter's one-sentence claim; the method
> required the RPC password, so it was never reachable without
> authentication. The helper and its table arm are
> now compiled only with the Cargo feature `sim-panic-rpc`, which no shipped
> build enables. Without the feature a legacy or versioned request for the
> name answers like any unknown method (R43.1A.1a–R43.1A.1c: `NoSuchMethod`
> in the mmrpc 2.0 envelope, HTTP 400). Tests: the `server_tests` cases for
> both feature states in `mm2_main/src/rpc.rs`. This records this
> repository's behavior only and states nothing about the reference.

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
- *Status note added 2026-11-10 -- under R43.4.1 (§43.4).* Reloaded-original,
  clean side: it records this repository's gating of the `sim_panic` test
  helper behind a Cargo feature (the maintainer's decision of 2026-11-10) and takes no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Scoped addition (2026-11-10, Spec Reader round 0.4.0-13) -- §43.1B
  (R43.1B.1--R43.1B.7, the table of documented forms, the finding paragraph,
  the compatibility note, the plan-input note and D43.1B.1); no other file
  was changed.* **KDF Dirty Gate: PASS on 2026-11-10, measure 0.01, no findings** (chapter
  01 R40; the attestation closes this item). *Inputs:* (1) the published
  reference API documentation at
  `https://gleec.com/komodo/docs/komodo-defi-framework/api/`, fetched
  2026-11-10 — the method index page, the v2.0 protocol overview page, the
  site map, and the legacy page of each of the 42 documented names (the
  combined coin-activation page for `electrum` and `enable`) — consulted
  **only** to establish, per name: whether a versioned request shape, a typed
  error form or a versioned success shape is documented; the documented
  request members with their optionality and value forms; the documented
  error-body and success-body layouts; the documented statements on
  authentication and deprecation; and what, if anything, the documentation
  says a versioned request to a legacy-only name answers. Member names cross
  as the API contract; no descriptive sentence, example value or error text of
  the documentation was reproduced. (2) The forbidden corpus at the rows
  `v2.6.0-beta`, `v3.0.0-beta`, `dev (as of 2026-03-27)` and `main (as of
  2026-10-07)` of chapter 02 §2.15A, consulted **only** to establish the set
  of wire method strings the versioned table routes at each row (to extend
  R43.1A.9 to all 44 names) and, at the row `v3.0.0-beta`, at which nesting
  the legacy `recover_funds_of_swap` request reads its argument. What crosses
  is the absence of 44 public wire names from a public method table and one
  dictated member placement; no private identifier, function body, helper
  decomposition, error, log or comment text was reproduced. (3) This
  repository's dev branch as of 2026-11-10, for the compatibility note. No
  row was added to chapter 02 §2.15A: every state read already had one, and
  each row's identifier was re-confirmed present by read-only inspection of
  the historical record's reference metadata before it was named; the
  documentation is not a state of the historical record and takes no row
  (§2.15A R13 tables commits, tags and branch states only), so it is named
  here by URL and fetch date instead.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-10, measure 0.01
  on the R35 basis (dictated-interop and public-surface tokens excluded), no
  findings. Reference states (chapter 02 R13): exactly four states are named —
  `v2.6.0-beta`, `v3.0.0-beta`, `dev (as of 2026-03-27)` and `main (as of
  2026-10-07)` — all rows of §2.15A, each read at its full identifier; the
  documentation URL is a public site and takes no row. Extent: (a) the daemon
  RPC dispatcher's versioned method table and legacy method table at the four
  rows, 8 files compared (2 per row), plus the legacy handler of the one
  params-nested legacy-only method at the two release rows (2 files); (b) the
  published reference API documentation fetched 2026-11-10: the method index
  page, the v2.0 protocol overview page and 13 legacy method pages (the
  params-nested method's page and a sample of 12 further pages whose table
  rows carry prose qualifiers); the remaining legacy pages were not fetched, a
  gap left visible and bounded to pages whose table cells carry member names
  only; (c) the relicensed base's own legacy handler of the same method, for
  the compatibility note. Method: reading of the candidate and of the files at
  the tabled identifiers; mechanical extraction and set comparison of the
  method-name tables; mechanical text probes of the fetched documentation; no
  code blocks in the gated lines.
- *Scoped addition (2026-11-15, Spec Reader round 0.5.0-2) -- §43.1C
  (R43.1C.1--R43.1C.9, the request table and the table of kind-carrying
  shapes, the not-established paragraph, the compatibility note and
  D43.1C.1--D43.1C.5); no other file was changed.* **KDF Dirty Gate: PASS on 2026-11-15, measure 0.02, no findings**
  (chapter 01 R40; the attestation closes this item). *Inputs:* (1) the published reference
  API documentation, requested under
  `https://komodoplatform.com/en/docs/komodo-defi-framework/api/` and served,
  after a permanent redirect of every address, from
  `https://gleec.com/komodo/docs/komodo-defi-framework/api/` (the host of the
  §43.1B item above; one set of pages under two host names), fetched
  2026-11-15 — 84 pages: the method index; the legacy overview and the v2.0
  protocol overview; 28 pages under the legacy path (`active_swaps`,
  `all_swaps_uuids_by_filter`, `best_orders`, `buy`, `cancel_all_orders`, the
  combined coin-activation page, `consolidate_utxos`, `get_enabled_coins`,
  `get_trade_fee`, `max_taker_vol`, `min_trading_vol`, `my_balance`,
  `my_orders`, `my_recent_swaps`, `my_swap_status`, `my_tx_history`,
  `orderbook`, `orders_history_by_filter`, `order_status`,
  `recover_funds_of_swap`, `send_raw_transaction`, `setprice`,
  `show_priv_key`, `trade_preimage`, `update_maker_order`, `withdraw`, the
  batch-request page and the rational-number note); 43 pages under the v2.0
  path (the section indexes for swaps and orders, utilities, wallet and
  streaming; `active_swaps`, `best_orders`, `get_locked_amount`,
  `max_maker_vol`, `my_recent_swaps`, `orderbook`, `recreate_swap_data`,
  `start_simple_market_maker_bot`, `stop_simple_market_maker_bot`,
  `trade_preimage`; `get_enabled_coins`, `peer_connection_healthcheck`,
  `get_public_key`, `get_public_key_hash`, `get_token_info`,
  `get_current_mtp`, `get_shared_db_id`, `get_mnemonic`,
  `add_node_to_version_stat`, `sign_message`, `verify_message`, the status
  page of the Trezor initialization task; `my_tx_history`, `withdraw`,
  `get_raw_transaction`, `sign_raw_transaction`, `z_coin_tx_history`,
  `get_private_keys`, `get_wallet_names`, the task-managed withdraw page and
  the task-managed account-balance page; the stream-enabling pages for order
  status, swap status, order book and balance; the lightning payments page
  and the lightning channels page; the task-managed UTXO activation page;
  `get_nft_list`); one page under the v2.0 (dev) path
  (`1inch_v6_0_classic_swap_quote`); and 9 shared-structure pages (the index,
  orders, swaps, maker events, wallet, activation, enumerations, the error
  catalog and the rational-number note). Of these, the following were
  searched mechanically only — for deprecation and cross-surface statements,
  error-envelope members and HTTP-status statements — and not read for
  content: the legacy pages of `all_swaps_uuids_by_filter`, `buy`,
  `cancel_all_orders`, coin activation, `get_trade_fee`, `my_orders`,
  `orders_history_by_filter`, `recover_funds_of_swap`, `setprice` and
  `update_maker_order` (round 0.4.0-13 covers them); the four v2.0 section
  indexes and the v2.0 pages of `get_public_key`, `get_public_key_hash`,
  `get_current_mtp`, `get_shared_db_id`, `get_mnemonic`,
  `add_node_to_version_stat`, `sign_message`, `verify_message`, the Trezor
  status task, `get_wallet_names`, the balance stream and UTXO activation;
  the shared activation and maker-event pages. The error catalog was read for
  the dispatcher family, the families of the shared names and the order
  families, and by heading only for the rest. **Not fetched**, a gap left
  visible: the other legacy pages; the other v2.0 pages (activation, NFT,
  staking, fee management, the HD task methods, version statistics, the
  hardware-wallet and MetaMask tasks, the remaining stream pages); every
  v2.0 (dev) page but one; and the lightning, NFT and taker-event
  shared-structure pages. The documentation was consulted **only** to
  establish: which names the index gives in both columns; per pair, the
  documented request members with their optionality, defaults and value
  forms on either side; the documented number forms of requests and answers;
  the documented success wrapper and the shapes of objects that carry a
  kind; the documented error members, the ways error tokens are listed, and
  the absence of any HTTP-status statement; and the documented statements
  about deprecation and about the relation of the two forms. Method names,
  member names, tag values and documented enumeration values cross as the
  API contract; no descriptive sentence, example value or error text of the
  documentation was reproduced, and the catalog's family headings and the
  tokens of its legacy-only families are described, not named. (2) The
  forbidden corpus at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02
  §2.15A — identical at both for every fact taken, except the one `withdraw`
  request member that R43.1C.2 records for the later row only — consulted
  **only** to establish: which wire names both method tables route; the
  request members, defaults and unknown-member tolerance of the versioned
  forms of the nine shared names, and of their legacy forms where the
  documentation is silent; whether the legacy and the versioned form of
  `withdraw` and of `trade_preimage` accept one member set and answer one
  payload; the answer wrapper and the tag members of the versioned swap
  queries, and the answer layout of the legacy `active_swaps`,
  `my_swap_status`, `get_enabled_coins`, `max_taker_vol` and `order_status`;
  which JSON forms a trading quantity and a wallet amount may take in a
  request; the tag members and tag values of the order-status and order-book
  stream events; and the `error_type` tokens, payload kinds and HTTP statuses
  of the error sets of `my_swap_status`, `active_swaps`, `get_enabled_coins`,
  `my_tx_history`, `trade_preimage`, `withdraw` and
  `peer_connection_healthcheck`, as far as R43.1C.8 names them. What crosses
  is behavior, public wire method names, dictated request, response and error
  members, tag values and statuses; no private identifier, function body,
  helper decomposition, error, log or comment text was reproduced. (3) This
  repository's tree at the base of this round (the dev branch as of
  2026-11-15 together with the documentation change that carries §43.1B),
  for the compatibility note and for the count of names Reloaded routes on
  both tables. D43.1C.1--D43.1C.5 are Reloaded-original. No row was added to
  chapter 02 §2.15A: both states read already had one, and each row's
  identifier was confirmed, by read-only inspection of the historical
  record's reference metadata, to be the commit its release tag names; the
  documentation is not a state of the historical record and takes no row
  (§2.15A R13 tables commits, tags and branch states only), so it is named
  here by URL and fetch date instead.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-15, measure 0.02
  on the R35 discretionary-expression basis, no findings. Reference states
  (chapter 02 R13): four state names are used — `v2.6.0-beta`, `v3.0.0-beta`,
  `dev (as of 2026-03-27)` and `main (as of 2026-10-07)` — all rows of §2.15A;
  the first two are the states the section rests on, read at their rows' full
  identifiers; the other two are named only as not read; the documentation is
  a public site and takes no row. Extent: (a) at the rows `v2.6.0-beta` and
  `v3.0.0-beta`: the daemon RPC dispatcher's versioned and legacy method
  tables; the request, answer and error-set definitions of the versioned and
  legacy forms of the nine names routed on both tables; the legacy forms of
  the three further names of the R43.1C.2 table and the legacy order-status
  answer; the request reading of multi-form quantities; the mmrpc request and
  answer envelope; the tag pairs and tag values of the swap-status,
  order-status and order-book stream events; the error sets R43.1C.8 names —
  23 files per row, plus a mechanical comparison of the whole tree at each of
  the two rows (893 and 943 text files) and at the rows `dev (as of
  2026-03-27)` and `main (as of 2026-10-07)` (943 text files each); (b) the
  published reference API documentation fetched 2026-11-15 from the host
  §43.1B names: 183 pages (all 84 pages this item lists and 99 further pages
  linked from the method index, the section indexes and the shared-structure
  pages; the shared error catalog compared in full; the requested host's
  permanent redirect to that host confirmed); (c) this repository's own tree
  for the cited rules, the plan name and the §2.15A table. Method: reading of
  the section and of the files at the tabled identifiers; set comparison of
  the two method tables; classification of all 172 distinct backticked tokens;
  word-shingle comparison (3, 4 and 5 words) against each tree and the
  documentation pages; sentence-level ranking against the documentation with
  the closest pairs read; comparison of the documentation's example literals;
  scans for spelled-out source identifiers, catalog headings, commit
  identifiers, source locations and code; the repository's clean-room gate
  tool was not run, as the section embeds no code blocks. The gate judged
  expression, not the accuracy of the [doc] and [impl] statements.
- *Status note added 2026-11-15 -- after D43.1B.1 (§43.1B).* Reloaded-original,
  clean side: it records the maintainer's 2026-11-15 resolution of D43.1B.1
  (`get_trade_fee` stays legacy-only); it takes no forbidden-class input, so
  chapter 01 R38 is not engaged and no gate pass is required.
- *Section 43.1R added 2026-11-15.* Reloaded-original, clean side: it binds
  this repository's versioned forms of six legacy-only order methods from the
  repository's own legacy shapes and failure classes; it takes no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Status note added 2026-11-15 -- after D43.1A.1 (§43.1A).* Reloaded-original,
  clean side: it records the maintainer's 2026-11-15 scheduling of the
  versioned-form extension for v0.5.0 and points to its plan; it takes no
  forbidden-class input, so chapter 01 R38 is not engaged and no gate pass is
  required.
- *Status notes added 2026-11-15 -- after D43.1C.1 to D43.1C.5 (§43.1C).*
  Reloaded-original, clean side: they record the maintainer's 2026-11-15
  resolutions of the five decisions; they take no forbidden-class
  input, so chapter 01 R38 is not engaged and no gate pass is required.
- *Section 43.1R amended 2026-11-15 (phase 2) and D43.1A.1 status note extended.*
  Reloaded-original, clean side: the four trading methods join the tables from
  this repository's own legacy shapes and failure classes; no forbidden-class
  input, chapter 01 R38 not engaged.
- *Section 43.1R amended 2026-11-15 (phase 3), D43.1A.1 status note extended,
  second status note after D43.1C.4.* Reloaded-original, clean side: the ten
  wallet and coin methods join the tables from this repository's own legacy
  shapes and failure classes, and the per-group application of D43.1C.4 is
  recorded as the maintainer decided; no forbidden-class input, chapter 01
  R38 not engaged.
