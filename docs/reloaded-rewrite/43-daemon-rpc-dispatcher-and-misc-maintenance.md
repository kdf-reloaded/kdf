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
- *Forbidden corpus:* not consulted.
