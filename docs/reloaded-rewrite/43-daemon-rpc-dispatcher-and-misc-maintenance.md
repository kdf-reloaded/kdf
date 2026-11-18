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
>
> *Phases 4 and 5 (swaps; daemon and network; this repository, 2026-11, one
> pull request):* `all_swaps_uuids_by_filter`, `import_swaps`, `version`,
> `stop`, `metrics`, `get_my_peer_id`, `get_directly_connected_peers`,
> `get_gossip_mesh`, `get_gossip_peer_topics`, `get_gossip_topic_peers`,
> `get_relay_mesh`, `ban_pubkey`, `list_banned_pubkeys` and `unban_pubkeys`
> are routed on both surfaces. `recover_funds_of_swap` stays legacy-only until
> the swap architecture decisions; `stats_swap_status` and `get_trade_fee`
> stay legacy-only for good. With this the extension of the plan is complete:
> 40 methods. Per-method shapes and error tables: §43.1R (dated amendment).

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
>
> *Reloaded status (clean side, 2026-11-16, third note):* §43.1D established
> that the documentation states no status for `withdraw` and that 404 and 502
> are what the reference answers (R43.1D.1, R43.1D.2); the two notes above
> and the texts of 2026-11-15 that say "documented statuses" are to be read
> as "the statuses the reference answers for `withdraw`". By the maintainer's
> decision of 2026-11-16, Reloaded's versioned `withdraw`, `withdraw_nft` and
> the start request of `task::withdraw::init` now answer `Transport` with
> 502, as the reference does; the sentence above on the 500 is historical.
>
> *Reloaded status (clean side, 2026-11-16, sibling sets of the last two
> groups):* the swap forms `all_swaps_uuids_by_filter` and `import_swaps`
> follow `my_recent_swaps` (R43.1A.7); the peer and gossip queries
> `get_my_peer_id`, `get_directly_connected_peers`, `get_gossip_mesh`,
> `get_gossip_peer_topics`, `get_gossip_topic_peers` and `get_relay_mesh`
> follow `peer_connection_healthcheck` (`Internal`, string, 500); `version`,
> `stop` and `metrics` have no failure class of their own beyond `Internal`
> where a string source exists. The ban methods `ban_pubkey`,
> `list_banned_pubkeys` and `unban_pubkeys` have no documented versioned
> sibling and take the general convention of R43.1C.8 as Reloaded's own
> choice: a request the client got wrong `InvalidParam` (string, 400), a
> failure of the node `Internal` (string, 500); a malformed public key never
> reaches the method and answers the dispatcher's `InvalidRequest`. Decided
> on 2026-11-16 under the maintainer's rule of 2026-11-15.

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

## 43.1D Statuses and payloads of the versioned `withdraw` errors (reference behavior)

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-16,
> measure 0.02 (attestation in §43.7).**
> This section was written on 2026-11-16 by the KDF Spec Reader (Spec Reader
> round 0.5.0-9) under the two-team procedure of chapter 01 §1.4A, from
> three inputs: the reference lineage at the rows `v2.6.0-beta` and
> `v3.0.0-beta` of chapter 02 §2.15A (read read-only; no untabled state was
> read); two pages of the published reference API documentation, fetched
> again on 2026-11-16 from the host §43.1C names (the versioned `withdraw`
> page and the v2.0 protocol overview page); and this repository's own tree.
> It edits no sentence of §43.1A -- §43.1C. It **completes R43.1C.8**, and
> where a statement here is more exact than the sentence it completes, the
> statement here governs. Numbering: R43.1D.1 -- R43.1D.4 and T43.1D.1 --
> T43.1D.6; no decision item is raised.

*Scope.* R43.1C.8 names two statuses of the versioned `withdraw` error set (an
unknown coin 404, a transport failure 502). This section states the whole
set as the reference answers it: each `error_type` token, the JSON kind of
its `error_data`, and its HTTP status. It records tokens, payload kinds and
statuses only; which condition of which coin family leads to a token stays
with the chapter that owns the family.

**R43.1D.1** *What the statuses rest on (completes R43.1C.8).* The two pages
fetched for this section state **no HTTP status** for any error, as R43.1C.8
already records for the pages of its round; the statuses 404 and 502 that
R43.1C.8 gives are therefore facts of the reference implementation (the
statements it marks **[impl]**), not of the documentation, and there is no
documented status from which an answered one could differ. The reference
answers every typed error of the versioned `withdraw` with the status of
R43.1D.2 as the status of the HTTP response, and with the error envelope of
R43.1C.8 as its body. A token without payload leaves `error_data` out
(chapter 55 R55.1.2 -- R55.1.3). A request whose `params` cannot be decoded
never reaches the method and answers the dispatcher's `InvalidRequest`
(string, HTTP 400, R43.1A.3).

**R43.1D.2** *The error set.* At the row `v3.0.0-beta` the set has the 32
tokens below. "Record" means a JSON object with the members named; an amount
is a decimal string, as R43.1C.8 says.

| `error_type` | `error_data` | HTTP status |
| --- | --- | --- |
| `NoSuchCoin` | record: `coin` (string) | 404 |
| `Timeout` | record: `secs`, `nanos` (integers) | 408 |
| `HwError` | string: the token of one hardware-wallet condition | 410 |
| `Transport` | string | 502 |
| `InternalError` | string | 500 |
| `DbError` | string | 500 |
| `ProtocolNotSupported` | string | 500 |
| `NotSufficientBalance` | record: `coin` (string), `available`, `required` (decimal strings) | 400 |
| `NotSufficientPlatformBalanceForFee` | record: `coin` (string), `available`, `required` (decimal strings) | 400 |
| `ZeroBalanceToWithdrawMax` | none | 400 |
| `AmountTooLow` | record: `amount`, `threshold` (decimal strings) | 400 |
| `InvalidAddress` | string | 400 |
| `InvalidFeePolicy` | string | 400 |
| `InvalidFee` | record: `reason` (string), and `details` (any JSON value) only when there are any | 400 |
| `InvalidMemo` | string | 400 |
| `FromAddressNotFound` | none | 400 |
| `UnexpectedFromAddress` | string | 400 |
| `UnknownAccount` | record: `account_id` (integer) | 400 |
| `UnexpectedUserAction` | record: `expected` (string) | 400 |
| `UnsupportedError` | string | 400 |
| `CoinDoesntSupportInitWithdraw` | record: `coin` (string) | 400 |
| `CoinDoesntSupportNftWithdraw` | record: `coin` (string) | 400 |
| `ContractTypeDoesntSupportNftWithdrawing` | string | 400 |
| `ActionNotAllowed` | string | 400 |
| `GetNftInfoError` | record: a nested error with its own `error_type` and, where that token has a payload, `error_data` | 400 |
| `NotEnoughNftsAmount` | record: `token_address`, `token_id` (strings), `available`, `required` (each an array of integers, the number in 32-bit parts, lowest part first) | 400 |
| `MyAddressNotNftOwner` | record: `my_address`, `token_owner` (strings) | 400 |
| `NoChainIdSet` | record: `coin` (string) | 400 |
| `SigningError` | string | 400 |
| `TxTypeNotSupported` | none | 400 |
| `IBCError` | record with exactly one key, which names the case, and a record as its value: `IBCChannelCouldNotBeFound` (`address_prefix`, string), `IBCChannelNotHealthy` (`channel_id`, integer), `IBCChannelMissingOnNode` (`channel_id`, integer), `Transport` (`reason`, string) or `InternalError` (`reason`, string) | 400 |
| `BroadcastExpected` (browser builds only) | string | 400 |

Read by status: 404 for `NoSuchCoin` alone; 408 for `Timeout` alone; 410 for
`HwError` alone; 502 for `Transport` alone; 500 for `InternalError`, `DbError`
and `ProtocolNotSupported`; 400 for every other token. `NoSuchCoin` answers
a ticker that is not active in the node, whatever the reason: an unknown
ticker, a coin that was never activated, and a platform coin in the passive
state of R43.1E.6 answer alike.

**R43.1D.3** *One set, three methods.* The set is shared by the versioned
`withdraw`, by `withdraw_nft` and by the task-managed withdraw
(`task::withdraw::init`: a refusal of the start request itself answers with
the statuses of the table, while a failure of the running task is carried
inside the status answer and has no HTTP status of its own). Four tokens
concern an NFT transfer only (`CoinDoesntSupportNftWithdraw`,
`GetNftInfoError`, `NotEnoughNftsAmount`, `MyAddressNotNftOwner`) and one the
task-managed form only (`CoinDoesntSupportInitWithdraw`). Four further tokens
are part of the set but no case that answers them was found at either row:
`ActionNotAllowed`, `ContractTypeDoesntSupportNftWithdrawing`, `DbError` and
`NoChainIdSet`. A client therefore has to accept every token of the table
from `withdraw`, and may expect the general ones in practice.

**R43.1D.4** *Reference states, and the documentation.* The two rows agree on
the whole table except one line: where `v3.0.0-beta` has
`ProtocolNotSupported` with a string, `v2.6.0-beta` has
`NftProtocolNotSupported` with no payload; the status is 500 at both. The
statuses of all other tokens, and their payload kinds, are the same at both
rows. Against the documentation: the overview page describes seven tokens of
`withdraw` (`NotSufficientBalance`, `AmountTooLow`, `InvalidAddress`,
`InvalidFeePolicy`, `NoSuchCoin`, `Transport`, `InternalError`), and their
documented payload kinds agree with the table; the other tokens of the table
are not documented on the two pages. The error example that the overview
page shows for `withdraw` carries a token that is in the set at neither row;
the set's token for an amount below the floor is `AmountTooLow`, with
`amount` and `threshold`.

> **Compatibility (this repository on dev, 2026-11-16; its own code, not the
> reference).** Reloaded's versioned `withdraw` has an error
> set of its own. The statuses agree for every token both sets have, except
> one: Reloaded answers `Transport` with 500, the reference with 502 (the
> difference the second status note under D43.1C.4 already records).
> `NoSuchCoin` is 404 and `Timeout` 408 in both. Reloaded has no `HwError`:
> it answers hardware-wallet conditions with tokens of its own, three of them
> with 410 and one with 500. Reloaded lacks
> `NotSufficientPlatformBalanceForFee`, `InvalidFee`, `InvalidMemo`,
> `SigningError`, `TxTypeNotSupported`, `ProtocolNotSupported`,
> `NoChainIdSet`, `IBCError`, `BroadcastExpected`, `DbError`,
> `ActionNotAllowed` and the NFT tokens, and it has tokens the reference
> lacks. Nothing else of Reloaded's behavior was checked for this section.

*Not established.* (1) Which backend failure of which coin family the
reference answers as `Transport` and which as `InternalError` was not traced
request by request. (2) The tokens a nested `GetNftInfoError` and the
`HwError` string can take were not listed. (3) Only the two release rows and
the two documentation pages named above were read. (4) Nothing was measured
against a running reference binary.

*Acceptance rows.* Each row is a versioned request (`mmrpc: "2.0"`, method
`withdraw`) against a node with one coin active on a mock backend. *Parity*
is the reference behavior of R43.1D.2.

T43.1D.1 *Unknown coin.* A request that names a ticker the node has not
activated answers HTTP 404 with `error_type` `NoSuchCoin` and `error_data` a
record whose `coin` is the requested ticker.

T43.1D.2 *Transport failure.* With a mock backend that, after activation,
fails the requests the withdraw needs in a way the coin family's chapter
classes as a transport failure, a request that is otherwise valid answers
`error_type` `Transport` with a string as `error_data`. Parity: HTTP 502.

T43.1D.3 *Insufficient balance.* A request for more than the wallet holds
answers HTTP 400 with `NotSufficientBalance` and a record of `coin`,
`available` and `required`, the two amounts as decimal strings.

T43.1D.4 *A token without payload.* A request with `max` true from a wallet
that holds nothing, for a coin family that answers
`ZeroBalanceToWithdrawMax`, answers HTTP 400 and a body without an
`error_data` member.

T43.1D.5 *Invalid destination.* A request whose `to` is not an address of
the coin answers HTTP 400 with `InvalidAddress` and a string.

T43.1D.6 *Undecodable `params`.* A request whose `amount` has a JSON kind the
method does not accept answers the dispatcher's `InvalidRequest`, HTTP 400,
and no token of R43.1D.2 (R43.1D.1).

## 43.1E The legacy `disable_coin` method (reference behavior)

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-16,
> measure 0.02 (attestation in §43.7).**
> This section was written on 2026-11-16 by the KDF Spec Reader (Spec Reader
> round 0.5.0-9) under the two-team procedure of chapter 01 §1.4A, from
> three inputs: the reference lineage at the rows `v2.6.0-beta` and
> `v3.0.0-beta` of chapter 02 §2.15A (read read-only; no untabled state was
> read), **which agree on everything in this section**; the legacy
> `disable_coin` page of the published reference API documentation, fetched
> again on 2026-11-16 from the host §43.1C names; and this repository's own
> tree. It edits no sentence of §43.1A -- §43.1C or of §43.1R. It
> **completes R43.1B.3 and the `disable_coin` line of the table of
> documented forms in §43.1B**, and it supplies what chapter 55 D55.6 and
> chapter 38 R38.6.4Q.10 item 4 leave to this chapter; where a statement
> here is more exact than the sentence it completes, the statement here
> governs. Numbering: R43.1E.1 -- R43.1E.8, D43.1E.1 -- D43.1E.2 and
> T43.1E.1 -- T43.1E.9.

*Scope.* A wire contract of the legacy `disable_coin` method: its request,
its answers with their HTTP statuses, and what a successful call does to the
node's orders, to the coin and to the tokens of a platform coin. Statements
marked **[doc]** rest on the documentation page, all others on the reference
implementation.

**R43.1E.1** *Surface and request.* `disable_coin` is routed on the legacy
table only, at both rows; no versioned form exists at either, and a versioned
request for the name answers `NoSuchMethod` (R43.1A.3). The request carries
these members beside the envelope's own:

| Member | Kind | Meaning |
| --- | --- | --- |
| `coin` | string, required | the ticker to disable |
| `force_disable` | boolean, optional | absent, false or any value that is not a boolean: not forced. True: forced (R43.1E.5) |

**[doc]** The page documents `coin` alone; `force_disable` is not documented.

**R43.1E.2** *Success.* HTTP 200 and a `result` object with exactly three
members:

| Member | Kind | Meaning |
| --- | --- | --- |
| `coin` | string | the requested ticker |
| `cancelled_orders` | array of strings | the identifiers of the orders this call cancelled (R43.1E.4); empty when there were none |
| `passivized` | boolean | true when the coin was made passive instead of being removed (R43.1E.5), false when it was removed |

**[doc]** The page documents `coin` and `cancelled_orders`; `passivized` is
not documented.

**R43.1E.3** *Refusals.* Every refusal answers **HTTP 500**. Two body shapes
occur. The *plain* shape is the legacy error body of chapter 55 R55.1.10: an
`error` string and nothing else. The *extended* shape has three top-level
members, all always present: `error` (string), `orders` (object with the two
arrays `matching` and `cancelled`) and `active_swaps` (array). Identifiers
are strings in the usual hyphenated form.

| Refusal | When it applies | Body |
| --- | --- | --- |
| No coin named | the request has no `coin` member, or it is not a string | plain |
| Coin not active | the ticker is not registered; or it is a passive platform coin (R43.1E.6) and the request is not forced | extended; `orders.matching`, `orders.cancelled` and `active_swaps` are all empty |
| Coin in use | at least one running swap of this node has the ticker as either of its coins (swaps of both protocol generations count), or at least one of this node's own orders, maker or taker, names the ticker on either side of its pair and is being matched at that moment | extended; `active_swaps` lists the identifiers of those swaps, `orders.matching` those of the orders being matched, either of them empty when it does not apply; `orders.cancelled` is empty |
| Node failure | the node's registry, swap state or order state cannot be read, or the cancellation step fails | plain, or extended with empty lists |

The coin-in-use refusal is **one** refusal for both causes, with one `error`
text; a client tells the cause from which list is not empty. It is decided
before anything is changed: no order is cancelled, the coin stays active, and
`orders.cancelled` is therefore empty in every refusal the reference answers.

**R43.1E.4** *What a successful call does to the orders.* After the checks of
R43.1E.3 have passed, the node cancels every one of its own orders, maker and
taker, that names the ticker on either side of its pair and can be
cancelled, with the same effect on each order as a `cancel_all_orders`
request by coin has.
Their identifiers are the `cancelled_orders` of the answer. An order that
began to be matched between the check and the cancellation is left in place
and is not reported in the answer. The cancellation happens in every
successful call, whether the coin is then removed or made passive. Only
orders that name the **requested** ticker are considered: orders of a
platform coin's tokens are untouched (R43.1E.5).

**R43.1E.5** *What a successful call does to the coin and to its tokens.*

1. *Dependent tokens.* The node keeps, for each platform coin, the set of
   tokens that depend on it. A token enters the set when it is activated by a
   token activation method or as part of a platform-with-tokens activation
   (chapter 55) while its platform is registered; a layer-two coin enters the
   set of its platform in the same way. A token activated by the legacy
   `enable` or `electrum` method does **not** enter it (chapter 38
   R38.6.4Q.10 item 2 records this for QRC20).
2. *A token, or a coin without dependent tokens.* The coin is removed from
   the node's registry and its background work is ended; a token is also
   taken out of its platform's set. `passivized` is false.
3. *A platform coin with at least one dependent token, not forced.* Nothing
   is removed. The platform coin becomes **passive** (R43.1E.6), its tokens
   stay active, and `passivized` is true.
4. *A platform coin with dependent tokens, forced.* The platform coin and
   every dependent token are removed from the registry and their background
   work is ended. `passivized` is false. The checks of R43.1E.3 and the
   cancellation of R43.1E.4 were made for the platform's ticker only: a
   running swap or an order of a dependent token neither refuses the call nor
   is cancelled by it (D43.1E.2).
5. *Forcing a coin that is not a platform with tokens* changes nothing: the
   call behaves as item 2.

**R43.1E.6** *The passive state.* Only a platform coin can be passive.

1. Every method that needs an active coin treats a passive coin as not
   active: the versioned `withdraw`, for example, answers `NoSuchCoin`
   (R43.1D.2).
2. `get_enabled_coins`, in both forms, still lists it.
3. The coin's own instance and its background work go on, and its dependent
   tokens stay active.
4. A further `disable_coin` for it answers the coin-not-active refusal unless
   forced; forced, it is removed as R43.1E.5 item 4 or item 2 describes.
5. Disabling its last dependent token neither removes it nor makes it active
   again: it stays registered and passive.
6. A platform-with-tokens activation of the ticker makes it active again
   (chapter 55 D55.6); the legacy `enable` and `electrum` methods refuse the
   ticker as already activated, with the legacy error body.

**R43.1E.7** *Where the reference differs from its documentation.*

| Subject | **[doc]** | Reference |
| --- | --- | --- |
| Request | `coin` | `coin`, `force_disable` |
| Success | `coin`, `cancelled_orders` | those two and `passivized` |
| Member that names blocking swaps | `swaps`, present only in that error | `active_swaps`, present in every extended refusal; no `swaps` member in any answer |
| Member that names blocking orders | `orders` with `matching` and `cancelled`, present only in that error | `orders` with both arrays, present in every extended refusal |
| Orders cancelled when the call is refused for a matching order | other orders may already have been cancelled and are listed in `orders.cancelled` | none is cancelled; `orders.cancelled` is always empty |
| A coin that is not active | an error; no members named | the extended shape with empty lists |
| HTTP status of a refusal | not stated | 500 |

R43.1B.3 records the documented members correctly; the table adds what the
reference answers in their place.

**R43.1E.8** *Reference states.* The method, the registry operations it uses
and the checks of R43.1E.3 are the same at `v2.6.0-beta` and `v3.0.0-beta`.
Exceptions that a coin family makes to this section are that family's
chapter's to state (chapter 40 §40.6B.5 records them for Solana).

> **Compatibility (this repository on dev, 2026-11-16; its own code, not the
> reference).**
>
> 1. *Request.* Reloaded reads `coin` only; `force_disable` is ignored.
> 2. *No dependent-token set, no passive state.* Reloaded removes the
>    requested ticker from its registry in every successful call and leaves
>    the tokens of a platform coin registered; `passivized` is not answered.
>    Chapter 55 D55.6 is open on exactly this.
> 3. *Refusals.* Reloaded follows the documentation, not the reference: a
>    running swap answers `error` with `swaps`; an order being matched answers
>    `error` with `orders`, **after** the cancellable orders of the coin have
>    been cancelled, so `orders.cancelled` can be non-empty; a coin that is
>    not active answers the plain shape. No Reloaded answer has
>    `active_swaps`. All are HTTP 500, as in the reference.
> 4. *Background work.* Reloaded's removal ends none of the coin's background
>    work at once (the note under chapter 38 R38.6.4Q.10).
> 5. *Versioned form.* Reloaded's versioned `disable_coin` is §43.1R's and has
>    no reference counterpart (R43.1E.1).
>

> *Reloaded status (clean side, 2026-11-18; Reloaded-original, its own code):*
> the first part of the passive-platform work of chapter 55 D55.6, with no
> change of any answer.
>
> 1. Reloaded now keeps, under the registry's lock, a dependent set per
>    platform coin, with the joining rule of R43.1E.5 item 1: the tokens of a
>    platform-with-tokens activation, and the token and layer-two coins of the
>    token and layer-two activation methods, join their platform's set; a token
>    registered by the legacy `enable` or `electrum` does not.
> 2. A disabled token is taken out of its platform's set and out of the
>    platform's own records of its tokens (EVM and TRON, BCH and SLP,
>    Tendermint, Solana). A second removal, or a removal while the platform is
>    not registered, changes nothing. Single-token Solana activation now records
>    the token on its platform, as the with-tokens activation does.
> 3. A platform with dependents is still removed by every successful call, its
>    dependent set is dropped with it, and its tokens stay registered: item 2 of
>    the compatibility note above is unchanged until the passive state is
>    built.
>

**D43.1E.1** *(Open, 2026-11-16.)* **Which members a refused `disable_coin`
carries.** A client written from the documentation reads `swaps`; a client
written against the reference reads `active_swaps` and expects `orders` and
`active_swaps` in every extended refusal. Either choice alone leaves one of
the two clients without the list it looks for. Options: (a) the reference
shape (R43.1E.3); (b) the documented shape, which Reloaded has today; (c)
both: the extended shape of R43.1E.3 with `swaps` added as a second member
that holds the same list as `active_swaps`. The same question arises for
`passivized` in the success answer, which costs nothing to add once D55.6 is
decided, and for whether orders are cancelled before a matching-order
refusal: the documented behavior (cancel first, report in
`orders.cancelled`) and the reference behavior (cancel nothing) differ, and a
client cannot tell from the body which one it got unless `orders.cancelled`
is filled. *Recommendation:* (c), and keep the documented cancel-first
behavior, which the body makes visible.

> *Reloaded status (clean side, 2026-11-16):* resolved by the maintainer's
> decision as option (c). A refused legacy `disable_coin` of the coin-in-use
> and coin-not-active kinds answers the extended shape of R43.1E.3 (`error`,
> `orders` with `matching` and `cancelled`, `active_swaps`) with `swaps` added
> as a fourth member holding the same list as `active_swaps`; the "no coin
> named" refusal and a node failure without lists stay plain; HTTP 500
> throughout. The documented cancel-first behavior is kept and is visible in
> `orders.cancelled`. The success answer is unchanged (`passivized` waits for
> chapter 55 D55.6); `force_disable` is still not read. The versioned
> `disable_coin` of §43.1R is unchanged.

> *Reloaded status (clean side, 2026-11-18, second note):* with the passive
> state decided (chapter 55 D55.6), the maintainer decided on 2026-11-18 that
> the legacy `disable_coin` reads `force_disable` and answers `passivized` as
> R43.1E.5 and R43.1E.6 describe. The versioned `disable_coin` of §43.1R gets
> neither: it keeps its request and answer unchanged.

**D43.1E.2** *(Open, 2026-11-16.)* **A forced removal does not look at the
dependent tokens.** In the reference the swap check, the matching-order check
and the order cancellation of a forced call cover the platform's ticker only,
while the call removes the dependent tokens as well (R43.1E.5 item 4). It
arises for Reloaded only if D55.6 brings the dependent-token set and the
forced removal. Options: (a) reproduce the reference; (b) apply R43.1E.3 and
R43.1E.4 to the platform's ticker and to every dependent token before
anything is removed, and refuse the whole call if any of them is in use.
*Recommendation:* (b).

> *Reloaded status (clean side, 2026-11-18):* resolved by the maintainer's
> decision of 2026-11-18 as option (b). A forced `disable_coin` applies
> R43.1E.3 and R43.1E.4 to the platform's ticker and to every dependent token
> before anything is removed, and refuses the whole call if any of them is in
> use.

*Status note (2026-11-18).* The maintainer decided that Reloaded builds the
passive state of R43.1E.5 item 3 and R43.1E.6 in 0.5.0 (chapter 55 D55.6).
What this section leaves open for that build -- re-activation, requested
tokens that are already registered, token and layer-two activations while a
platform is passive, the NFT chain, the standard UTXO platform with a
Lightning coin, and what the methods that need an active coin answer -- is
§43.1F (passed the KDF Dirty Gate on 2026-11-18). No sentence of this section is
changed by it.

*Not established.* (1) What ending a coin's background work covers was not
traced family by family; chapter 38 R38.6.4Q.10 states it for QRC20 and
chapter 40 §40.6B.5 for Solana. (2) Whether a passive platform coin still
appears in answers other than `get_enabled_coins` was not examined method by
method; R43.1E.6 item 1 states the rule those methods share. (3) The `error`
texts are not recorded; they are not part of the contract. (4) Nothing was
measured against a running reference binary.

*Acceptance rows.* Each row is a legacy request (no `mmrpc` member) against a
node with mock backends. *Parity* is the reference behavior bound above;
where a D-item would change the expected result, the row says so.

T43.1E.1 *Success without orders.* With a coin active that has no orders and
no swaps, the call answers HTTP 200 and `result` with `coin` equal to the
ticker and an empty `cancelled_orders`; the coin is no longer listed by
`get_enabled_coins`. Parity: `passivized` is present and false.

T43.1E.2 *Orders are cancelled.* With two maker orders that have the coin on
either side and one that has not, the call answers the two identifiers in
`cancelled_orders`, `my_orders` afterwards shows the third order only, and
the coin is removed.

T43.1E.3 *Coin not active.* A call for a ticker that was never activated
answers HTTP 500. Parity: `error`, `orders` with two empty arrays and an
empty `active_swaps`.

T43.1E.4 *No coin named.* A call without `coin` answers HTTP 500 and a body
whose only member is `error`.

T43.1E.5 *Running swap.* With a swap in progress that uses the coin, the call
answers HTTP 500 and names the swap's identifier; the coin stays active and
no order is cancelled. Parity: the identifier is in `active_swaps`, `orders`
is present with two empty arrays, and there is no `swaps` member. Under
D43.1E.1 (c) the identifier is in `active_swaps` and in `swaps`.

T43.1E.6 *Order being matched.* With one order of the coin held in the
matching state and a second, idle order of the coin, the call answers HTTP
500 with the first identifier in `orders.matching` and the coin stays
active. Parity: the second order is not cancelled and `orders.cancelled` is
empty. Under the documented behavior the second order is cancelled and
listed in `orders.cancelled` (D43.1E.1).

T43.1E.7 *Platform with a dependent token (reference behavior; applies to
Reloaded if D55.6 brings the passive state).* With a platform coin and one
token activated by a token activation method, an unforced call for the
platform answers `passivized` true; afterwards the token's `my_balance`
answers, a versioned `withdraw` for the platform answers `NoSuchCoin`,
`get_enabled_coins` lists both, and a second unforced call answers the
coin-not-active refusal.

T43.1E.8 *Forced removal (as T43.1E.7).* From the state T43.1E.7 ends in, a
call with `force_disable` true answers `passivized` false, and afterwards
neither the platform nor the token is listed. Under D43.1E.2 (b) the same
call is refused while the token has a running swap.

T43.1E.9 *No versioned form in the reference.* A request with `mmrpc: "2.0"`
and method `disable_coin` answers `NoSuchMethod`, HTTP 400, from the
reference; from Reloaded it answers the form of §43.1R.

## 43.1F Passive platform coins: re-activation, token activation and what a passive coin answers (reference behavior)

> **Clearance (chapter 01 R40) — passed the KDF Dirty Gate on 2026-11-18,
> measure 0.02 (attestation in §43.7).**
> This section was written on 2026-11-18 by the KDF Spec Reader (Spec Reader
> round 0.5.0-13) under the two-team procedure of chapter 01 §1.4A. It was
> written from two inputs: the
> reference lineage at the rows `v2.6.0-beta`, `v3.0.0-beta` and
> `main (as of 2026-10-07)` of chapter 02 §2.15A (read read-only; no untabled
> state was read), and this repository's own tree (for the compatibility
> note). It edits no sentence of §43.1A -- §43.1E or of §43.1R; it
> **completes R43.1E.6** (items 1, 3 and 6) and **chapter 55 D55.6**, and where
> a statement here is more exact than the sentence it completes, the
> statement here governs. Numbering: R43.1F.1 -- R43.1F.12,
> D43.1F.1 -- D43.1F.5 and T43.1F.1 -- T43.1F.12.

*Status note (2026-11-18).* The maintainer decided that Reloaded builds the
reference's passive state (R43.1E.5 item 3, R43.1E.6) in 0.5.0; chapter 55
D55.6 is thereby decided in favor of the passive state. This section
supplies what that build needs and §43.1E does not establish.

*Scope.* What a passive platform coin is reused for, what a
platform-with-tokens activation does when it names a passive platform or an
already registered token, which activations a passive platform refuses,
how the NFT chain and a layer-two coin take part, and what the methods that
need an active coin answer while a platform is passive. "Registered",
"dependent token" and "passive" have the meaning of R43.1E.5 and R43.1E.6;
"active" means registered and not passive. The *token record* of a platform
coin is the list of tokens the platform instance itself keeps for its
balance reports (R43.1F.3 item 6); it is distinct from the dependent-token
set of R43.1E.5 item 1.

*Reference states.* The three rows agree on everything in this section
(`main (as of 2026-10-07)` differs from `v3.0.0-beta` only in a read-me
notice), with one exception: the TRON platform (`TRX`) and its TRC20 tokens
exist as such at `v3.0.0-beta` and `main (as of 2026-10-07)` only; there
they follow every EVM rule below. At `v2.6.0-beta` a `TRX` platform is an
ordinary EVM coin (chapter 21 R21.10B.1) and follows the EVM rules as well.

**R43.1F.1** *Which platform coins can become passive.* Every platform coin
registered by any route -- a platform-with-tokens activation, a standalone
activation task, or the legacy `enable` or `electrum` method -- has a
dependent-token set, empty at first. A platform becomes passive only through
R43.1E.5 item 3, so only a platform whose set can gain a member:

| Platform family | Members its set can gain |
| --- | --- |
| EVM (incl. `TRX` where present) | ERC20/TRC20 tokens activated by `enable_eth_with_tokens`, `task::enable_eth::init`, `enable_erc20` or `task::enable_erc20::init`; the platform's NFT chain (R43.1F.8) |
| BCH | SLP tokens activated by `enable_bch_with_tokens` or `enable_slp` |
| Tendermint | tokens activated by `enable_tendermint_with_assets`, `task::enable_tendermint::init` or `enable_tendermint_token` |
| Solana (`experimental::` methods) | tokens activated by `experimental::enable_solana_with_assets` or `experimental::enable_solana_token` |
| Standard UTXO coin | a Lightning coin activated by `task::enable_lightning::init` (R43.1F.9) |

QTUM (whose QRC20 tokens activate only through the legacy methods), the
Zcash family and Sia never gain a member and so never become passive. A
platform activated through the legacy methods or a standalone task gains
members exactly as one activated through a platform-with-tokens method.

**R43.1F.2** *Re-activation reuses the passive instance.* A
platform-with-tokens activation whose `ticker` names a registered platform
coin decides as follows, **before** the platform's configuration is read:

1. The ticker is active: refused with `PlatformIsAlreadyActivated` (HTTP
   400), as chapters 35, 36 and 40 state.
2. The ticker is passive and the registered coin belongs to the family of
   the method (an EVM coin for the EVM methods, `TRX` included; a BCH coin
   for `enable_bch_with_tokens`; a Tendermint coin for the Tendermint
   methods; a Solana coin for the Solana method): the request **re-activates
   the passive coin** as R43.1F.3 describes.
3. The ticker is passive but the registered coin belongs to another family
   (for example a passive standard UTXO coin named by
   `enable_bch_with_tokens`): refused with `PlatformIsAlreadyActivated` (HTTP
   400); nothing changes.

Re-activation builds **no new platform instance**: the passive instance,
with every setting it was first built with, is used. The request's
platform-level members that would shape a new instance -- its nodes, its
signing policy, swap-contract settings, required confirmations, and
`tx_history` -- are not applied and are not checked against the existing
instance. The members that shape only the answer are applied: `get_balances`
and, for an HD wallet, the address-scan settings and `path_to_address` used
to build the balance report. The one-shot method's refusal of a hardware
signing policy (chapter 35 R35.1.2 phase 1) still comes first.

The task forms (`task::enable_eth::init`, `task::enable_tendermint::init`)
do not see a passive ticker as active: `init` answers a `task_id` and the
task re-activates as above, ending in its `Ok` status with the same answer
the one-shot form gives. The BCH and Solana families have no
platform-with-tokens task form; `task::enable_bch::init` is a standalone
activation and is governed by R43.1F.9.

**R43.1F.3** *What re-activation does, in order.*

1. The request's tokens are resolved and checked against the platform
   exactly as in a first activation (configuration or custom `protocol`,
   then the platform match), all of them before any is built; a failure is
   the refusal a first activation would answer (R43.1F.5).
2. Every requested token is then built anew from the request, also a token
   that is already registered (R43.1F.4); building may contact the nodes
   (for example to read a token's decimals when its configuration does not
   give them).
3. When the request carries an NFT request (`nft_req`, EVM only), the NFT
   chain is built anew (R43.1F.8).
4. The answer is assembled (R43.1F.3 item 6).
5. The platform is made **active** again, every requested token that is not
   yet registered is registered, and every requested ticker -- and the NFT
   ticker when item 3 applied -- joins the platform's dependent-token set.
   If the platform has meanwhile become active through another request, the
   call is refused with `PlatformIsAlreadyActivated` and registers nothing.
6. *The answer.* It has the shape the family's first activation answers. Its
   token part covers **every token on the platform's token record**, not
   only the request's tokens: the record holds each token that was ever
   activated onto this platform instance by a platform-with-tokens
   activation or by a token activation method and has not been disabled
   since, plus the request's tokens. With `get_balances` false the token
   tickers are listed instead of balances (EVM single-address, BCH,
   Tendermint); the EVM HD answer (chapter 35, HD shape) and the Solana
   answer always carry the balances, likewise for every recorded token. A
   token activated through the legacy methods is on no platform's record
   unless a later platform-with-tokens request names it. The NFT part of an
   EVM answer comes from item 3 only: without `nft_req` it is empty even
   when the platform's NFT chain is registered.

What re-activation leaves alone: the platform's background work continues
as it was -- an existing history collector, balance streams and event
streams are neither restarted nor duplicated, and **no history collector is
started** even when the request sets `tx_history` and none runs. (For
Solana, whose first activation with `tx_history: true` ends abnormally in
the reference -- chapter 40, the divergence note after R40.2.5 -- a
re-activation with `tx_history: true` therefore succeeds.) The dependent
tokens already registered stay registered and stay in the set, with their
own background work, and orders of the platform's tokens are untouched. No
order of the platform itself exists to restore: `disable_coin` cancelled
them (R43.1E.4).

**R43.1F.4** *A requested token that is already registered.* The
registration step of R43.1F.3 item 5 (and of a first activation) never
replaces a registered token and never refuses one: the instance already
registered is **kept**, the instance built from the request is discarded,
and the request's token-level members (for example its required
confirmations) are not applied to the kept instance. The ticker joins the
platform's dependent-token set and the platform's token record whatever the
kept instance is. Applied to the cases:

| Requested ticker | Outcome |
| --- | --- |
| (a) a dependent token of the passive platform being re-activated | kept; it stays in the set; the answer reports it |
| (b) a registered token whose platform is not registered -- possible only for a token activated through the legacy methods (an ERC20 token, or an SLP token, after its platform was removed; R43.1E.5 item 1) -- named in a first activation of that platform | the activation succeeds; the legacy-activated instance is kept and from now on is a **dependent** of the new platform, so a later unforced `disable_coin` of the platform makes it passive and a forced one removes the token too. A kept SLP instance stays bound to the BCH instance it was built with, not to the new one |
| (c) a token whose configuration names another platform | refused with `PlatformCoinMismatch` (HTTP 400) at item 1, before any token is built; nothing changes |
| (c') a custom token (request `protocol`, ticker not in the configuration) whose ticker is registered for another platform | the protocol names the requested platform, so no mismatch arises; the registered instance (of the other platform) is kept and its ticker joins this platform's set as well |
| (d) a custom ERC20/TRC20 token whose platform-and-contract pair is already registered under any ticker -- including case (a) when the dependent was itself a custom token | refused with `CustomTokenError` carrying `TokenWithSameContractAlreadyActivated` (chapter 35's custom-token table), **HTTP 500**, while the tokens are built (item 2); nothing is registered and a passive platform stays passive |
| (e) the same ticker twice in one request | not refused: both entries are built, the first one is registered unless the ticker was already registered, the ticker joins the set once and is reported once |

**R43.1F.5** *Refusals and what exists after them.* Every refusal of a
platform-with-tokens activation leaves the registry as it was: nothing is
registered and a passive platform stays passive. Before the refusal,
however:

1. in a first activation the platform instance has been built (and its
   nodes contacted) before any token is resolved, so a token refusal follows
   a full platform build;
2. in a re-activation no platform instance is built, and the token checks of
   R43.1F.3 item 1 come before any token is built;
3. a refusal that arises after the tokens were built in a re-activation --
   the NFT build of R43.1F.3 item 3, or a node failure while the answer of
   item 4 is assembled -- leaves the requested tokens on the passive
   platform's **token record** although they were not registered (see the
   code-quality finding below).

**R43.1F.6** *Token and layer-two activations while the platform is
passive.* Every standalone token activation and the layer-two activation
looks its platform up as an *active* coin, so a passive platform counts as
not activated:

| Method | Answer |
| --- | --- |
| `enable_erc20`, `enable_nft`, `enable_slp`, `enable_tendermint_token`, `experimental::enable_solana_token` | `PlatformCoinIsNotActivated`, string: the **platform** ticker, HTTP 400; nothing is built or registered |
| `task::enable_erc20::init` | the same refusal synchronously at `init` (string: the platform ticker, HTTP 400); no task is created |
| `task::enable_lightning::init` | the same refusal synchronously at `init`, string: the **Lightning** ticker (chapter 41 R41.2.6 row 6), HTTP 400; no task is created |
| legacy `enable`/`electrum` of an SLP token | refused (legacy error body, HTTP 500), the platform not being active |
| legacy `enable`/`electrum` of an ERC20 token | **accepted**: the token is built independently of the platform instance and registered, but it does not join the platform's set (R43.1E.5 item 1) |

So no new token joins a passive platform's set by any of these routes. One
race is the exception: a token task (`task::enable_erc20::init`) whose
platform was active at `init` and becomes passive while the task runs still
registers its token, and the token joins the passive platform's set.

**R43.1F.7** *A token already registered, named in a standalone token
activation.* The duplicate check of every standalone token activation
treats any registered token as active (a token is never passive), so it
answers `TokenIsAlreadyActivated` whatever the state of the token's
platform. This is unchanged by the passive state and is recorded only to
contrast it with R43.1F.4, where a platform-with-tokens request keeps the
registered token without refusing.

**R43.1F.8** *The NFT chain of an EVM platform (a TRON platform has
none).*

1. *It is a dependent.* An NFT chain activated by `enable_nft`, or inline by
   `nft_req` in `enable_eth_with_tokens` or `task::enable_eth::init`, joins
   the platform's dependent-token set. It is not put on the platform's token
   record, so it never appears among the token balances of R43.1F.3 item 6.
   Consequently a platform whose only dependent is its NFT chain becomes
   passive on an unforced `disable_coin`.
2. *Platform passive.* The NFT chain stays registered. The NFT methods that
   look up only the NFT chain go on answering; the methods that look the
   platform up by ticker refuse: `withdraw_nft` answers `NoSuchCoin`
   naming the platform (HTTP 404), and `get_nft_transfers` answers
   `TransferConfirmationsError` naming the platform (HTTP 400). A new
   `enable_nft` is refused as R43.1F.6 states.
3. *Platform forced off.* The NFT chain is removed with the other
   dependents and its background work is ended (R43.1E.5 item 4).
4. *Platform re-activated.* Without `nft_req` the registered NFT chain is
   kept untouched. With `nft_req` a new NFT chain is built and **replaces**
   the registered one -- unlike a token (R43.1F.4), the NFT entry is
   overwritten -- and the earlier entry is not separately ended.
5. *The NFT chain disabled by itself.* It leaves the set as any token does
   (R43.1E.5 item 2); a platform left with an empty set stays passive
   (R43.1E.6 item 5).

**R43.1F.9** *A standard UTXO platform with a Lightning dependent, and the
standalone activations.*

1. A standard UTXO coin with a registered Lightning coin becomes passive on
   an unforced `disable_coin` (R43.1F.1); the Lightning coin stays
   registered and keeps using the platform instance.
2. **No activation makes it active again.** No platform-with-tokens method
   accepts a standard UTXO coin (R43.1F.2 item 3 refuses it as
   `PlatformIsAlreadyActivated`). The legacy `enable` and `electrum` refuse
   it as already initialized (legacy error body, HTTP 500; R43.1E.6
   item 6). It stays passive until a forced `disable_coin` removes it and the
   Lightning coin, or until the node restarts (R43.1F.12).
3. *The standalone activation tasks* (`task::enable_utxo::init`, and alike
   `task::enable_bch::init` for a passive BCH platform): `init` does not see
   the passive ticker as active, answers a `task_id`, and the task builds a
   complete new coin -- connecting to its servers, scanning an HD wallet,
   and starting a history collector when `tx_history` is set -- and only at
   registration ends at status level with `CoinIsAlreadyActivated`
   `{ticker}` (chapter 55 R55.5.7 item 3). The registered passive coin is
   untouched and stays passive. Whether the background work of the discarded
   instance ends was not traced.

**R43.1F.10** *What "needs an active coin" means while a platform is
passive.*

1. *Methods addressed to the passive platform's own ticker* treat it as not
   active, each with its own not-found answer:

   | Method | Answer |
   | --- | --- |
   | legacy `my_balance`, `my_tx_history`, `get_trade_fee`, `setprice`, `buy`, `sell`, `max_taker_vol`, `withdraw` | the legacy error body, HTTP 500 |
   | versioned `withdraw` | `NoSuchCoin`, HTTP 404 (R43.1D.2) |
   | versioned `my_tx_history` | `CoinIsNotActive`, HTTP 404 |
   | `trade_preimage`, `max_maker_vol` | `NoSuchCoin` (`max_maker_vol`: HTTP 404; `trade_preimage`: HTTP 400) |
   | `account_balance` and `task::account_balance::init`, `get_new_address` and `task::get_new_address::init`, `task::create_new_account::init`, `task::scan_for_new_addresses::init` | `NoSuchCoin`, HTTP 400, answered by the call itself; no task is created |
   | `stream::balance::enable`, `stream::tx_history::enable`, `stream::fee_estimator::enable` | `CoinNotFound`, HTTP 404 |
   | `get_eth_estimated_fee_per_gas` (EVM) | `CoinNotFound`, HTTP 404 |
   | `get_swap_gas_fee_policy`, `set_swap_gas_fee_policy` (EVM) | `NoSuchCoin`, HTTP 400 |
   | `get_token_info` for a token protocol of that platform | `NoSuchCoin` naming the platform, HTTP 404 |

   `get_public_key` is not addressed to a coin and is unaffected.
   `get_enabled_coins` lists the passive platform in both forms (R43.1E.6
   item 2); the legacy form reports its address as for an active coin.
2. *Its background work goes on.* A history collector started at its first
   activation keeps collecting into the history storage (the versioned
   `my_tx_history` still refuses to read it, item 1); balance streams and
   other event streams already enabled for it keep publishing -- for an EVM
   platform the balance stream also covers the tokens on its token record.
   No new stream can be enabled for it (item 1).
3. *Methods addressed to a dependent token* look the token up, and a token
   is never passive, so they answer as for any active token: `my_balance`,
   both `withdraw` forms, `my_tx_history`, `get_eth_estimated_fee_per_gas`
   for the token's own ticker, and the token's own streams. The exception is
   every step that looks the **platform** up by its ticker. For the families
   in the reference, only the EVM family (ERC20 and TRC20 tokens) has such a
   step: an EVM token holds its swap gas-fee policy in its platform and reads
   it through the platform's ticker for every swap transaction and every
   swap fee estimate. While the platform is passive, therefore, for an EVM
   token:

   | Request touching the token | Answer |
   | --- | --- |
   | `trade_preimage` with the token on either side | `InternalError`, HTTP 500 |
   | `max_maker_vol` | `InternalError`, HTTP 500 |
   | legacy `setprice`, `buy`, `sell`, `max_taker_vol`, `get_trade_fee`, with the token on either side where a pair is named | the legacy error body, HTTP 500; no order is created |
   | `get_swap_gas_fee_policy`, `set_swap_gas_fee_policy` for the token | `NoSuchCoin` naming the platform, HTTP 400 |
   | `get_token_info` for the token's protocol | `NoSuchCoin` naming the platform, HTTP 404 |

   SLP, Tendermint and Solana tokens use the platform instance they were
   built with and have no such step: every method answers for them as while
   the platform was active (Solana tokens still refuse swaps, chapter 40).
4. *Existing orders of a dependent token* stay in place (R43.1E.4 cancels
   only the platform's own orders). For an EVM token such an order can still
   be matched, and the swap then fails at its start checks (R43.1F.11
   item 2).

**R43.1F.11** *Running swaps of a dependent token.* `disable_coin` checks
only the platform's ticker for running swaps (R43.1E.3), so a swap of a
dependent token can be running when the platform becomes passive. The swap
holds its coin instances and goes on.

1. An SLP, Tendermint or Solana token swap is not affected.
2. An EVM (ERC20/TRC20) token swap, of either protocol generation, fails at
   **every step that sends a transaction or estimates a swap fee for that
   token** while the platform is passive -- the dex-fee payment, the payment
   or funding transaction, the spend and the refund -- because each such step
   reads the gas-fee policy through the platform's ticker (R43.1F.10 item 3).
   The step fails as a failed send of that step does in the governing swap
   chapter (chapter 51 for the first generation, chapter 52 for the second);
   a swap still in its start checks fails them, in either generation, because
   those checks estimate the swap fees. Whether and when a failed step is
   retried is the governing chapter's; this rule adds only that the retry
   meets the same failure until the platform is active again. As far as
   traced, steps that neither send a transaction nor estimate a fee (waiting
   for confirmations, validating the other side's transaction) are not
   affected, except that the watcher-reward amount, where watcher rewards are
   used, is also computed through the platform.
3. The platform itself has no running swap at that moment (R43.1E.3), and
   none can start while it is passive (R43.1F.10 item 1).

**R43.1F.12** *The passive state is not persisted.* The flag lives only in
the running node's coin registry; nothing about it is written to disk. After
a restart no coin is registered until activated again, a platform activated
again is active, and a swap that resumes after a restart waits for its coins
to be activated as any resumed swap does. A platform passive before the
restart therefore comes back active with whatever tokens its new activation
names.

> **Code-quality finding (informative).** The registration step of
> R43.1F.4 inserts a requested token only when its ticker is free and does
> not look at what is registered under it otherwise, so a token activated
> earlier through the legacy methods is never told apart from a dependent.
> The effect is that a platform-with-tokens request silently adopts
> whatever instance is registered under a ticker -- a
> legacy-activated token (case b), or a custom token of another platform
> (case c') -- as its dependent, without telling the caller, and records the
> request's own token details on the platform's token record even when the
> kept instance differs (case c'). *Proposed fix:* in a platform-with-tokens
> request, refuse a requested ticker that is registered and is not already a
> dependent of this very platform, before anything is built (D43.1F.1).
>
> **Code-quality finding (informative).** Two further effects of the same
> registry are apparent from R43.1F.3 -- R43.1F.5: (1) a re-activation refused
> after its tokens were built leaves those tokens on the passive platform's
> token record, so a later answer of R43.1F.3 item 6 and an EVM balance
> stream report tokens that are not registered; (2) the registration of
> R43.1F.3 item 5 re-inserts the platform when it finds no registered entry,
> so a forced removal that lands between the start of a re-activation and its
> registration lets the removed instance -- whose background work the removal
> ended -- be registered again as an active coin. *Proposed fix:* build the
> token record only at registration, and make re-activation fail when the
> passive entry it started from is gone (D43.1F.2).

> **Compatibility (this repository on dev, 2026-11-18; its own code, not the
> reference).**
>
> 1. *No passive state yet* (§43.1E compatibility item 2): every rule above
>    that presupposes a passive platform is a target, not current behavior.
>    A platform-with-tokens request for any registered platform ticker
>    answers `PlatformIsAlreadyActivated`.
> 2. *Already registered token in a platform request.* Reloaded **replaces**
>    the registered token with the instance built from the request; the
>    earlier instance's background work is not ended. The reference keeps the
>    registered instance (R43.1F.4).
> 3. *Same-ticker serialization.* Reloaded serializes activations that name
>    the same platform or token (chapter 55 R55.1.12), so the race of
>    R43.1F.3 item 5 and the second effect of the code-quality finding above
>    do not arise between two activations; a `disable_coin` is not part of
>    that serialization.
> 4. *History start.* Reloaded's EVM platform activation can start a history
>    collector (chapter 55 D55.8); the reference's EVM platform-with-tokens
>    activation never does, at a first activation or a re-activation.

**D43.1F.1** *(Open, 2026-11-18.)* **A requested token that is already
registered.** Options: (a) reproduce the reference: keep the registered
instance silently, case by case as R43.1F.4; (b) keep it when it is already
a dependent of this platform (case a) and refuse every other registered
ticker -- cases b and c' -- with `TokenIsAlreadyActivated` (string: the token
ticker, HTTP 400) before anything is built; (c) Reloaded's current
behavior, replace it. A reference client that re-activates a passive
platform with its earlier token list relies on case (a) succeeding; options
(a) and (b) both keep that, option (c) keeps it only by replacing running
token instances. *Recommendation:* (b); it keeps every case a reference
client is known to rely on (re-activation naming its own dependents) and
turns the silent adoption into an answer.

> *Reloaded status (clean side, 2026-11-18):* resolved by the maintainer's
> decision of 2026-11-18 as a node setting that chooses between options (b)
> and (a). The top-level startup-configuration member
> `reactivation_registered_token` takes one of two values:
>
> - `"refuse"` (the default) -- option (b): a requested token that is
>   already a dependent of this very platform is kept (case a); any other
>   registered ticker (cases b and c') is refused with
>   `TokenIsAlreadyActivated` (string: the token ticker, HTTP 400) before
>   anything is built;
> - `"keep"` -- option (a), the reference behavior of R43.1F.4: the
>   registered instance is kept silently.
>
> Under either value a token whose configuration names another platform is
> still refused with `PlatformCoinMismatch` (case c). Option (c), replacing
> the registered instance, is not offered.

**D43.1F.2** *(Open, 2026-11-18.)* **Failed re-activations and the
forced-removal race (the second code-quality finding).** Options: (a)
reproduce the reference; (b) update the token record only at registration,
and refuse the re-activation (`PlatformIsAlreadyActivated` is unsuitable;
`Internal`, HTTP 500, is the available token) when the passive entry it
started from was removed meanwhile, or serialize `disable_coin` with the
activations of the same ticker. *Recommendation:* (b), serializing
`disable_coin` through the lock of chapter 55 R55.1.12.

> *Reloaded status (clean side, 2026-11-18):* resolved by the maintainer's
> decision of 2026-11-18 as option (b) in its serializing form: the
> platform's token record is written only at registration, and
> `disable_coin` is serialized with the activations of the same platform
> instead of a re-activation being refused when its passive entry was
> removed meanwhile.

**D43.1F.3** *(Open, 2026-11-18.)* **EVM token operations that need the
platform while it is passive** (R43.1F.10 item 3, R43.1F.11 item 2).
Options: (a) reproduce the reference: such steps fail while the platform is
passive, also inside a running swap; (b) let a token's own internal look-up
of its platform find the passive platform (the platform stays unusable for
requests addressed to its ticker), so a token's swaps, orders and fee
estimates keep working. Option (a) can strand a running swap's spend or
refund until the platform is re-activated; option (b) changes no answer a
reference client relies on except that the refusals of R43.1F.10 item 3
become successes. *Recommendation:* (b).

> *Reloaded status (clean side, 2026-11-18):* resolved by the maintainer's
> decision of 2026-11-18 as option (b). A token's own internal look-up of its
> platform finds the passive platform, so the token's swaps, orders and fee
> estimates keep working while the platform is passive.

**D43.1F.4** *(Open, 2026-11-18.)* **A standalone activation task for a
passive ticker** (R43.1F.9 item 3). Options: (a) reproduce the reference:
`init` answers a `task_id`, the task builds a whole coin and ends with
`CoinIsAlreadyActivated`; (b) refuse at `init` with `CoinIsAlreadyActivated`
`{ticker}` (HTTP 400), as for an active coin, so nothing is built.
*Recommendation:* (b); the terminal answer a client sees is the same token,
only earlier.

> *Reloaded status (clean side, 2026-11-18):* resolved by the maintainer's
> decision of 2026-11-18 as option (b): `init` of a standalone activation task
> for a passive ticker answers `CoinIsAlreadyActivated` `{ticker}`, HTTP 400,
> and nothing is built.

**D43.1F.5** *(Open, 2026-11-18.)* **Re-activation and `tx_history`**
(R43.1F.3). Options: (a) reproduce the reference: the request's
`tx_history` is ignored on re-activation; (b) start the history collector
when the request sets `tx_history`, the family supports it and none is
running for the platform. *Recommendation:* (a); a passive platform keeps
the collector it had, and (b) would make re-activation differ from the
reference in what is written to the history storage.

> *Reloaded status (clean side, 2026-11-18):* resolved by the maintainer's
> decision of 2026-11-18 as option (a), the reference behavior: a
> re-activation ignores the request's `tx_history`.

*Not established.* (1) Whether the background work of an instance built and
then discarded (a refused first activation, R43.1F.5 item 1; the standalone
task of R43.1F.9 item 3; a token built for an already registered ticker,
R43.1F.4) ends was not traced. (2) The order-matching side of an existing
EVM-token order while its platform is passive (R43.1F.10 item 4) was traced
to the swap start only. (3) Feature-gated or WASM-only routes (MetaMask,
WalletConnect specifics, cross-chain routing of Tendermint orders) were not
examined. (4) Nothing was measured against a running reference binary.

> *Reloaded status (clean side, 2026-11-18):* the maintainer decided on
> 2026-11-18 that ending a coin's background work when the coin is removed,
> or when an instance built for an activation is discarded, is built as a
> separate change, the fifth of the passive-state work. Item (1) above and
> item (1) of the not-established paragraph of §43.1E stay open until then.

*Acceptance rows.* Each row runs against a node with mock backends; *parity*
is the reference behavior above. Rows that need a passive platform apply to
Reloaded once the passive state is built.

T43.1F.1 *EVM re-activation reuses the instance.* Activate an EVM platform
with token A, disable the platform unforced (passive), then
`enable_eth_with_tokens` for the platform naming token B with different
nodes and `get_balances` true: the call answers HTTP 200; its token balances
list A and B; the platform answers `my_balance` again; the nodes contacted
are the first activation's; `get_enabled_coins` lists platform, A and B
once each.

T43.1F.2 *Task form.* As T43.1F.1 with `task::enable_eth::init`: `init`
answers a `task_id`, and the status ends `Ok` with the same answer shape.

T43.1F.3 *Re-activation naming its own dependent.* From the passive state
of T43.1F.1, a request naming token A only answers HTTP 200 and A stays the
registered instance (its required confirmations unchanged although the
request names others). Under D43.1F.1 (b) the same result.

T43.1F.4 *Already active.* A request for an active platform answers
`PlatformIsAlreadyActivated`, HTTP 400; for a passive platform of another
family (a standard UTXO coin named by `enable_bch_with_tokens`), the same.

T43.1F.5 *Legacy-activated token adopted (case b).* Activate an ERC20 token
through legacy `enable`, then the platform by `enable_eth_with_tokens`
naming that token: parity, HTTP 200, the legacy instance is kept and an
unforced `disable_coin` of the platform answers `passivized` true. Under
D43.1F.1 (b): `TokenIsAlreadyActivated`, HTTP 400, nothing registered.

T43.1F.6 *Configuration mismatch.* A re-activation naming a token whose
configuration names another platform answers `PlatformCoinMismatch`, HTTP
400, and the platform stays passive.

T43.1F.7 *Token activation on a passive platform.* With the platform
passive: `enable_erc20` answers `PlatformCoinIsNotActivated` with the
platform ticker, HTTP 400; `task::enable_erc20::init` the same at `init`;
legacy `enable` of another ERC20 token succeeds and the token is not a
dependent (a forced removal of the platform leaves it registered).

T43.1F.8 *NFT chain.* With a platform whose only dependent is its NFT
chain, an unforced `disable_coin` answers `passivized` true; `withdraw_nft`
then answers `NoSuchCoin` (HTTP 404); a re-activation with `nft_req`
answers HTTP 200 and the NFT chain is registered once.

T43.1F.9 *UTXO with Lightning.* With a standard UTXO coin and a Lightning
coin, an unforced `disable_coin` answers `passivized` true; legacy
`electrum` for the coin is refused (HTTP 500); parity:
`task::enable_utxo::init` answers a `task_id` whose status ends with
`CoinIsAlreadyActivated`; under D43.1F.4 (b) `init` itself answers it, HTTP
400.

T43.1F.10 *Passive platform's own methods.* With a passive platform:
versioned `my_tx_history` answers `CoinIsNotActive` (HTTP 404),
`stream::balance::enable` answers `CoinNotFound` (HTTP 404),
`task::account_balance::init` answers `NoSuchCoin` (HTTP 400), and
`get_public_key` answers as before.

T43.1F.11 *EVM token while the platform is passive.* With an ERC20
dependent and a passive platform: the token's `my_balance` and versioned
`withdraw` answer as before; parity: `trade_preimage` with the token
answers `InternalError` (HTTP 500) and `setprice` with the token is refused.
Under D43.1F.3 (b) both succeed.

T43.1F.12 *Not persisted.* Make a platform passive, restart the node:
`get_enabled_coins` lists nothing until coins are activated, and
`enable_eth_with_tokens` for the platform then builds a new instance and
answers HTTP 200.

## 43.1R Reloaded versioned forms of the legacy-only methods (Reloaded-original, clean side)

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
`withdraw`, with the statuses the reference answers for it (R43.1D.2; the
documentation states none): `NoSuchCoin` at 404 and `Transport` at 502, where
the order and trading forms answer 400 and 500. *(Amended 2026-11-16: the
wording of 2026-11-15 said "as R43.1C.8 documents it" and noted Reloaded's
`withdraw` answering 500; since 2026-11-16 Reloaded's `withdraw` answers 502
as well, D43.1C.4 third note.)* The same error name thus carries
different statuses in the two groups, as it does on the documented surface
(D43.1C.4 second status note).

**R43.1R.1** *Surface.* `setprice`, `update_maker_order`, `cancel_order`,
`cancel_all_orders`, `my_orders`, `order_status`, `buy`, `sell`,
`orderbook_depth`, `orders_history_by_filter`, `my_balance`, `disable_coin`,
`send_raw_transaction`, `validateaddress`, `convertaddress`,
`convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`,
`set_required_confirmations`, `set_requires_notarization`,
`all_swaps_uuids_by_filter`, `import_swaps`, `version`, `stop`, `metrics`,
`get_my_peer_id`, `get_directly_connected_peers`, `get_gossip_mesh`,
`get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh`,
`ban_pubkey`, `list_banned_pubkeys` and `unban_pubkeys` are routed on the
versioned table as well as the legacy table. *(Amended 2026-11-16, phases 4
and 5: the fourteen swap, daemon and network methods added.)* The legacy forms and their
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
| `all_swaps_uuids_by_filter` | all optional: `my_coin` string, `other_coin` string, `from_timestamp` integer, `to_timestamp` integer (seconds) |
| `import_swaps` | `swaps` list of swap records |
| `version`, `stop`, `metrics`, `get_my_peer_id`, `get_directly_connected_peers`, `get_gossip_mesh`, `get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh`, `list_banned_pubkeys` | none; `params` absent, `null`, `{}` or any object |
| `ban_pubkey` | `pubkey` string (32-byte hex); `reason` string (required, as on the legacy surface); `duration_min` integer (optional; absent = until unbanned) |
| `unban_pubkeys` | `unban_by` object `{"type": "All"}` or `{"type": "Few", "data": [pubkeys]}` |

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
`all_swaps_uuids_by_filter` `{uuids, my_coin, other_coin, from_timestamp,
to_timestamp, found_records}` (the member is `found_records`, as the legacy
body has it); `import_swaps` `{imported, skipped}` (per-record failures stay
inside `skipped`, R43.1B.3; the versioned form runs on the request task,
where the legacy form runs on the blocking-work executor, an accepted
difference with no wire effect); `version` the legacy object unchanged, so the
versioned answer nests a `result` member holding the version string beside
`datetime` inside `result` (the bare-body rule of R43.1C.6 applied without a
redesign); `stop` the string `"success"`,
answered before the shutdown proceeds; `metrics` the bare `{metrics}` object;
`get_my_peer_id` the peer identifier string; the four gossip and peer
queries their legacy maps or lists; `ban_pubkey` `"success"`;
`list_banned_pubkeys` the map of public key to ban record;
`unban_pubkeys` `{still_banned, unbanned, were_not_banned}`;
`order_status` the legacy
bare object unchanged (`type`, `order`, the orderbook-ticker members where the
legacy body has them, and `cancellation_reason` for a history order; R43.1B
table row `order_status`).

**R43.1R.4** *Errors (W3–W6).* The mmrpc error envelope of chapter 55
R55.1.2. A request the client got wrong answers 400, a failure of the node
500; a refusal while the node's own clock is off (`ClockOutOfRange`) 503. A
refusal while a coin's Electrum servers disagree answers 400, as `withdraw`
does (chapter 38). The wallet and coin forms (phase 3) follow the `withdraw`
set as the reference answers it (R43.1D.2; the documentation states no
status): an unknown coin 404, a transport failure 502. One token may cover
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
| `NoSuchCoin` | object `{coin}` | 404 | coin not found or not activated (the `withdraw` set of R43.1D.2) | my_balance, disable_coin, send_raw_transaction, validateaddress, convertaddress, convert_utxo_address (target coin), kmd_rewards_info (KMD), set_required_confirmations, set_requires_notarization |
| `Transport` | string | 502 | node or server unreachable, invalid answer, broadcast refused by the network, and a transaction the coin cannot decode (the coin's broadcast gives one text for all three; the `withdraw` set of R43.1D.2) | my_balance, send_raw_transaction, kmd_rewards_info |
| `InvalidParam` | string | 400 | request carries neither `tx_hex` nor `tx_json`, or the wrong type; address conversion refused by the coin | send_raw_transaction, convertaddress |
| `InvalidAddress` | string | 400 | address does not parse | convert_utxo_address |
| `UnsupportedError` | string | 400 | target coin is not a UTXO coin | convert_utxo_address |
| `ActiveSwapsBlockDeactivation` | object `{coin, swaps}` | 400 | running swaps use the coin (the legacy body carries `swaps` beside `error`) | disable_coin |
| `MatchingOrdersBlockDeactivation` | object `{coin, matching, cancelled}` | 400 | orders of the coin are being matched (the legacy body carries `orders` beside `error`) | disable_coin |
| `FeeCapBelowBaseFee` | as chapter 35 | 500 | EVM fee estimate below base fee (balance read) | my_balance |
| `InvalidTimeStampRange` | none | 400 | `from_timestamp` after `to_timestamp` (browser builds; a native node answers an empty list) | all_swaps_uuids_by_filter |
| `DbError` | string | 500 | swap storage failure (the `my_recent_swaps` set) | all_swaps_uuids_by_filter |
| `InvalidParam` | string | 400 | public key banned already | ban_pubkey |
| `Internal` | string | 500 | peer identity not initialized; metrics registry failure; swaps context or ban-list lock failure (the `peer_connection_healthcheck` set and the general convention) | get_my_peer_id, metrics, ban_pubkey, list_banned_pubkeys, unban_pubkeys |
| none | | | `version`, `stop`, `import_swaps` and the peer and gossip queries other than `get_my_peer_id` have no failure of their own; a request that does not decode answers the dispatcher's `InvalidRequest` | |

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
- *Section 43.1R amended 2026-11-16 (phases 4 and 5), D43.1A.1 status note
  extended, status note after D43.1C.4 on the sibling sets of the last two
  groups.* Reloaded-original, clean side: the fourteen swap, daemon and network
  methods join the tables from this repository's own legacy shapes and failure
  classes; no forbidden-class input, chapter 01 R38 not engaged.
- *Section 43.1R amended 2026-11-15 (phase 3), D43.1A.1 status note extended,
  second status note after D43.1C.4.* Reloaded-original, clean side: the ten
  wallet and coin methods join the tables from this repository's own legacy
  shapes and failure classes, and the per-group application of D43.1C.4 is
  recorded as the maintainer decided; no forbidden-class input, chapter 01
  R38 not engaged.
- *Scoped addition (2026-11-16, Spec Reader round 0.5.0-9) -- §43.1D
  (R43.1D.1 -- R43.1D.4, the error table, the compatibility note, the
  not-established paragraph, T43.1D.1 -- T43.1D.6) and §43.1E (R43.1E.1 --
  R43.1E.8 with their tables, the compatibility note, D43.1E.1 -- D43.1E.2,
  the not-established paragraph, T43.1E.1 -- T43.1E.9); no other text of
  this chapter was changed.* **KDF Dirty Gate: PASS on 2026-11-16, measure 0.02, no findings**
  (chapter 01 R40; the attestation closes this item). *Inputs:* (1) the forbidden corpus
  at the rows `v2.6.0-beta` and `v3.0.0-beta` of chapter 02 §2.15A, read
  read-only and consulted **only** to establish: for the versioned
  `withdraw`, the `error_type` tokens of its error set, the JSON kind of
  each token's payload, including the payloads nested in three of them, the
  HTTP status of each token, which methods share the set, which tokens no
  case was found to answer, and the one token in which the two rows differ;
  for the legacy `disable_coin`, the method table that routes it, its
  request members and how they are read, its success members, each refusal
  with its body members, its HTTP status and the condition under which it
  applies, the order of the method's checks and effects, what it does to the
  node's orders, when a token counts as dependent on a platform coin, the
  passive state of a platform coin and what observes it, what a forced call
  removes and which tickers its checks cover; and the two rows' agreement
  (the `disable_coin` method, the coin-registry operations it uses, its two
  checks and both method tables are the same at both rows; the `withdraw`
  error set differs in the one token R43.1D.4 names). Behavior, public wire
  names, payload kinds and statuses only; no private identifier, function
  body, helper decomposition, error, log, comment or test text was
  reproduced, and the error texts of both methods are deliberately not
  recorded. (2) The published reference API documentation, requested on
  2026-11-16 under
  `https://komodoplatform.com/en/docs/komodo-defi-framework/api/` and served,
  after a permanent redirect, from the host §43.1B names: three pages -- the
  versioned `withdraw` page, the v2.0 protocol overview page and the legacy
  `disable_coin` page -- consulted **only** to establish whether an HTTP
  status is documented, which `withdraw` tokens and payload kinds are
  documented, and the documented request, success and error members of
  `disable_coin`. Member names and tokens cross as the API contract; no
  descriptive sentence, example value or error text of the documentation was
  reproduced, and the token of the overview page's `withdraw` error example
  is described, not named. (3) The public source of the `num-bigint` crate,
  for the serialized form of an unsigned big integer (the two array-valued
  members of one NFT token in R43.1D.2). (4) This repository's tree at the
  base of this round (the dev branch as of 2026-11-16), for the two
  compatibility notes. D43.1E.1 -- D43.1E.2 are Reloaded-original. No row
  was added to chapter 02 §2.15A: both states read already had one, and each
  row's identifier was confirmed, by read-only inspection of the historical
  record's reference metadata, to be the commit its release tag names; the
  documentation is not a state of the historical record and takes no row, so
  it is named here by URL and fetch date.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-16, measure
  0.02, no findings. Reference states (chapter 02 R13): PASS; the rows
  `v2.6.0-beta` and `v3.0.0-beta`. Extent: the versioned `withdraw` error
  set with its payload serialization and status mapping; the legacy
  `disable_coin` method, the coin-registry operations and the two in-use
  checks it relies on, the order cancellation by coin; both method tables;
  the reference's tests on disabling platform coins; 12 source files read in
  the relevant parts at `v3.0.0-beta`, with row agreement confirmed by
  mechanical difference against `v2.6.0-beta`; mechanical comparison against
  29 files at `v3.0.0-beta` and 28 at `v2.6.0-beta`; three public
  documentation pages reached on 2026-11-16 and compared. Method: reading,
  plus mechanical comparison (5-word overlap of the gated lines against
  comment and literal wording; identifier-shaped tokens and
  identifier-derived phrases against the identifier set); the additions
  embed no code blocks.
- *Status notes added 2026-11-16 -- third note after D43.1C.4, note after
  D43.1E.1; wording amendment in §43.1R.* Reloaded-original, clean side: they
  record the maintainer's 2026-11-16 decisions (withdraw `Transport` 502;
  D43.1E.1 option (c)) and correct the "documented statuses" wording to what
  §43.1D established; no forbidden-class input, chapter 01 R38 not engaged.
- *Scoped addition (2026-11-18, Spec Reader round 0.5.0-13) -- §43.1F
  (R43.1F.1 -- R43.1F.12 with their tables, the two code-quality findings,
  the compatibility note, D43.1F.1 -- D43.1F.5, the not-established
  paragraph, T43.1F.1 -- T43.1F.12) and the status note after D43.1E.2; no
  other text of this chapter was changed.* **KDF Dirty Gate: PASS on
  2026-11-18, measure 0.02, no findings** (chapter 01 R40; the attestation
  closes this item). *Inputs:* (1) the forbidden corpus at the rows
  `v2.6.0-beta`, `v3.0.0-beta` and `main (as of 2026-10-07)` of chapter 02
  §2.15A, read read-only and consulted **only** to establish: how a
  platform-with-tokens activation (one-shot and task forms, every family that
  has one) treats a registered passive or active platform ticker and a
  requested token that is already registered; the order of its token checks,
  token builds, answer and registration, and what exists after a refusal;
  what its answer reports; which token, NFT and layer-two activations look
  their platform up as an active coin and what they answer; which coin
  families can gain dependents; what the standalone activation tasks and the
  legacy activation methods do for a passive ticker; which methods addressed
  to a passive platform or to a dependent token look a coin up as active,
  with their answer tokens and HTTP statuses; which token-family operations
  look the platform up by ticker and the swap steps that do so; whether the
  passive flag is persisted; and the rows' agreement (identical in every
  compared respect; TRON exists only at `v3.0.0-beta` and
  `main (as of 2026-10-07)`, which differs from `v3.0.0-beta` only in a
  read-me notice). Behavior, public wire names, answer tokens and statuses
  only; no private identifier, function body, helper decomposition, error,
  log, comment or test text was reproduced; the first code-quality finding
  restates as a behavioral fact a gap that a code comment in the reference
  acknowledges, without quoting it. (2) This repository's tree on the dev
  branch as of 2026-11-18, for the compatibility note. D43.1F.1 -- D43.1F.5
  are Reloaded-original. No row was added to chapter 02 §2.15A: all three
  states read already had one, and each row's identifier was confirmed by
  read-only inspection of the historical record's reference metadata.
  *Dirty Gate clearance (chapter 01 T1, R40):* PASS, 2026-11-18, measure
  0.02, no findings. *Extent:* the platform-with-tokens activation in its
  one-shot and task forms, including the re-activation of a passive
  platform; the coin registry and its passive flag; dependent registration
  (tokens, the NFT chain, the layer-two coin); the standalone, NFT,
  layer-two and legacy activations; the not-found and error answers this
  section lists; the EVM swap gas-fee policy look-up; and the reference's
  tests of the passive scenario. About 15 files were read at
  `v3.0.0-beta`, the primary state; the governing files were cross-checked
  at `v2.6.0-beta` (identical), and `main (as of 2026-10-07)` differs only
  in a read-me notice. *Method:* reading, plus a mechanical five- and
  four-word sequence comparison against comments and literals (no
  five-word match, two generic four-word matches), a per-token
  forced-versus-free test, and a scan for pull-request numbers and commit
  identifiers. *Measure:* 0.02.
- *Status notes added 2026-11-18 -- second note after D43.1E.1, notes after
  D43.1E.2, D43.1F.1 -- D43.1F.5 and the not-established paragraph of
  §43.1F.* Reloaded-original, clean side: they record the maintainer's
  decisions of 2026-11-18 on those items; no cleared sentence was changed
  and no forbidden-class input was used, chapter 01 R38 not engaged.
