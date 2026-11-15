# Plan: one error vocabulary for the versioned RPC surface

**Status:** planning (candidate for a later version, not 0.5.0; no schedule) — **Last reviewed:** 2026-11-15

> **Context.** The versioned (`mmrpc: "2.0"`) surface answers a failure with an `error_type` token, an
> `error_data` payload and an HTTP status, but each method's error set chose these on its own. CRD ch.43
> R43.1C.8 records the spread on the documented surface: the internal-failure token is `Internal` in some
> sets and `InternalError` in others; `CoinIsWalletOnly` carries a string in one set and a record in
> another; an unknown coin answers 400 in one set and 404 in another; `Transport` is 500 in one and 502 in
> another. The maintainer decided on 2026-11-15 that the versioned forms added for the legacy-only methods
> follow their nearest documented sibling (ch.43 D43.1C.4), which keeps the spread rather than widening it,
> and ordered this plan for the version after: one spelling and one payload kind per failure across
> methods, and HTTP statuses where HTTP has a fitting one. Nothing here is executed.

## Why

A client that handles failures by `error_type` has to learn, per method, which spelling and which payload
kind to expect for the same failure, and cannot branch on the HTTP status alone because the same failure has
different statuses in different sets. One vocabulary makes a generic handler possible: one match arm per
failure, one payload reader per token, one meaning per status.

## Scope

- **In:** every `error_type` the versioned surface can return, including the `task::` status-level
  errors (`details` of a `status` answer) and the dispatcher's own tokens (ch.43 R43.1A.3): spelling,
  `error_data` kind and members, HTTP status.
- **Out:** the legacy surface (its error body is a string with HTTP 500, ch.55 R55.1.10, and stays so); the
  human-readable `error`, `error_path` and `error_trace` members (not part of the contract); the meaning of
  any failure (which condition raises it stays bound by the governing chapters).

## What clients would see change

| Today | After |
| --- | --- |
| `Internal` in some sets, `InternalError` in others | one token for an internal failure of the node |
| `CoinIsWalletOnly` as a string in `orderbook` and `best_orders`, as a record with `coin` in `trade_preimage` | one payload kind per token, chosen so that no information is lost (a record where any set carries members) |
| `NoSuchCoin` 400 in trading sets, 404 in `withdraw`; `CoinIsNotActive` 404 | one status per failure class |
| `Transport` 500 in one set, 502 in another | one status for an unreachable or misbehaving backend |
| An unknown order or swap identifier 400 (`UUIDNotFound`, `NoSwapWithUuid`) | 404, if the maintainer takes the HTTP-fitting option (D1) |
| A state conflict (`OrderBeingMatched`, `OrderStateChanged`, `OrderDeleted`, `TaskFinished`-like cases) 400 or 409 by set | 409 throughout, if D1 takes that option |

Every row is a wire change for an existing client that matches the old token, kind or status. That is why
the plan needs a compatibility period (below) and why it is a documented deliberate divergence from the
reference surface under `rpc-api-reference-wire-parity.md` §2.2 for every method the reference documents.

## How a compatibility period could work

Three mechanisms, to be chosen in D2; they can be combined.

1. **Per-request opt-in.** A client that wants the new vocabulary says so in the request; the envelope has
   no free member for it, so it would be a new `mmrpc` version value (for example `"2.1"`), which R43.1A.2
   already keys the envelope on, or an optional top-level member of the versioned request, which R43.1A.2
   forbids today and would have to be loosened for that one member. Old clients see no change.
2. **Node-wide switch.** One `MM2.json` boolean (default: old vocabulary for one release, new vocabulary
   from the next) under `docs/COMPAT_SWITCHES.md`, with the GLEEC-compatible value recorded in
   `docs/GLEEC_COMPATIBILITY.md`. Simple for operators, invisible to clients, but a node cannot serve old
   and new clients at once.
3. **Documentation-only alias period.** The new token is emitted, and both spellings are listed in the
   chapter for one release with a removal date and a deprecation notice. A client only reads error tokens,
   so there is nothing to accept on the request side.

Whichever is chosen, the release that flips the default carries an upgrade note in the CHANGELOG, the
divergence record of §2.2, and a deprecation line in every affected `error_type` row of the governing
chapters.

## Work packages (when scheduled)

1. **Inventory.** A table of every `error_type` the versioned surface returns, with its set, payload kind,
   status, and the failure class it names (the per-method table form of ch.43 §43.1R R43.1R.4), built from
   the chapters and the code. The
   reference's documented spellings come from ch.43 §43.1C and further Spec Reader rounds where a set is not
   yet recorded.
2. **Vocabulary.** One row per failure class: the token, the payload kind and members, the status; the
   mapping from every old token. Decided with the maintainer (D1, D3).
3. **Mechanism.** The compatibility mechanism of D2, built once in the dispatcher and the error rendering of
   ch.55 R55.1.2, with tests for both vocabularies for one release.
4. **Migration per chapter.** Each governing chapter's error table gains the new column; the code of each set
   moves to the shared vocabulary behind the mechanism; the deviations page and the catalog list the change.
5. **Flip and removal.** The default flips in a later release; the old vocabulary is removed one release
   after that.

## Open decisions (for the version that schedules this plan)

| # | Question | Options |
| --- | --- | --- |
| D1 | HTTP statuses | (a) keep the two-value rule of today (400 caller-side, 500 node-side, 503 node state); (b) HTTP-fitting statuses where one exists: 404 unknown identifier or coin, 409 state conflict, 502 unreachable backend, 503 node state, 400 everything else the caller got wrong |
| D2 | Compatibility mechanism | (a) new `mmrpc` version value; (b) node-wide switch; (c) documentation-only alias period; (d) a combination of (a) and (b) |
| D3 | Internal-failure token | `Internal` (the dispatcher's and `orderbook`'s spelling) or `InternalError` (`trade_preimage`, `withdraw`, the plan `mmrpc-v2-completion` forms) |
| D4 | Payload rule | always a record (string payloads become `{"message": ...}` or a named member), or the richest kind any set uses today per token |
| D5 | Scope of the first flip | the whole surface at once, or the trading and wallet sets first and the rest later |

## Dependencies

- `rpc-api-reference-wire-parity.md` (parked): its §2.2 record form and the register file it names.
- `mmrpc-v2-completion.md`: its forms are the first consumers of the shared vocabulary; until this plan
  lands they follow ch.43 D43.1C.4 option (ii).
- CRD ch.55 §55.1 (envelope and error rendering), ch.43 §43.1A and §43.1C.

## Out of scope

- Changing which condition raises which failure.
- The legacy error body.
- The `error`, `error_path` and `error_trace` texts.
