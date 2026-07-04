# Chapter 45 — Startup Configuration and Environment Tolerance

**Status:** driving-spec.

> **One-sentence claim:** the daemon shall remain launch-interchangeable with
> GLEEC-era / upstream nodes by treating unset, empty, and unexpected-type
> configuration fields and environment variables the way upstream does —
> starting in a tolerant default mode (including a no-signing-identity
> *no-login* mode when no passphrase is supplied) rather than aborting at
> startup.

This chapter binds the **startup-time tolerance contract**: what the daemon
must DO when a runtime-configuration field or a launch environment variable is
absent, empty, or carries a value of an unexpected type. It complements
[Chapter 02](02-baseline-state.md) §2.11 (the bound configuration surface),
[Chapter 06](06-network-id-seed-node.md) (network-id and seed-node accessors),
[Chapter 07](07-wallet-lifecycle-and-key-export.md) (the named-wallet startup
handshake), and [Chapter 43](43-daemon-rpc-dispatcher-and-misc-maintenance.md)
(RPC authentication and the `rpc_password` strength policy). It does not
redefine those surfaces; it binds the *unset / empty / unexpected* boundary
behaviour for launch success, and resolves it toward upstream compatibility.

> **Binding scope.** Configuration field names, environment-variable names,
> default values, file-name defaults, and start-versus-refuse semantics are
> dictated interoperability facts (a public config/launch contract authored by
> operators and by GUI frontends that drive the daemon) and are normative here.
> Private helper names, accessor decomposition, diagnostic text, and module
> layout are non-normative and are intentionally omitted.

## 45.1 Executive Summary

A graphical wallet, an integration harness, or an operator launches the binary
with only environment variables set (for example a coins-list path pointing at
a temporary file) plus a minimal runtime configuration, and expects the daemon
to start. Upstream and the public KomodoPlatform documentation do **not**
require a `passphrase`: a node may start with no signing identity at all
(*no-login* mode), deriving identity instead from a named wallet, from a
hardware wallet, or from a plaintext passphrase only when one is supplied.

The motivating defect this chapter closes is a launch-time refusal: the active
reloaded tree aborts startup when no `passphrase` is present and no
hardware-wallet identity is configured. That refusal is an unintended
divergence — it makes the binary non-interchangeable with upstream for launch
purposes and breaks GUI wallets that drive the daemon in no-login mode. This
chapter binds the tolerant, upstream-compatible behaviour: a missing passphrase
is a supported *no-login* start, not a fatal condition.

The chapter generalises the same discipline across the launch-relevant
configuration fields and environment variables: each unset/empty/unexpected
input maps to a bound outcome — a documented default, a documented degraded
mode, or (only where the daemon genuinely cannot proceed) a clean refusal — and
no launch input may be able to *panic* the process.

## 45.2 Scope and Cross-References

R45.2.1 This chapter binds startup-time behaviour only: the window from process
launch through the point at which the RPC surface begins serving. Runtime
reconfiguration is out of scope.

R45.2.2 Identity/wallet resolution detail for the named-wallet path
(`wallet_name` / `wallet_password`) is bound by Chapter 07 and is referenced,
not redefined, here. This chapter binds only the *passphrase-absent* identity
outcome and how it composes with the Chapter 07 handshake.

R45.2.3 RPC authentication and the `rpc_password` strength policy are bound by
Chapter 43 and are referenced, not redefined, here.

R45.2.4 Network-id and seed-node accessor semantics are bound by Chapter 06 and
are referenced, not redefined, here.

## 45.3 Configuration and Coins-List Resolution

R45.3.1 **Configuration source precedence.** The runtime configuration is a
single JSON document resolved in this order: (a) a JSON configuration supplied
directly as the process's first command-line argument, when present; otherwise
(b) the contents of the configuration file named by the `MM_CONF_PATH`
environment variable. When `MM_CONF_PATH` is unset, the bound default
configuration file name is `MM2.json`.

R45.3.2 When neither a command-line configuration nor a readable, non-empty
configuration file is available, the daemon shall refuse to start with a
configuration error. A configuration document that is present but is not valid
JSON shall likewise be refused. These are the only two genuinely
unrecoverable configuration-source conditions.

R45.3.3 The canonical configuration-path environment variable is `MM_CONF_PATH`.
No `MM2_CONF_PATH` alias is part of the contract; implementations shall not
introduce one as a silent synonym.

R45.3.4 **Coins-list resolution.** When the configuration document carries a
non-null inline `coins` array, that array is the coins list. When the inline
`coins` field is absent or null, the coins list shall be loaded from the file
named by the `MM_COINS_PATH` environment variable. When `MM_COINS_PATH` is
unset, the bound default coins file name is `coins`.

R45.3.5 When the inline `coins` field is absent **and** the resolved coins file
is absent or empty, the daemon shall refuse to start with a configuration
error. A coins file that is present but is not valid JSON shall likewise be
refused.

R45.3.6 An environment variable that selects a file path (R45.3.1, R45.3.4) but
is set to an empty string shall be treated as equivalent to a *missing* file at
that slot: the empty value selects neither a usable path nor the built-in
default, so the corresponding "file absent" rule applies.

## 45.4 Identity Resolution and Passphrase Tolerance

R45.4.1 **No-login mode is a supported start.** When no signing identity is
configured — that is, when `passphrase` is absent/null, no `wallet_name` is
configured, and no hardware-wallet identity is configured — the daemon shall
start normally in *no-login* mode: it initialises no signing-key context and
exposes the identity-free portion of its surface (for example public discovery
and orderbook-viewing functionality). A missing passphrase in this state is
**not** a fatal condition and shall not refuse the launch.

R45.4.2 **Plaintext passphrase.** When `passphrase` is present as a string and
no `wallet_name` is configured, the daemon shall derive its signing identity
directly from that passphrase (single-key by default, or a hierarchical-
deterministic identity when HD mode is enabled in the configuration). The
plaintext passphrase is not persisted in this legacy path.

R45.4.3 **Encrypted-passphrase object.** When `passphrase` is present as an
encrypted-data object (rather than a plaintext string), it shall be accepted
only in conjunction with a configured `wallet_name`, because storing and
verifying an encrypted passphrase requires a named-wallet slot (Chapter 07).
An encrypted-passphrase object supplied **without** a `wallet_name` shall be
refused as a configuration error.

R45.4.4 **Named-wallet path.** When `wallet_name` is configured, identity
resolution follows the Chapter 07 startup handshake (which requires a
`wallet_password`, may load an existing stored mnemonic, and may generate or
encrypt-and-persist one). The passphrase-present and passphrase-absent
sub-cases of that path are bound by Chapter 07; this chapter only requires that
the *passphrase-absent + wallet_name configured* combination is handled by that
handshake and is not short-circuited into a launch refusal by a passphrase
presence check.

R45.4.5 **Unexpected passphrase type.** A `passphrase` value that is neither a
string nor a valid encrypted-data object (for example a number, boolean, or
array) is a configuration type error and shall refuse the launch. The refusal
shall be a clean configuration error, never a panic.

R45.4.6 **Empty-string passphrase.** An empty-string `passphrase` is a *present*
(non-null) value and is routed to identity derivation (R45.4.2 / R45.4.4), not
to no-login mode. Whether an empty seed is itself acceptable is governed by the
identity-derivation layer (Chapters 05/07) and is out of scope here; this
chapter binds only that an empty string does not select no-login mode.

R45.4.7 **Hardware-wallet identity.** A configured hardware-wallet identity is
an alternative signing-identity source that, like a passphrase, satisfies the
"identity present" condition. Its presence shall not be required for a
successful launch, and its absence shall not by itself refuse a launch
(R45.4.1).

R45.4.8 **HD-mode identity selection (single source of truth).** The
configuration field `enable_hd` is a boolean that selects, for *every* path
that initialises a passphrase-derived signing identity — the plaintext-
passphrase path of R45.4.2 **and** the named-wallet path of R45.4.4 /
Chapter 07 — which key-pair policy the resolved seed is bound to. It shall be
parsed as a JSON boolean defaulting to `false` when absent, null, or of any
non-boolean type (the same truthy convention as `allow_weak_password`). When
`enable_hd` is truthy the resolved plaintext seed MUST initialise a **global-HD
account** context (the BIP-39 / BIP-32 hierarchical-deterministic identity of
Chapter 05); otherwise it MUST initialise the baseline **Iguana** single-key
context. This requirement is the single normative source that discharges the
"or a global-HD account when HD mode is enabled" clause of Chapter 07 R28 and
the parenthetical of R45.4.2: the selection MUST be applied at the **one**
startup identity-initialisation site that consumes the resolved seed, so that
no resolved-seed path (legacy plaintext, generate-and-persist, re-login load-
and-use, first-save, confirm, or import-and-save) can silently fall back to the
Iguana policy while HD mode is configured. An acceptance test MUST assert both
directions: a truthy `enable_hd` yields a global-HD key-pair policy (such that
HD-only operations — e.g. deriving an additional account/address — are
available), and an absent or `false` `enable_hd` yields the Iguana policy.

## 45.5 Per-Field Startup Tolerance

R45.5.1 The following launch-relevant configuration fields are bound to the
listed unset / empty / unexpected-value behaviour. "Tolerated" means the daemon
starts; "refuse" means the daemon declines to start with a clean configuration
error (never a panic).

| Field | Absent / null | Empty or unexpected type | Bound default / outcome |
| --- | --- | --- | --- |
| `passphrase` | Tolerated → no-login (R45.4.1) | Empty string → derivation (R45.4.6); wrong type → refuse (R45.4.5) | No signing identity unless supplied |
| `coins` (inline) | Falls back to coins file (R45.3.4) | Malformed → refuse (R45.3.5) | Loaded from coins file |
| `rpc_password` | See R45.5.2 / Chapter 43 | Empty treated as unset for auth purposes | Protected RPCs stay gated |
| `netid` | Tolerated → default network id `0` | Non-integer → default `0`; out-of-range → refuse (R45.5.3) | `0` |
| `seednodes` | Tolerated → empty seed set | Non-array → treated as empty | Empty list; feeds `disable_p2p` default |
| `disable_p2p` | Tolerated → derived default (R45.5.4) | Non-boolean → treated as unset | Derived from seednodes/bootstrap/in-memory presence |
| `i_am_seed` | Tolerated → `false` | Non-boolean → `false` | `false` |
| `gui` | Tolerated → unset metadata | Any value used only as metadata | No effect on launch success |
| `dbdir` | Tolerated → default data directory | Unwritable/uncreatable → refuse (R45.5.5) | Built-in default data directory |
| `rpcip` | Tolerated → loopback default | — | `127.0.0.1` |
| `rpcport` | Tolerated → default port | `0` → bind any free port; `<1024` non-zero → refuse; `>65535` → refuse; numeric string accepted | `7783` |
| `allow_weak_password` | Tolerated → `false` | Non-boolean → `false` | `false` |
| `enable_hd` | Tolerated → `false` (Iguana single-key identity) | Non-boolean → `false` | `false`; truthy selects the global-HD identity (R45.4.8) |

R45.5.2 **`rpc_password` absence/empty.** An absent or empty `rpc_password`
shall not silently grant unauthenticated access to protected methods: protected
RPCs remain gated (Chapter 43 R43.3.1). Any derivation of a default password
from the passphrase, and the startup strength-policy validation, are governed
by Chapter 43; in no-login mode (no passphrase) there is no passphrase to
derive from, and the node still starts with only its identity-free surface
reachable.

R45.5.3 **`netid` range.** An absent, empty, or non-integer `netid` resolves to
the default network id `0`. A `netid` value that exceeds the unsigned 16-bit
range is a configuration error and shall refuse the launch with a clean error.

R45.5.4 **`disable_p2p` default.** When `disable_p2p` is absent (or not a
boolean), its effective value is derived from whether the node is a bootstrap
node, whether any `seednodes` are configured, and whether an in-memory P2P
transport is selected. An explicit boolean value is honoured as given.

R45.5.5 **`dbdir` writability.** When `dbdir` is absent, the bound default data
directory is used. Whichever directory is selected MUST be creatable and
writable; if it cannot be created or written, the daemon shall refuse to start
with a clean filesystem error.

R45.5.6 **Networking-bootstrap inputs are not launch gates.** Absence of
`seednodes` (or of a reachable seed) affects the node's ability to *join* the
peer mesh at runtime but shall not by itself refuse the launch. A node may
start with an empty seed set.

## 45.6 Environment-Variable Tolerance

R45.6.1 `MM_CONF_PATH` — selects the configuration file path. Unset → the
default file name `MM2.json` (R45.3.1). Empty → treated as a missing
configuration file (R45.3.6).

R45.6.2 `MM_COINS_PATH` — selects the coins-list file path. Unset → the default
file name `coins` (R45.3.4). Empty → treated as a missing coins file
(R45.3.6).

R45.6.3 `MM_LOG` — selects an optional log-file destination. Unset → file
logging is not enabled and the daemon logs to its default sink. When set, the
value is expected to name a file ending in the `.log` suffix; a value that does
not end in `.log`, or that names a path that cannot be opened, shall disable
file logging and allow startup to continue. An invalid `MM_LOG` value shall
never refuse the launch.

R45.6.4 `RUST_LOG` — selects the log-level filter. Unset or empty → the
daemon's built-in default log level. An unparsable filter value shall degrade
to the default log level and shall never refuse or panic the launch.

R45.6.5 No environment variable bound in this chapter is *required* for a
successful launch; each has a bound default or a bound degraded mode.

## 45.7 No-Panic-On-Launch Obligation (security/robustness)

R45.7.1 No combination of unset, empty, or unexpected-type configuration fields
or environment variables shall be able to panic or crash the process during
startup. A genuinely unrecoverable launch input (R45.3.2, R45.3.5, R45.4.3,
R45.4.5, R45.5.3, R45.5.5) shall produce a clean, structured configuration
error and an orderly non-zero exit, not a panic or an aborted backtrace.

R45.7.2 This obligation is the launch-time counterpart of the no-panic-via-RPC
obligation bound in Chapter 43 R43.4. Any change to the startup path must
preserve at least this floor.

## 45.8 Tests (test invariants)

T45.1 *No-login launch.* A configuration that omits `passphrase`, omits
`wallet_name`, and configures no hardware-wallet identity — combined with a
valid coins source — MUST start the daemon and reach its identity-free RPC
surface, and MUST NOT refuse the launch.

T45.2 *GUI-style env-only launch.* A launch driven by environment variables for
the coins-list path plus a minimal configuration that omits `passphrase` MUST
start successfully, exercising R45.3.4 and R45.4.1 together.

T45.3 *Plaintext passphrase launch.* A configuration with a string `passphrase`
and no `wallet_name` MUST start with a derived signing identity (R45.4.2).

T45.4 *Unexpected passphrase type.* A `passphrase` set to a number, boolean, or
array MUST refuse the launch with a clean configuration error and MUST NOT
panic (R45.4.5, R45.7.1).

T45.5 *Missing config / coins.* With neither a command-line configuration nor a
readable configuration file, the daemon MUST refuse cleanly; with a
configuration that omits inline `coins` and no readable coins file, the daemon
MUST refuse cleanly (R45.3.2, R45.3.5).

T45.6 *Empty env paths.* Setting the config-path or coins-path environment
variable to an empty string MUST behave as a missing file at that slot
(R45.3.6).

T45.7 *Field defaults.* Omitting each of `netid`, `i_am_seed`, `gui`, `rpcip`,
`rpcport`, and `allow_weak_password` MUST resolve to the bound defaults in
§45.5 and MUST start the daemon.

T45.8 *Bad logging env vars.* An `MM_LOG` value not ending in `.log`, and an
unparsable `RUST_LOG` value, MUST each allow the daemon to start (file logging
disabled / default level), not refuse or panic (R45.6.3, R45.6.4).

## 45.9 Deferred Work

D45.1 A consolidated, typed startup-configuration schema (replacing ad-hoc
per-field access against a loosely-typed JSON document) is deferred. This
chapter binds the external tolerance contract; a typed loader may be introduced
later provided it preserves every outcome bound in §45.4–§45.6.

D45.2 A launch-time "configuration lint" that warns (without refusing) on
suspicious-but-tolerated inputs — for example an empty seed set on a non-seed
node, or a `gui` value that is present but not a string — is deferred as an
operator-experience enhancement.

## 45.10 External References

- KomodoPlatform Komodo DeFi Framework documentation — *Configure the
  KDF/MM2 JSON* and the API walkthrough — for the operator-facing meaning of
  `passphrase`, `coins`, `rpc_password`, `netid`, `gui`, `rpcip`/`rpcport`,
  `i_am_seed`, `seednodes`, `dbdir`, and `allow_weak_password`, and for the
  environment variables `MM_CONF_PATH`, `MM_COINS_PATH`, and `MM_LOG`.
- The daemon's own `--help` / `-h` output, which documents the same
  configuration fields and environment variables and marks `rpc_password` as
  derivable from the passphrase when unset.
- Chapter 02 (baseline state) §2.11 — the bound two-file configuration surface
  (`MM2.json`, `coins`) and its status as a public interoperability contract.
- Chapter 06 (network-id and seed-node decoupling) — the `netid` and
  `seednodes` accessor semantics referenced in §45.5.
- Chapter 07 (wallet lifecycle) — the named-wallet startup handshake (§7.8)
  that owns the `wallet_name` / `wallet_password` identity path referenced in
  §45.4.4.
- Chapter 43 (daemon RPC dispatcher) — the `rpc_password` strength policy
  (§43.3.2) and the no-panic-via-RPC obligation (§43.4) referenced in §45.5.2
  and §45.7.

## 45.11 Upstream Divergence (informative)

> **Upstream divergence (informative).** The active reloaded tree currently
> refuses to start when `passphrase` is absent and no hardware-wallet identity
> is configured, treating a missing passphrase as a fatal "field not found"
> condition. Upstream and the public documentation treat a missing passphrase
> as a supported *no-login* start. This chapter binds the upstream-compatible
> tolerant behaviour (§45.4.1) so the binary remains launch-interchangeable with
> GLEEC-era / upstream nodes and with GUI wallets that drive the daemon with
> only environment variables and a minimal, passphrase-free configuration.

> **Upstream divergence (informative).** An out-of-range `netid` is handled in
> some upstream lineages by aborting the process abruptly. This chapter binds a
> clean configuration-error refusal instead (§45.5.3, §45.7.1), consistent with
> the no-panic-on-launch obligation; the externally observable result (the node
> does not run on an invalid network id) is unchanged.

## 45.12 Provenance Footer

- *Inputs consulted for this chapter:* the bound configuration surface of
  Chapter 02 §2.11; the network/seed accessors referenced from Chapter 06; the
  named-wallet startup handshake of Chapter 07 §7.8; the RPC authentication and
  password-policy bindings of Chapter 43 §43.3–§43.4; the daemon `--help`
  configuration/environment documentation; and the public KomodoPlatform KDF
  configuration and API-walkthrough documentation.
- *Permitted-input classes used:* reloaded source and sibling-chapter bindings;
  chapter-bound configuration field names and environment-variable names
  introduced here as the public launch/config contract; default values and
  start-versus-refuse semantics stated abstractly; public project
  documentation.
- *Sibling chapters cross-referenced:* Chapter 02, Chapter 06, Chapter 07,
  Chapter 43.
- *Author of this chapter:* clean-room driving-spec working set.
- *Forbidden corpus:* consulted only to recover the externally observable
  startup tolerance contract (which unset/empty/unexpected inputs start the
  daemon, in what mode, and which refuse). No private identifiers, function
  bodies, control flow, or error/log string literals were carried across; all
  behaviour is restated as functional requirements.
