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

> **Dirty-side derivation record (sanitized).** R45.3.7 and T45.9 were
> authored by the KDF Spec Reader under
> the chapter-01 two-team workflow (§1.4A), from the GLEEC KDF public release
> designations `v3.0.0-beta` (reference) and `v2.6.0-beta` (legacy
> cross-check), which agree. They carry only the externally observable
> tolerance of a coins-list entry towards keys the reader does not recognise.
>
> **Clearance (chapter 01 T1, R40).** That material -- R45.3.7 and T45.9 --
> **passed the KDF Dirty Gate** on 2026-10-24: PASS, discretionary-expression
> measure 0.01, finding list empty, and accordingly no remedy class for any
> line or for the candidate as a whole. Its clearance is therefore *complete*
> and not pending, and R40 withholds nothing in it. One limit of that pass
> belongs here and not only in the footer: the comparison reached material at
> the `v3.0.0-beta` designation only, so the agreement between the two
> designations that the record above asserts **is not attested by the
> clearance** and rests on the derivation record alone. The extent, method and
> measure of the pass are recorded once, in the *Dirty Gate clearance* item of
> §45.12, and are not restated here.

R45.3.7 **An unrecognised key in a coins-list entry is tolerated.** A coin
entry in the resolved coins list is a loosely-typed JSON object from which each
consumer reads the keys it needs by name. An entry carrying a **top-level key
no consumer recognises** shall be accepted: the daemon shall start, the coin
shall remain activatable, and the unknown key shall neither refuse the launch
nor refuse the coin's activation. Specifically:

- the key is **silently ignored**. No rejection, and no warning or diagnostic
  is emitted for it -- the daemon has no inventory of permitted coin-entry keys
  against which to notice one, so an unknown key is indistinguishable from a
  key belonging to a consumer that is not active in this build;
- the same tolerance holds for an unrecognised member *inside* a recognised
  nested object (for instance the per-coin gas-limit blocks of
  [Chapter 57](57-evm-fee-inputs.md) R57.5.1);
- the tolerance is a property of the reader, not of a version check. It
  therefore holds for **older** nodes reading a **newer** shared coins file:
  publishing a new top-level coin-entry key into the shared public coins file
  cannot break a node that predates the key. This is the interoperability fact
  the rule exists to record, because the shared coins file is consumed by nodes
  of many versions at once;
- what *is* rejected is a **recognised** key whose value fails to convert to
  the type that key's consumer expects. That is a per-coin activation error
  naming the key (R45.5.1's "unexpected type" column, and R57.5.1 for the
  gas-limit case), not a launch refusal, and it is unaffected by this rule;
- two keys are nonetheless load-bearing for startup and are **not** covered by
  this tolerance, because a consumer does read them for every entry: the
  entry's ticker key, and the order-book alias key that the order-matching
  subsystem reads across the whole list at startup. An entry whose ticker key
  is absent or not a string, or whose alias key is present but not a string,
  fails that whole-list read and refuses the launch with a clean configuration
  error (R45.3.5's class). A coins file is therefore tolerant of *extra* keys
  and intolerant of a *malformed* ticker or alias.

Reloaded shall preserve this tolerance exactly. A typed coins-entry schema
introduced under D45.1 shall be permissive about unknown members by
construction; making it strict would be a silent compatibility break against
every shared coins file that is newer than the binary reading it.

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
| `malloc_arena_max` *(Reloaded-original, §45.6A)* | Tolerated → limit `2` | Not a whole number from `0` to `2147483647` → warning and limit `2` (R45.6A.4) | `2`; `0` leaves the allocator's own default |

R45.5.2 **`rpc_password` absence/empty.** An absent or empty `rpc_password`
shall not silently grant unauthenticated access to protected methods: protected
RPCs remain gated (Chapter 43 R43.3.1). Any derivation of a default password
from the passphrase, and the startup strength-policy validation, are governed
by Chapter 43; in no-login mode (no passphrase) there is no passphrase to
derive from, and the node still starts with only its identity-free surface
reachable. (Chapter 43 §43.3 records a code-quality finding on a shipped
gap in this floor for the *absent*-`rpc_password` case specifically; the
binding requirement in this paragraph is unchanged.)

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

R45.6.6 *Startup path report (KDF Reloaded original addition; operator
diagnostics).* After the logger is initialized, startup shall report at INFO
level, as absolute paths (relative paths resolved against the working directory
without symlink resolution; the path as given if resolution fails):

- the source of the runtime configuration: the command-line argument, or the
  configuration file with whether it was selected via `MM_CONF_PATH` or by the
  default name; a host-supplied configuration (library entry point) is reported
  as such;
- the source of the coins list: inline in the configuration (noting an
  `MM_COINS_PATH` that is set but therefore ignored), or the coins file with
  whether it was selected via `MM_COINS_PATH` or by the default name;
- the log file named by a usable `MM_LOG` value (R45.6.3);
- the database root and the per-identity database directory;
- the optional file the configuration references, when configured: the
  named-wallet record file actually read (canonical or legacy). The report
  names **no** `wss_certs` file: the paths of the certificate and private-key
  files are not logged by it (the errors that report a bad file name the path
  as before).

The report names paths and source kinds only; it shall never include the
contents of any file, the command-line configuration text, or any configuration
value. Producing the report shall never fail or panic the launch (R45.7.1). The
log text is operator-facing and non-normative: it carries no wire or
compatibility obligation.

R45.6.7 *Version line (KDF Reloaded original addition; operator
diagnostics).* The daemon shall state its version in the form
`KDF-Reloaded DeFi Framework <crate version> (<short commit hash>) DT <build
datetime>` in the startup banner log line, which is also the output of the
`--version` and `version` command-line arguments, and in the form
`Version: <crate version> (<short commit hash>) DT <build datetime>` in the
second startup log line. The crate version is the version of the released binary
crate (`mm2_bin_lib`) as built, not that of an internal library crate (a
pre-release suffix is shown as it is). The short commit hash and the
build datetime are the same values as before; a build without Git information
shows its hash value as it does today. This requirement changes nothing else
that carries the hash: the result of the `version` RPC method and the version
recorded in swap records stay the bare short hash. The log text is
operator-facing and non-normative: it carries no wire or compatibility
obligation.

## 45.6A Memory Allocator Arena Limit (Reloaded-original)

> **Reloaded-original (2026-10-31, Decision BJ).** This section binds a
> startup setting that the compatibility contract of this chapter does not
> cover. It changes how the process asks the system's memory allocator for
> memory. It changes no order, swap, wallet or network behavior, and no value
> any peer or client can observe. The plan is
> [`docs/plans/malloc-arena-cap.md`](../plans/malloc-arena-cap.md).

**Why.** On Linux with the standard (glibc) allocator, every thread that
allocates memory can get an allocation area of its own, an *arena*. The
daemon runs several hundred threads on a many-core host (about 780 on a
256-core host) and about 30 on an 8-core host. Memory that a thread frees
inside its arena is often not reusable by the others and cannot always be
returned to the system. Under heavy order updating the memory held by the
process then grows although the data the daemon holds does not. Limiting the
number of arenas removes that growth (R45.6A.7).

R45.6A.1 **Platform.** The limit is applied only by the native daemon process
on Linux built against glibc. On every other platform (other operating systems,
other C libraries) and in the embedded-library launch path, in which the daemon
runs inside the process of a host application, the daemon neither sets a limit
nor logs a line about it.

R45.6A.2 **Moment.** The limit is applied once per process, after the
configuration has been read and before the daemon starts any of its worker
threads. A limit set later does not reduce arenas that exist already, so the
moment is part of the contract.

R45.6A.3 **Source and precedence.** The limit in effect is taken from the first
of these that applies:

1. *The environment.* If the environment variable `MALLOC_ARENA_MAX` is set to
   a non-empty value, the operator has chosen. The daemon makes no allocator
   call of its own, so the allocator's own reading of that variable applies, and
   the configuration key is ignored.
2. *The configuration key `malloc_arena_max`.* A whole number from `0` to
   `2147483647` selects that limit; `0` means "leave the allocator's own
   default" and the daemon makes no allocator call of its own.
3. *The default.* If the key is absent or `null`, the limit is `2`.

R45.6A.4 **Tolerance.** A `malloc_arena_max` value that is not a whole number in
the range of R45.6A.3 item 2 (a string, a boolean, a fraction, a negative
number, a number above `2147483647`, an array or an object) is treated as
invalid: the daemon logs a warning that names the key and states that the
default is used, and the limit in effect is `2`. If the allocator refuses the
limit, the daemon logs a warning that states the value it asked for, and goes on
with the allocator's own default. Neither case refuses or aborts the launch
(R45.7.1).

R45.6A.5 **Startup line.** On a platform where R45.6A.1 applies, the daemon
logs exactly one information-level line at launch that states the outcome in
plain words and names the source, one of: the limit in effect set from the
default; the limit in effect set from the configuration; the limit left to the
environment, with the value of `MALLOC_ARENA_MAX` as given; or no limit set
because the configuration key is `0`. An invalid key (R45.6A.4) is reported by
its warning, and the line then states the default.

R45.6A.6 **What it is not.** The limit is not a cap on the memory of the
process. It does not change how much data the daemon holds. A value of `1` or
`2` makes threads wait for each other more often when they allocate at the same
time; the default `2` is chosen from the measurements in R45.6A.7, which show no
slowdown in the order-update workload they covered. The effect on activation
and swap workloads under the default is not measured (D45.4).

R45.6A.7 **Measured basis (informative).** One maker with 480 orders on 16
pairs, each order updated once about every 6 seconds (about 80 updates per
second) for 40 minutes, the same binary each time:

| Setting | Arenas | Memory growth per minute (from minute 10) | Data held by the daemon |
|---|---|---|---|
| Allocator default, 256-core host | about 780 | about +1.2 MB | about 63 MB, flat from minute 20 |
| Allocator default, 8 cores | 36 | about +1.0 MB | about 55 MB, flat from minute 20 |
| `MALLOC_ARENA_MAX=2`, 256-core host | 2 | about −0.04 MB (flat) | about 61 MB, flat from minute 20 |

With the limit the update rate (81.8 against 81.3 updates per second) and the
processor use (22.3 % against 21.7 % of one core) were the same within noise.
Whether the growth without the limit levels off in a longer run was not
measured when this table was made; R45.6A.8 reports it.

R45.6A.8 **Measured basis after the first beta (informative).** The measurements deferred as D45.4 and D45.5 were run on
2026-11-03 with one binary, the key `malloc_arena_max` set to `0` as the arm without the limit. Plateau runs at about 20
order updates per second on the 8-core setting, 6.5 hours per arm: both arms levelled off by the pre-registered rule
(minutes 180–360, fixed-phase points: resident memory +0.004 ± 0.003 MB/min with the limit, +0.019 ± 0.009 without;
60–180 and 180–360 are reported separately in the plan). With the limit, resident memory was about 17 MB lower, entirely in
memory the allocator keeps after freeing it, with 2 arenas against 36–37. This is an observation of consecutive runs, not a
pre-registered comparison. The limit is therefore not shown to be necessary for a plateau at that load on 8 cores; it
removed the growth measured at about 80 updates per second on 256 cores (R45.6A.7). Rows a–c: no cost of the limit was
resolved on start-up and activation (a UTXO set, an EVM coin with tokens, a shielded coin), on eight swaps (three UTXO
pairs, one EVM cell; no shielded swap) or on the latency of the query methods and of `update_maker_order` while 100 orders
were updated at 20 per second; the default of `2` stands. The figures, the deviations from the registered procedure and the
runs not made are in `docs/plans/malloc-arena-cap.md`.

## 45.6B Hostname Resolution (Reloaded-original)

> **Reloaded-original (2026-11-02, maintainer decision).** This
> section binds one setting for every hostname the native daemon looks up when
> it opens an outgoing connection: seeds and other peers, coin servers and every
> HTTP(S) service. It changes no wire shape and no order, swap or wallet
> behavior. It changes which DNS servers learn the names the node connects to.
> The plan is
> [`docs/plans/p2p-dialing-and-discovery.md`](../plans/p2p-dialing-and-discovery.md),
> item P5.

**Why.** Every name the node looks up tells the DNS servers it asks which
seeds, coins and services the node uses. Before this section the node asked two
different resolvers: a fixed public resolver for the names of seeds (chapter
28, the reference note after R8c's rationale, item 4) and the operating
system's resolver for every other name. Neither was the operator's choice. This
section gives the operator one setting for all of them, with a default that
does not depend on the local network's resolver.

R45.6B.1 **Platform.** This section applies to the native daemon, including the
embedded-library launch path. A browser build leaves every lookup to the
browser and ignores the key.

R45.6B.2 **The key `dns_resolver`.** Its value selects the *mode*:

| Value | Mode |
|---|---|
| absent, `null` or `"google"` | **Google Public DNS**, the default: the servers `8.8.8.8`, `8.8.4.4`, `2001:4860:4860::8888` and `2001:4860:4860::8844`, port 53. |
| `"system"` | **The system resolver**: every name is looked up by the operating system's own name-lookup service, exactly as an application asks it, with no cache or added behavior of the node's own (R45.6B.4, R45.6B.5 do not apply). |
| a non-empty array of at most 8 strings | **Fixed servers**: only the listed DNS servers, preferred in the order given. Each string is `"<IPv4>"`, `"<IPv4>:<port>"`, `"<IPv6>"`, `"[<IPv6>]"` or `"[<IPv6>]:<port>"`; the port defaults to 53 and must be 1 to 65535. |

Any other value refuses the launch with a clean configuration error that names
the key, repeats the value and lists the accepted forms (R45.7.1). That includes
another string (the keywords are lowercase and exact), an empty array, an array
with more than 8 entries, an entry that is not a string or not an address in
one of the forms above (a hostname, for example), a port outside 1 to 65535, a
number, a boolean and an object. This is a deliberate exception to the
tolerance rule of §45.5: a resolver the operator did not choose would send every
name to servers the operator meant to avoid, so a mistyped value is refused,
never replaced by the default.

R45.6B.3 **Queries in the Google and fixed modes.** The node asks the servers
of its mode directly, over UDP, and repeats a query over TCP when the answer is
truncated. Each query waits up to 5 s for an answer, and a lookup as a whole
fails after at most 15 s. No other server is ever asked: there is no fallback
to the system resolver or to any other server, in any mode.
1. *Address families.* For a name with no family given (an HTTP(S) URL, an
   Electrum or lightwalletd server, a `/dns` address), the node uses the name's
   IPv4 addresses if it has any, and otherwise its IPv6 addresses. A `/dns4`
   address uses IPv4 only and a `/dns6` address IPv6 only.
2. *The local hosts file.* The operating system's hosts file (`/etc/hosts`,
   and its counterpart on Windows) is read when the resolver is installed
   (R45.6B.8). A name it lists is answered from it, without a query. A change
   to the file takes effect at the next start.
3. *Address literals and `localhost`.* A name that is an IP address needs no
   lookup, in any mode. In the Google and fixed modes, `localhost` and every
   name ending in `.localhost` resolve to the loopback addresses without a
   query, as RFC 6761 reserves them. Without this rule a hosts file that does
   not list `localhost` (the Windows default) would send it to the servers of
   the mode.
4. *Queries.* Each UDP query uses a random query ID and a fresh random source
   port, and an answer is accepted only for the question asked. A name is
   looked up as an absolute name: no search domain is appended.

R45.6B.4 **Cache (Google and fixed modes).** Answers are cached in memory for
at most 1,024 entries.
1. *Lifetimes.* An answer is kept for the lifetime its records carry, but at
   least 30 s and at most 1 hour. An answer that the name does not exist or has
   no address of the family asked for is kept at least 5 s and at most 5
   minutes.
2. *Never on disk.* Nothing about lookups is written to disk: a stored cache
   would reveal which coins the node runs. The cache starts empty at every
   start.

R45.6B.5 **Last known answer (Google and fixed modes).** For at most 1,024
names, the oldest dropped first, the node keeps the last successful answer in
memory.
1. *When it is used.* When a lookup fails because no server of the mode
   answered (no answer in time, connection refused, network unreachable) or
   because the server reported its own failure (a server failure or a refusal),
   and the name's last known answer expired at most 24 hours ago, the lookup
   returns that answer.
2. *When it is not.* Never when a server answered that the name does not exist
   or has no address. That answer is authoritative, not a glitch.
3. *Logging.* One WARN line per name per hour states that the last known
   answer is served for the name, and its age.
4. *What it is not.* It is not a fallback: no other resolver is asked.

R45.6B.6 **Failure.** A lookup that fails fails only the connection or request
that needed it. That connection's own logic retries it: the redial guards for
seeds (chapter 28 R27a), the peer-name record for peer exchange (chapter 28 R8c
item 7), the server rotation for Electrum (chapter 38 R38.6.4d), and the node
rotation for EVM and the other HTTP(S) families. A failed lookup never stops the
node or keeps it from starting. For each name, at most one WARN line per hour
says that the name could not be resolved, through which mode and why, and that
`dns_resolver` selects the resolver. The record of names logged holds at most
1,024 names.

R45.6B.7 **Startup line.** The daemon logs exactly one INFO line at launch that
names the mode:
- for the default, that the names of every seed, peer, coin server and service
  the node connects to are sent to Google Public DNS, and that `dns_resolver`
  set to `"system"` uses the operating system's resolver instead;
- for `"system"`, that the operating system's resolver is used;
- for fixed servers, the servers in order.

R45.6B.8 **One resolver per process, installed at start.**
1. *Moment.* The node's start-up reads the key and installs the resolver
   before it starts the peer-to-peer layer and before any coin activation or
   HTTP(S) request.
2. *Replacement.* A later node start in the same process (the embedded-library
   path stops a node and starts another) installs its own resolver, replacing
   the earlier one together with its cache and last known answers.
3. *A lookup before installation.* Inside the node this is a programming
   error, never a mode: a debug build fails an assertion, and a release build
   logs one ERROR line and looks the name up through the system resolver.
   Outside the node (unit tests and helper programs that never run the node's
   start-up), a lookup uses the system resolver.

R45.6B.9 **Where it applies.** In this release the resolver of the mode
answers:
1. *Peer-to-peer dials.* Every dial of a `/dns`, `/dns4` or `/dns6` address,
   by the node's own resolution layer under its TCP and WebSocket transports
   (chapter 28 R0b, R18). Every address a name resolves to is tried in turn
   until one connects. A WebSocket dial keeps the hostname for its TLS check.
   A `/dnsaddr` address is not supported: its dial fails with an error that
   says so. Configured seeds never produce one (chapter 28 R21), and peer
   exchange skips it (chapter 28 R8c).
2. *Peer-exchange names,* the lookups of chapter 28 R8c item 7.
3. *HTTP(S) requests* of the node's shared HTTP client and of the Tendermint
   RPC client: EVM nodes (chapter 35), Solana, Tendermint (chapter 36), UTXO
   native node RPC, the NFT providers, the coins-configuration fetch, the price
   sources of the simple market maker (chapter 42) and the other HTTP(S)
   services the node calls through that client.
4. *Electrum servers* over TCP and TLS (chapter 38 R38.6.4e item 1). TLS keeps
   the hostname for the server-name indication and the certificate check.
5. *lightwalletd servers* (chapter 39 R39.2.4). TLS keeps the hostname.

Sia, the WalletConnect relay, Lightning peer addresses and the statistics
method's address conversion still use the system resolver, whatever the key
says, until D45.6 is done.

R45.6B.10 **Metrics.** On an existing periodic tick, the node records four
gauges, each since the resolver was installed: `dns.lookups.count`,
`dns.failures.count`, `dns.stale_served.count` and `dns.last_known.entries`.

R45.6B.11 **One DNS library.** The node's hostname resolution uses one
maintained DNS library. The transport library's own DNS layer, and the
unmaintained resolver library it pulls in, are not built (chapter 28 R0b), and
the dependency policy bans them, so they cannot return unnoticed.

R45.6B.12 **Limits (informative).**
- Plain DNS on port 53 is visible to the networks it crosses, and they can
  block it. Encrypted DNS is deferred (D45.7).
- The names still appear in the TLS server-name indication and HTTP `Host`
  headers the node sends to the servers it connects to.
- In the default mode, Google Public DNS learns every name the node looks up.
- Seeds and servers given as IP addresses need no lookup in any mode.

R45.6B.13 **Upgrade note (informative).** Before this section, coin servers
and services were looked up through the operating system's resolver. On a
network that blocks Google Public DNS (some countries, many company networks,
captive portals), a node upgraded with the default can resolve no name, coin
servers included, until `dns_resolver` is set to `"system"` or to a reachable
server. The startup line (R45.6B.7) and the failure line (R45.6B.6) name the
key. Seeds were already resolved through Google Public DNS before this section.

## 45.7 No-Panic-On-Launch Obligation (security/robustness)

R45.7.1 No combination of unset, empty, or unexpected-type configuration fields
or environment variables shall be able to panic or crash the process during
startup. A genuinely unrecoverable launch input (R45.3.2, R45.3.5, R45.4.3,
R45.4.5, R45.5.3, R45.5.5, R45.6B.2) shall produce a clean, structured
configuration error and an orderly non-zero exit, not a panic or an aborted
backtrace.

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

T45.9 *Unknown coin-entry key.* A coins list in which one entry carries an
extra top-level key no consumer reads MUST start the daemon and MUST leave that
coin activatable, with the key ignored (R45.3.7). The same list with the entry's
ticker key removed, and with the order-book alias key set to a non-string, MUST
each refuse the launch cleanly instead. The test MUST fail if a strict schema is
introduced that rejects the extra key: that is the regression the rule exists to
prevent, and it would break every node older than the shared coins file it
reads. *(Cleared with R45.3.7 -- see the clearance statement above that rule in
§45.3 and the record in §45.12.)*

T45.10 *Arena limit decision (R45.6A.3, R45.6A.4).* A table-driven test of the
decision from (environment value, configuration value) to (call to make, line
to log) MUST cover at least: nothing set → limit `2` from the default; key `4`
→ limit `4` from the configuration; key `0` → no call, "no limit set"; key
`null` → `2`; environment `3` with key `4` → no call, "left to the
environment"; environment empty with key `4` → `4`; key `"2"` (string), `-1`,
`1.5`, `true`, `2147483648` and `[]` → warning and `2`.

T45.11 *Arena limit call (R45.6A.1, R45.6A.4).* On Linux with glibc a test MUST
call the allocator to set the limit it already has and MUST see success; it
MUST NOT change the arena limit of the test process to a lower value. A failed
call is exercised through the decision function, which returns the warning
without any process-wide effect.

T45.12 *Arena limit in a running daemon (lab).* On Linux with glibc the
allocator statistics of a started daemon MUST show at most 3 arenas with the
default; with the key `0` they MUST show as many arenas as before this section
existed; with `MALLOC_ARENA_MAX` set they MUST follow the variable whatever the
key says. The startup line of R45.6A.5 MUST appear once, naming the matching
source. A 40-minute run of the workload of R45.6A.7 with the default MUST show a
memory growth below 0.3 MB per minute from minute 10.

T45.13 *Invalid key never refuses the launch (R45.6A.4, R45.7.1).* Each invalid
value listed in T45.10 MUST start the daemon.

T45.14 *Startup path report.* With the configuration taken from the command line,
from the default-named file, and from an `MM_CONF_PATH` file, and with coins
inline versus from the default-named file versus from an `MM_COINS_PATH` file,
the report MUST state the actual source of each, MUST show relative paths as
absolute, MUST note an ignored `MM_COINS_PATH` when coins are inline, and MUST
NOT contain any file content or configuration value (R45.6.6). With a
`wss_certs` configuration, the report MUST NOT contain either file's path.

T45.15 *Version line.* A formatter test MUST show, for explicit arguments, the
banner and the second startup line in the forms of R45.6.7, including a
pre-release crate version, an `UNKNOWN` hash and an empty build datetime; a
second test MUST show that the banner produced for the running build contains
the crate version and the short hash of that build, and that the bare short hash
is unchanged as the value the `version` RPC method returns (R45.6.7).

T45.16 *Resolver key (R45.6B.2).* A table-driven test of the parsing MUST
cover at least: absent, `null` and `"google"` → Google Public DNS; `"system"` →
the system resolver; `["192.0.2.53"]`, `["192.0.2.53:5353"]`,
`["2001:db8::53"]`, `["[2001:db8::53]"]` and `["[2001:db8::53]:5353"]` → fixed servers with the
right ports, in the given order. Each of `"Google"`, `""`, `"192.0.2.53"` (a
string, not an array), `[]`, `[""]`, `["dns.example.com"]`, `["192.0.2.53:0"]`,
`["192.0.2.53:65536"]`, `[53]`, nine entries, `true`, `5` and `{}` MUST refuse
the launch with a clean configuration error naming the key, and MUST NOT panic
(R45.7.1).

T45.17 *Cache and last known answer (R45.6B.3 to R45.6B.5).* Against a DNS
server run by the test on a loopback address, in the fixed mode:
- an answer with a record lifetime of 0 is served from the cache for 30 s, and
  one with a lifetime of a day is asked again after 1 hour (a test clock or the
  configured bounds may stand in for real time);
- a name the hosts file of the test lists is answered without a query;
- an IP literal makes no query, and `localhost` and `a.localhost` resolve to
  loopback with no query;
- a truncated UDP answer is repeated over TCP, and a first server that does not
  answer is followed by the second;
- with the server stopped after a first answer, a lookup after that answer
  expired returns it and logs the WARN line once; with the server answering
  that the name does not exist, it does not; an answer that expired more than
  24 hours ago is not served;
- a lookup through a server that never answers fails within 15 s, and the
  failure line of R45.6B.6 appears once per name per hour.

T45.18 *Peer-to-peer resolution (R45.6B.9 item 1).* With a test resolver
installed, a dial of `/dns4/<name>/tcp/<port>` reaches the address the name
resolves to; a dial of a name with two addresses, the first unreachable,
connects to the second; a `/dnsaddr` dial fails with its own error; and a WSS
dial to a hostname completes its TLS check against that hostname.

T45.19 *Coin and HTTP(S) resolution (R45.6B.9 items 3 to 5).* With a test
resolver that maps a name to a loopback address, an HTTP(S) request through
the shared client, an Electrum connection (TCP and TLS) and a lightwalletd
connection to that name each reach the loopback server, and each lookup is
counted in `dns.lookups.count`. The TLS cases verify the certificate against
the name, not the address.

T45.20 *Installation (R45.6B.7, R45.6B.8).* A started node logs the startup
line of its mode exactly once. A lookup inside the node before the resolver is
installed fails a debug assertion. A lookup outside the node uses the system
resolver. A second node start in the same process replaces the resolver.

T45.21 *One DNS library (R45.6B.11).* The dependency policy check fails if the
transport library's DNS layer or the unmaintained resolver library returns to
the dependency tree.

T45.22 *Lab (R45.6B.2 to R45.6B.9).* With a lab DNS server that logs every
query and answers one seed name, one RIN Electrum host, one EVM node host and
one lightwalletd host, none of them in the hosts file:
1. default mode, Google unable to answer the lab names: the failure line
   appears, the dials and activations fail and retry, a seed given by IP
   connects, and the lab server logs no query;
2. `"system"` with the system pointed at the lab server: every site resolves
   and connects, and the lab server logs every name;
3. fixed `[<lab server>]`: as in case 2;
4. fixed `[<unreachable address>]`: every site fails and retries, no other
   server is asked, and the failure line appears once per name per hour;
5. last known answer: resolve through the lab server, stop it, and reconnect
   after the answers expired: the last known answer is served with its WARN
   line, and the connection succeeds;
6. an invalid value: the node refuses to start;
7. activation in mode 3 of a UTXO coin over Electrum, an EVM coin, a
   Tendermint coin, a shielded coin over lightwalletd and Solana.

## 45.9 Deferred Work

D45.1 A consolidated, typed startup-configuration schema (replacing ad-hoc
per-field access against a loosely-typed JSON document) is deferred. This
chapter binds the external tolerance contract; a typed loader may be introduced
later provided it preserves every outcome bound in §45.4–§45.6.

D45.2 A launch-time "configuration lint" that warns (without refusing) on
suspicious-but-tolerated inputs — for example an empty seed set on a non-seed
node, or a `gui` value that is present but not a string — is deferred as an
operator-experience enhancement.

D45.3 Sizing the worker threads of the daemon's three executors (two
multi-threaded runtimes and the async-std executor, each started with one
worker per core) explicitly, instead of one per core each, is deferred. It
would reduce threads and stacks on many-core hosts and is independent of
R45.6A.

D45.4 Measuring the arena limit under activation and swap workloads (start-up
time of a UTXO set, an EVM coin with tokens and a shielded coin; swap
completion; latency of the query methods under order updating) and re-checking
the default of `2` against them was done on 2026-11-03 (R45.6A.8): the default
stands. A shielded swap was not run and a swap set larger than one per pair and
arm was not measured.

D45.5 A 3-hour run at about 20 order updates per second, with and without the
limit, to learn whether the growth without the limit levels off, was done on
2026-11-03 as two 6.5-hour runs (R45.6A.8): both settings levelled off at that
load on the 8-core setting.

D45.6 Bringing the remaining lookups under R45.6B is deferred to the next
batch: Sia (its HTTP client sits in a sidecar library), the WalletConnect relay
connection, Lightning peer addresses (resolved today while a configuration
value is read, so the lookup must move to the moment of connecting) and the
statistics method's address conversion. Until then they use the system
resolver, and the operator documentation says so.

D45.7 Encrypted DNS (DNS over HTTPS or over TLS) for the Google and fixed modes
is deferred. It is a plan row without an effort estimate.

D45.8 *Library choice (decided 2026-11-03, maintainer decision).* The Google
and fixed modes are built on the message crate of the hickory DNS project
(`hickory-proto`, the 0.26 line) and the runtime's own UDP and TCP sockets, not
on its resolver crate. That resolver crate, from the 0.25 line on, depends on a cache crate
with files under Apache-2.0 only, which LEGAL/LICENSING-POLICY.md §4 does not
accept as a new dependency of the GPL-2.0-only combined work. Its 0.24 line has
no such dependency, but it carries an advisory that is fixed only in 0.26 and
gets no further fixes. The node therefore keeps its own cache (R45.6B.4), its
own reading of the hosts file (R45.6B.3 item 2), the server order and the TCP
retry on truncation (R45.6B.3) on top of that crate. Moving to the resolver
crate is deferred until it drops that dependency.

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
- Chapter 57 (EVM fee inputs) §57.5 — the per-coin gas-limit configuration
  blocks, whose own unknown-member tolerance (R57.5.1) and type-rejection
  behaviour are the nested-object instance of the coin-entry rule bound in
  R45.3.7.
- The public third-party coins-configuration file, consumed concurrently by
  nodes of many versions, which is the interoperability setting R45.3.7
  exists to protect.

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

> **Upstream divergence (informative).** The reference resolves seed names
> through Google Public DNS (chapter 28) and, as far as the source inherited at
> the anchor shows, every other name (coin servers, HTTP(S) services,
> lightwalletd) through the operating system's resolver. It has no setting for
> either. With `dns_resolver` absent, this node resolves every name
> through Google Public DNS, so coin servers and services diverge. With
> `"system"`, every name goes through the operating system's resolver, so coin
> servers match the reference and seeds diverge. No value reproduces the
> reference's mix; the maintainer chose one resolver for all names (§45.6B).

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
  Chapter 43, Chapter 57.
- *Section 45.6A (arena limit):* Reloaded-original, written from our own
  measurements (R45.6A.7) and the documentation of the system allocator; no
  reference material was consulted for it.
- *Addition R45.6.6 / T45.14 (startup path report) and R45.6.7 / T45.15 (version line):* two KDF Reloaded original
  operator-diagnostics requirements, authored independently from this
  repository's code and public documentation, without consulting the forbidden
  corpus.
- *Section 45.6B (hostname resolution):* Reloaded-original, written from the
  maintainer's decisions of 2026-11-02 and 2026-11-03, this repository's own
  source, the licenses and advisories of the candidate DNS crates, and the
  documentation of the DNS library and of Google Public DNS; no reference
  material was consulted for it. Its one statement about the reference, that
  seed names are resolved through Google Public DNS, restates chapter 28's
  reference note after R8c's rationale, item 4, which has passed the KDF Dirty
  Gate. The statement that the reference resolves other names through the
  operating system restates this repository's own source as inherited at the
  anchor; later reference releases were not examined for it.
- *Author of this chapter:* clean-room driving-spec working set.
- *Forbidden corpus:* consulted only to recover the externally observable
  startup tolerance contract (which unset/empty/unexpected inputs start the
  daemon, in what mode, and which refuse), and -- for R45.3.7 and T45.9, under
  the chapter-01 two-team workflow of §1.4A and at the GLEEC KDF public release
  designations `v3.0.0-beta` and `v2.6.0-beta` -- the coins-list entry's
  tolerance of an unrecognised key. No private identifiers, function
  bodies, control flow, or error/log string literals were carried across; all
  behaviour is restated as functional requirements. R45.3.7 and T45.9 have
  passed KDF Dirty Gate clearance, as their derivation record in §45.3 states
  and as the item below records; that item also records the one element of the
  attestation that is a gap.
- *Dirty Gate clearance (chapter 01 T1, R40, R43) for R45.3.7 and T45.9:* the
  forbidden-class-sourced material this item covers is exactly what the
  derivation record in §45.3 names: R45.3.7 and T45.9. The two
  external-reference entries added in §45.10 were inside the gated extent too,
  but they only name R45.3.7 and the public coins-configuration file and carry
  no fact beyond what that rule states, so they are not separately consulted
  material. The rule and the test were authored by the KDF Spec Reader under
  the two-team procedure of chapter 01 §1.4A and **passed the KDF Dirty Gate**
  on 2026-10-24: PASS, discretionary-expression measure 0.01, finding list
  empty, and accordingly no remedy class for any line or for the candidate as
  a whole. That was the first gate pass on this material; no earlier verdict
  for it exists or was relayed. The clearance is therefore **complete and not
  pending**, and R40 withholds nothing in it. This item is written on the
  clean side from the verdict, which reached the clean side as relayed text
  rather than as a gate-written file; that is record-keeping and adds no
  chapter content.

  **Only the named material was gated.** The rest of this chapter was not
  re-examined, and the first clause of the *Forbidden corpus* item above --
  the externally observable startup tolerance contract this chapter rests on
  elsewhere -- keeps the position it already had. This pass neither extends to
  it nor reopens it.

  *Which parts carry an inline pointer to this item, and why those.* R45.3.7
  carries the clearance statement at the point the material appears (R40), and
  T45.9 carries a one-line pointer back to it, because those two are the
  numbered, rule-bearing and test-bearing text an implementer codes from and
  they sit among R- and T-numbers that have stood in this chapter through
  earlier revisions, which is the case in which long-settled requirement and
  newly-cleared requirement are easiest to confuse. Nothing else carries a
  pointer, and that is a scope judgment rather than an oversight: the two
  §45.10 entries are external-reference citations that bind nothing and name
  R45.3.7 as the rule they serve, §45.11's divergence note was not part of the
  candidate, and every other section of this chapter rests on the position the
  item above already records. §45.12 is where the record lives and carries
  none.

  The three elements of the R39 scope attestation, carried forward here in
  corpus-free terms as chapter 01 T1 requires. One of the three is incomplete,
  and it is recorded as a gap rather than completed from elsewhere:

  - *Extent.* The forbidden-class material corresponding to the subject matter
    this chapter names in its own terms: the startup read of the resolved coin
    registry -- how a registry entry is consumed key by key by each consumer,
    what becomes of a top-level key no consumer reads, the equivalent case
    inside a recognised nested object, the distinction between an unrecognised
    key and a recognised key of the wrong type, and the whole-list read the
    order-matching subsystem performs at launch together with the two entry
    keys that read requires. Quantity examined: **one file** read in
    substance, together with token-level searches across the startup and
    order-matching file set, of the order of five files. Reference
    designation: the publicly published release designation `v3.0.0-beta`.
    **The record is no more precise than that designation** -- no post-anchor
    commit identifier, no branch state and no location within the compared
    material is given (chapter 01 R11 and the reference-version limit of
    §1.7).

    **The gap in this element, left visible rather than completed.** The
    derivation record in §45.3 establishes its facts at two designations and
    asserts that they agree. This clearance reached material at one of them
    only: no material at the `v2.6.0-beta` designation was available to the
    pass, so **the asserted agreement between the two designations is not
    attested here**. The element is incomplete rather than approximated, and
    chapter 01 T1 is satisfied by saying so -- a completed clearance statement
    carries the three elements **or** states that an element is not recorded.
    Nothing in this clearance supports the legacy-cross-check half of the
    derivation record: that half rests on the Spec Reader's record alone, and
    this item neither confirms nor contradicts it. Whether either designation
    is a publicly published release designation of the kind R11's carve-out
    admits is a separate question, checkable on the clean side against public
    release records, and is not answered here.

  - *Method.* **Combined.** Reading, together with a mechanical token
    comparison: the extent was fixed by a line-level comparison of this
    chapter against the branch tip so that only added and modified lines were
    judged, and every quoted token on an added line was then extracted
    mechanically and put through the forced-versus-free test individually. No
    aggregate match-density judgment was used or admitted. **This
    repository's own clean-room gate tooling under `tools/` was not applicable
    and was not run:** the added material embeds no code block and that
    tooling's test is defined over source-file bodies, so the instrument had
    no operand. The absence is stated so that it is visible rather than
    implied.

  - *Measure.* **0.01** on the discretionary-expression basis -- the same
    figure the leak report carries, not a second one -- read against the
    standard that a measure at or near zero passes only if, in addition, no
    added line falls into any leak category, one item of genuine discretionary
    expression being enough to fail a candidate however much legitimately
    dictated content surrounds it. The added material quotes no identifier at
    all from the compared surface: it states both load-bearing entry keys by
    their function rather than by name, so the measured set is effectively
    empty and the figure is a floor rather than the measurement of a residue.
    With the finding list empty, no added line was classified into any leak
    category -- no private identifier, no body or control-flow transcription,
    no error or log literal, no internal module tree, and no internal test
    identifier.
