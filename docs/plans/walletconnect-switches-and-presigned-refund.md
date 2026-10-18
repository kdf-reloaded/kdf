# Plan: WalletConnect availability switches, then pre-signed refund transactions

> **Status: planning only.** Two maintainer-approved designs for external-signer
> coins, in the order they should be built. Phase 1 is specified normatively in
> CRD ch.22 §22.9C (RS1–RS10, ACS1–ACS9, tracked as ch.22 D12–D14) and this
> document is its implementation plan, not its specification — where the two
> disagree, the chapter governs. Phase 2 has no chapter yet; this document is
> the whole of it, and the last section argues that it needs one before it is
> built.
>
> Created 2026-10-17. Nothing here is implemented.

## Why this exists

Two gaps in the same population — users whose signing key lives in a wallet we
do not control.

**Phase 1 (security).** WalletConnect is the only subsystem in the tree holding
a persistent, relay-mediated, externally-pairable session with key material at
rest, and that key material is in **plaintext** on disk under the default
`wc_session_persistence` value (ch.22 §22.5.1; the fix for *that* exposure is
the reserved encrypted at-rest format of **D8**, unimplemented). An operator who
learns of a relay or pairing vulnerability has no way to shut the subsystem off
without waiting for a release. They should.

**Phase 2 (availability).** A WalletConnect or hardware-wallet user whose wallet
is unavailable at refund time **cannot refund at all** — every refund path asks
the absent wallet for a signature. Pre-signing the refund while the user is
present and holding it closes that, and degrades to today's behaviour when it
cannot be used.

Phase 1 is containment and Phase 2 is a capability. Neither is a key-handling
fix, and there are **two** of those, answering two different threats:
**ch.22 D8** (the encrypted at-rest record) answers the plaintext session key
on disk, and **ch.22 R5 / D3** (zeroize-on-drop for the in-memory session-key
type) answers in-memory disclosure through core dumps, swap and freed-heap
reads. Zeroizing an in-memory key does nothing about a key at rest, so the two
are not substitutes and D8 is the one that closes the hole named above. **Both
should be done before either phase of this plan.** A switch protects the
operator who knows to flip it; fixing key handling protects everyone. That
ordering is stated in ch.22 §22.9C.1 and is repeated here so this plan is not
mistaken for the higher priority.

## Current state — verified in this repository, 2026-10-17

| Fact | Where |
| --- | --- |
| The WalletConnect subsystem pairs but cannot sign for a user: no activation policy, withdrawal route or swap path reaches a `WcCoinSigner` | ch.22 §22.0, D10; `mm2src/kdf_walletconnect/src/integration.rs` |
| `PrivKeyPolicy<T>` has no WalletConnect variant — but it is **not** the type activation requests use | `mm2src/coins/lp_coins_context.rs` (`KeyPair` / `HDWallet` / `Trezor` only) |
| The **EVM activation request** type *does* have one, and it is constructible from the wire today: `EthPrivKeyPolicy::WalletConnect { session_topic }`, adjacently tagged exactly as ch.22 RP8 describes, parsed in a real activation request and refused with a 400 by `unsupported_policy_error`, both covered by tests | `mm2src/coins_activation/src/eth_with_tokens_activation.rs` and its `tests.rs` |
| `TendermintActivationPolicy` has one variant, `PrivateKey(..)` | `mm2src/coins/tendermint/tendermint_types.rs` |
| A `walletconnect` config object already exists, carrying `project_id` (**required**) and `relay_address` (optional) | `mm2src/kdf_walletconnect/src/lib.rs`, `WalletConnectConfig::from_ctx` |
| `wc_session_persistence` is a separate **top-level** string with a strict parser. ch.22 §22.5.1 / R7 / D8 say a bad value "stops startup"; **it does not** — the parser is reachable only through `WalletConnectConfig::from_ctx` → `WalletConnectCtx::init` → the private lazy `wc_ctx` helper, so a bad value fails the **first WalletConnect RPC**. Nothing resolves this object at startup | `WcSessionPersistence::from_conf` (same file); `wc_ctx` in `mm2src/mm2_main/src/rpc/lp_commands/wallet_connect.rs`; ch.22 §22.9C.9 item 1 |
| The node *does* have a startup configuration-validation path that fails startup, and it is where ch.22 RS11 puts the new resolution | `lp_init` in `mm2src/mm2_main/src/lp_native_dex.rs` (netid range, netid support, `parse_passphrase_form`) |
| The handle's single outbound signing funnel is `WalletConnectCtx::send_session_request` | same file |
| The v1 UTXO HTLC refund branch is `<time_lock> OP_CHECKLOCKTIMEVERIFY OP_DROP <pub_0> OP_CHECKSIG` | `payment_script`, `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs` |
| The UTXO per-swap v2 **key** accessor errors for a policy with no local secret | `get_htlc_key_pair_v2` (same file) |
| The UTXO per-swap v2 **public-key** accessor is a separate function on a separate path — neither calls the other — and it **answers** for the one non-key-pair policy in the tree, returning the enabled HD address's public key | `try_derive_htlc_pubkey_v2` in `mm2src/coins/utxo/utxo_standard_swap_v2.rs` (its `Trezor` arm); this is why ch.22 RS6.2 / D15 adds a gate there |
| The Tendermint v1 HTLC key comes from the activated **local** secret | `get_htlc_key_pair` in `mm2src/coins/tendermint/tendermint_swap_ops.rs` → `activated_key_or_err` |
| An absent per-swap key is **not** an error in the v1 machine: it substitutes the node's own persistent public key | `derive_htlc_pubkeys` in `mm2src/mm2_main/src/lp_swap.rs` (`map_or(&node_pubkey, ..)`) |
| A Tendermint **token** forwards the per-swap key accessor to its platform coin, so it inherits the hazard above | `get_htlc_key_pair` in `mm2src/coins/tendermint/tendermint_token.rs` |

The last two rows together are why ch.22 §22.9C.5 refuses a `cosmos` swap switch
and ch.22 RS6.1 / D14 adds a gate instead: the Tendermint legacy HTLC path is
the one key-less path for which neither ch.22 RP11.1 nor ch.47 R47.5.13a states
a gate, and it fails *silently* rather than loudly.

---

## Phase 1 — the availability switches

Specification: ch.22 §22.9C. Steps below are the implementation of it.

### S1 — Resolved configuration type

Add a `WcAvailability` (name provisional) resolved-configuration type beside
`WcSessionPersistence` in `mm2src/kdf_walletconnect/src/lib.rs`, parsed from the
existing `walletconnect` object by a `from_conf`-shaped constructor. Follow that
type's *parser* pattern — strict parse, explanatory error naming the key and the
offending value, absent-means-default — but **not** its placement: it is
resolved lazily at the first WalletConnect RPC, which is the thing ch.22 RS11
forbids for this value. Resolve it from `lp_init` instead and cache it on the
context (see S1a).

- `enabled: bool`, default `true` (RS1).
- `families: Option<BTreeMap<CaipNamespace, FamilySwitches>>` — `None` means "no
  per-family policy expressed" (RS9), which is **not** the same as an empty map.
- `unnamed_family_policy: Allow | Deny`, default `Deny` (RS9).
- `FamilySwitches` carries only the operations ch.22 §22.9C.5 specifies **for
  that namespace**; an operation key for a combination with no switch is a parse
  error (RS10 case 5). A per-namespace deserializer, not one shared struct with
  optional fields, is what makes that fall out of the types rather than out of a
  hand-written check.
- Resolution must distinguish "absent" from "unparsable": absent yields the
  available default, unparsable stops startup or yields disabled, never a
  guessed value (RS3).

`project_id` and `relay_address` keep their current lenient handling. Do not
tighten them in this change; that would be a behaviour change for existing
operators and belongs to its own commit if it is wanted at all.

### S1a — The startup resolution point (ch.22 RS11)

This is new work and it has no analogue in the tree, so it is called out as its
own step rather than folded into S1.

- call the S1 resolver **once** from `lp_init` in
  `mm2src/mm2_main/src/lp_native_dex.rs`, next to `validate_netid_range` and
  `parse_passphrase_form`, which already fail startup on a bad configuration;
- store the resolved value on the context, and have all three enforcement
  points (activation, session establishment, chokepoint) read it from there;
- **exactly one parser** in the tree (ch.22 RS11(4), ACS13). The activation
  point in `coins_activation` must not re-parse `ctx.conf`: when the switch is
  off there is no handle to read the value from — ACS1 requires that — so a
  second, simpler `enabled`-only check in the activation crate is what an
  implementer will otherwise write, and two parsers for one security policy
  agree on the easy cases and diverge on the hard ones;
- `WalletConnectCtx::init` then reads the already-resolved value instead of
  parsing. Do **not** also move `wc_session_persistence`'s resolution in this
  step: that is a behaviour change to implemented, released material and is the
  maintainer decision recorded as ch.22 §22.9C.9 item 1. If that decision comes
  back as option (b), it is a one-line follow-up on top of S1a.

### S2 — The global switch at the session enforcement point

`WalletConnectCtx::init` is where the refusal goes — it is reached through the
per-context lazy init, so a disabled subsystem never builds a handle. The
*value* it reads is the one S1a resolved at startup; `init` does not resolve it
(ch.22 RS11(2), RS4.2):

- read the startup-resolved value **before** `open_storage` and before
  `relay_auth_token`;
- when disabled, return a structured refusal and log, at startup and on each
  refusal, that WalletConnect is disabled by configuration. Be aware of what
  this cannot be: ch.22 RP7 fixes the RPC surface at three `error_type` tokens
  and every initialisation failure collapses into one of them, so the refusal
  reaches an API caller as the same token and status code as a missing
  `project_id`. Do **not** add a fourth token to fix that — it is a
  pre-existing wire contract marked implemented, and the decision is recorded
  as ch.22 §22.9C.9 item 2. The distinction is for the operator, in the log and
  in the config they wrote, not for a GUI;
- three of the five public RPC handlers surface that error rather than an empty
  result (ACS2). The other two — session enumeration and session deletion —
  stay available in a **local-records-only** form (RS4.3, ACS12), which is the
  one piece of this step that needs a code route the tree has in no form today:
  a storage-only list and delete that do not go through `WalletConnectCtx::
  init`. Without it, an operator who disables after a key-exposure incident
  cannot delete the plaintext session keys they were worried about, because
  `wc_delete_session` is the only supported way to remove a persisted row
  (`drop_session` → `forget_session` → `storage.delete_session`) and it goes
  through the lazy `wc_ctx` helper that the disabled state refuses;
- `restore_sessions` is never reached, so no session is restored and no row is
  touched. Nothing deletes or rewrites rows (RS4.1), and ch.22 R8's
  unconditional load is untouched because the loader does not run — R8 binds
  what the loader does about `wc_session_persistence`, not whether the
  subsystem starts.

This enforcement point gates operations that are reachable **today**, which is
what keeps the global switch out of RS5's dormant category from its first
commit.

### S3 — The signer-selection chokepoint

One function, in the WalletConnect crate, through which every delegated signing
request passes (RS7). `send_session_request` is already the single outbound
funnel on the wire side, but the switch decision needs the *coin* and the
*operation*, which that function does not have. So:

- add one chokepoint that takes the coin's CAIP-2 namespace, the session topic,
  and an explicit `WcOperation { Withdraw, Swap, Activation, Unclassified }`
  (RS7.1) — supplied by the caller, never inferred. `Unclassified` is the
  residual variant and the chokepoint **refuses** it unconditionally (RS7.3):
  it is not a catch-all that passes on the global switch. `experimental::
  staking::delegate` and its siblings are the concrete reason — they are live,
  fund-moving routes that a catch-all would let through while the operator's
  `cosmos.withdraw: false` reported protection it was not giving;
- it evaluates: global switch → namespace lookup → `unnamed_family_policy` →
  per-operation switch → `unnamed_operation_policy`, and on a permit it
  **performs the signing call itself**, returning the signature / signed
  transaction / transaction hash. It does **not** return the signer: handing the
  signer back is what lets the signing method be invoked outside the
  chokepoint, which is the property RS7.2 denies (RS7);
- every family's `wc_integration.rs` signing entry point goes through it;
- enforce structurally (RS7.2), and note that **Rust visibility cannot do this**
  — `WcCoinSigner` is a `pub` trait of `kdf_walletconnect` implemented in
  `coins`, and a trait method's visibility is the trait's visibility. Use a
  **capability witness**: a token type in the chokepoint's module with a private
  constructor, not `Clone`/`Copy`/`Default`, taken by value by every restricted
  method. Outside code can name the method and cannot build the argument.
- the witness goes on the **funnel**, not only the trait. `send_session_request`
  is `pub` and takes an arbitrary payload, and `sessions()` → `transport_for()`
  hands out the session symmetric key, so a trait-only restriction leaves the
  signing channel open to any holder of the handle. Both of those take the
  witness too; the subsystem's own non-signing lifecycle traffic (delete
  courtesy publish, ping, expiry extension, inbound decrypt) moves to
  crate-private accessors. **A review convention is not acceptable here** — the
  whole point of one chokepoint is that the property is checkable by reading one
  function instead of by enumerating call sites, and partial enforcement is
  indistinguishable from protection.

### S4 — The activation enforcement point

Per request shape, not wholesale — and **one of the three can be written now**.

- **EVM: now, with the global switch** (ch.22 RS2.1). The activation request
  type already carries `EthPrivKeyPolicy::WalletConnect { session_topic }`,
  already parses it from the wire, and already refuses it through
  `unsupported_policy_error`. The switch arm goes beside that one and **takes
  precedence** over it, so an operator who turned WalletConnect off is told
  that, not "not supported by this build". Its *refuses-when-off* half is
  testable immediately; its *proceeds-when-on* half is not, because the
  build-unsupported refusal fires either way until ch.22 D10's activation port
  lands, so ACS1's EVM row is half-discharged until then.
- **Tendermint and UTXO: with their activation arms.** Those request types
  genuinely have no WalletConnect variant (`TendermintActivationPolicy` has
  one variant, `PrivateKey(..)`), so a check there would be written against
  something that cannot be constructed. They land with ch.22 D10.

The earlier plan deferred all three on the premise that no WalletConnect key
source exists in the activation types. That premise is true of
`PrivKeyPolicy<T>` and false of the type the EVM activation route actually
uses, and deferring the one shape that can be written is what would leave
ch.22 RS2's "shall enforce at both" unsatisfied for no reason.

### S5 — Per-family switches, each behind its own operation

Per RS5.1, each switch ships in or after the change that makes its operation
reachable, never before:

| Switch | Ships with |
| --- | --- |
| `eip155.withdraw` | ch.22 D10's EVM withdrawal wiring |
| `cosmos.withdraw` | ch.22 D10's Tendermint withdrawal wiring |
| `bip122.swap` | ch.22 D10's UTXO legacy-swap wiring |
| `bip122.withdraw` | ch.22 D11's UTXO withdrawal wiring (coordinate with CRD ch.49) |

A switch whose operation slips out of a release slips with it. Shipping the
parser's acceptance of a key whose enforcement is not yet wired is the same
defect as shipping a dormant switch, only harder to see.

The chapter now agrees with that rather than contradicting it: ch.22 RS10 case
5 keys the accepted key set off the switches **this build enforces**, not the
switches §22.9C.5 specifies, so the parser's accepted set grows row by row with
this table. The earlier wording keyed it off the specification, which would
have required the first release to accept `{"bip122": {"withdraw": true}}`
with nothing behind it. Two consequences to implement deliberately: each row of
this table extends the parser in the same change that wires its enforcement
(ch.22 RS5.2), and a configuration file written for a later release fails to
load on an earlier one — the same deliberate trade as strict parsing generally,
in a second form. ACS6 tests both directions.

### S6 — The RS6.1 / D14 Tendermint gate, and the RS6.2 / D15 UTXO gate

Independent of the switches and worth doing on its own: refuse a key-less
Tendermint coin entry to the legacy HTLC paths at order placement / swap start
with a structured unsupported-operation error, on the same terms as ch.22
RP11.1 and ch.47 R47.5.13a. Today the absent key is not an error — the v1
machine substitutes the node's own persistent public key, which announces a key
the paired wallet cannot sign with. That is a silent wrong answer, and it is
the reason `cosmos` gets a gate instead of a switch. The gate must cover a
Tendermint **token** as well: a token forwards the accessor to its platform
coin, so a test written against the platform coin alone does not prove it
(ACS9).

The UTXO counterpart (ch.22 RS6.2 / D15) is the same shape one path over and
belongs in the same work: the swap-protocol-v2 per-swap **public-key** accessor
answers for a non-key-pair policy today, so the WalletConnect arm added there
must refuse rather than mirror the existing arm (ACS10). This one is *not*
independently shippable — there is nothing to gate until the policy arm exists
— so it lands with the change that introduces the arm, not before.

### S7 — Documentation

- The per-setting documentation goes where `wc_session_persistence` is already
  documented, with a **"Compatibility: GLEEC KDF has no equivalent"**-style note
  that makes clear it is not a divergence switch.
- **No** `GLEEC_COMPATIBILITY.md` row and **no** `COMPAT_SWITCHES.md` entry.
  These are not compatibility switches: they describe no divergence from GLEEC,
  there is no GLEEC-compatible value to recover, and their lifecycle is the
  threat's, not a compatibility window's (ch.22 §22.9C.2).
- The default-side rule does **not** apply, and the per-setting documentation
  should not claim it does. `docs/COMPAT_SWITCHES.md` records the rule under
  *"The default-side rule"* and, under *"What this convention is not"*, states
  that it does not reach operator-facing security controls: such a control's
  default is a security decision for the feature that owns it. ch.22 §22.9C.2
  owns this one and settles it as *available* because an operator who
  configures nothing must see no change. The operator-facing docs should cite
  §22.9C.2 and not re-derive anything.
- **Open: whether operator security controls get a document of their own.** My
  recommendation is **not yet**. One control does not need a central chapter,
  and creating `docs/OPERATOR_SECURITY_CONTROLS.md` speculatively would invite
  exactly the mistake §22.9C.2 guards against — a reader treating it as the
  compatibility chapters' sibling. Create it when there is a **third**
  operator-facing security control to list (the existing candidates being
  `allow_insecure_key_export` and whatever the D8 encrypted-at-rest format
  exposes), and at that point move all of them into it at once.

### Tests (Phase 1)

ACS1–ACS9 of ch.22 §22.9C.8 are the acceptance gate. The ones that carry the
weight:

- **ACS4 is per switch, not per section** — four pairs of assertions, one pair
  per switch: operation refused with its switch off, operation proceeds with it
  on, no other switch changed. This is the test that discharges the
  no-dormant-switches requirement (RS5), and it is the reason each switch is
  sequenced behind its operation: a switch whose "proceeds" half cannot be
  written does not gate anything.
- **ACS3** — resolver unit tests: absent block yields *available*; an
  unparsable block never yields *available* (it yields *disabled* or an error
  that stops startup — here, the latter, per open decision 1). The two cases
  must not be conflated in either direction.
- **ACS6 and ACS13** — one startup-error test per RS10 case, driven through
  **startup** and not through the resolver alone, because the defect being
  guarded against is a misconfigured node running until somebody calls a
  WalletConnect RPC. In particular `{"eip155": {"swap": false}}` is
  **rejected** rather than silently accepted. ACS13 adds the mechanical
  one-parser check.
- **ACS7** — the mechanical check that no second call site reaches a
  `WcCoinSigner` signing method.
- **ACS8** — with no new keys configured, nothing changes. This one is cheap and
  is the regression gate for the whole phase.

The `WcSessionPersistence` unit tests in `mm2src/kdf_walletconnect/src/lib.rs`
are the model for the parser tests; follow their shape.

### Migration and compatibility (Phase 1)

**There is nothing to break.** The default is "available", absent configuration
resolves to today's behaviour for every family and operation, and the new keys
nest into an existing object without changing either key already in it. No
on-disk format changes, no wire format changes, no RPC shape changes, no
persisted session is rewritten or deleted. An operator who upgrades and
configures nothing cannot tell the feature landed — ACS8 asserts exactly that.

Two deliberate asymmetries worth repeating to whoever implements it, because
both look like bugs:

1. The default is *available* but the failure mode is *closed* (RS1 vs RS3).
   Absence of configuration is a determinate answer; a configuration error is
   not an answer at all.
2. No `families` map means everything available; a `families` map that omits a
   family means that family is **denied** (RS9). Writing a map at all is an
   allowlist, which is what stops a family added in a later release from
   becoming available by omission in an existing operator's config.

---

## Phase 2 — pre-signed refund transactions

**Maintainer-approved design; no CRD chapter yet.** Motivation: a WalletConnect
or hardware-wallet user whose wallet is unavailable at refund time currently
cannot refund at all. Pre-sign the refund while the user is present, hold it,
and broadcast it if the swap fails — **with graceful degradation to today's
behaviour (ask the user to sign a fresh refund) whenever the pre-signed one
cannot be used.**

That fallback is the whole reason this is worth doing: the attempt either works
or costs nothing, so it is **weakly dominant** over the status quo. Every design
decision below exists to keep it that way, because a pre-signed refund that can
make things *worse* than today loses the only argument in its favour.

### P0 — The load-bearing premise

**The v1 UTXO HTLC refund branch is**

```
<time_lock> OP_CHECKLOCKTIMEVERIFY OP_DROP <pub_0> OP_CHECKSIG
```

(verified: `payment_script` in `mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`).

`OP_CHECKLOCKTIMEVERIFY` is **consensus-enforced**. A pre-signed refund is
therefore *not a valid transaction* until the timelock passes: no node will
accept it, no miner can mine it, and holding it confers **no early-refund
power**. The artifact is inert until the moment its owner was already entitled
to the funds.

**This is the entire safety argument.** Everything else in Phase 2 is
engineering around it.

**Corollary, and it must be written into the chapter as a requirement:**
pre-signing anything whose spend condition is **not** consensus-enforced would
be a **bearer instrument** — whoever holds it can spend, immediately — and the
safety argument above would collapse completely. This must never be built by
accident. Concretely, that rules out pre-signing the *spend* (claim) side,
anything gated only by application-level logic, and any future HTLC form whose
refund branch is enforced by a contract's own checks rather than by consensus
unless that contract's time check is itself consensus-equivalent. Any new
pre-sign site must state which consensus rule makes its artifact inert, and a
reviewer who cannot name that rule must reject the change.

### P1 — What a leak actually costs

The artifact pays **only to its owner**, from **one specific outpoint**. If it
leaks, the holder's maximum power is to return the owner's funds to the owner,
after the owner was already entitled to them. That is not a fund-loss exposure.

The real cost of a leak is **privacy**: the existence of a swap, its amounts,
its addresses, and its timelock. That argues for encrypting the artifact at
rest rather than for refusing to store it — and note the ordering dependency,
because the encrypted-at-rest work for ch.22 D8 is plausibly the same
machinery. Do not store it in plaintext merely because losing it is harmless;
the thing it reveals is not harmless.

### P2 — The one way this can be worse than today

**The stuck-in-mempool middle state.** A refund broadcast at too low a feerate
sits unmined and locks the output. While the wallet is absent it cannot be
fee-bumped at all:

- RBF needs a **new signature**, and while the wallet is away there is nobody
  to produce one;
- CPFP needs a signature on the **child** — same wallet, same problem.

So during the absence the user is unable to refund *and* unable to retry, which
is worse than being asked to sign a fresh refund. This is the only state in the
design that breaks weak dominance. Three measures are specified below; the
first reduces how often the state is entered, and the second and third decide
whether entering it is recoverable.

**P2.1 — Gate the broadcast on a pre-flight feerate (or gas price) check.**

> If the held transaction would not be mined at current conditions, **do not
> broadcast it** — skip straight to asking the user for a fresh refund. A
> rejected broadcast is free; a stuck one is not.

The check must be a precondition of broadcast, not a reaction to a stuck
transaction. But be exact about what it buys: **it narrows the window, it does
not close it.** Feerates rise *after* broadcast, and once the transaction is in
mempools the fee is committed. A gate evaluated at time T cannot bind
conditions at T+1, so "designed out" is the wrong claim and this plan should
not make it. What the gate delivers is a large reduction in the probability of
entering the unrecoverable state — large because the common cause is a stale
artifact signed at a quiet moment and broadcast during a fee spike, which is
exactly what the gate catches.

**P2.2 — Sign the pre-signed refund with an RBF-signalling nSequence.** This is
free at signing time and it converts "unrecoverable" into "fixable the moment
the wallet is back", which is the difference between breaking weak dominance
and merely degrading. **Do not inherit the surrounding code's value.** The
existing refund paths sign with `SEQUENCE_FINAL - 1` = `0xfffffffe`
(`mm2src/coins/utxo/utxo_common/utxo_common_swap.rs`, five call sites;
`SEQUENCE_FINAL` is `0xffff_ffff` in `mm2src/kdf_chain/src/constants_.rs`).
That value was chosen to satisfy CLTV, which needs only a non-final nSequence,
and it is **not** BIP-125 opt-in RBF: BIP-125's explicit signal requires an
input with nSequence **strictly less than** `0xffffffff - 1`, and `0xfffffffe`
is not less than `0xfffffffe`. A refund pre-signed on this machinery is
therefore unreplaceable even after the wallet returns, until mempool eviction —
conventionally around two weeks of the user being told their funds are stuck
while the instrument that could free them sits in their returned wallet,
useless.

So the pre-signed refund's nSequence is a **decision, not an inheritance**, and
this plan makes it: `0xffff_fffd`. It satisfies all three constraints at once —
non-final, so CLTV is enforced and P0's inertness argument is untouched (the
RBF flag has no bearing on CLTV); strictly below `0xfffffffe`, so BIP-125
explicit signalling applies; and bit 31 set, so BIP-68 relative locktime stays
disabled and no unintended sequence lock is created on a version-2
transaction. Nothing in the tree validates a refund's nSequence, so the value
is genuinely free to choose here — the one existing sequence assertion is on a
v2 taker-payment-**spend** preimage, not a refund.

**P2.3 — Consider asking the wallet for a ladder of refunds, not one.** The
design goal is "ask the wallet once, while the user is present", and that
permits **N** signatures in one sitting as easily as one: several refunds over
the same outpoint at escalating feerates, all RBF-signalling, broadcast in
ascending order as conditions require. This is the measure that actually
*designs the stuck state out* rather than gating or recovering from it, because
a higher rung is always available without the wallet. The costs are real and
are why it is an open decision rather than a specified requirement: more
wallet-side prompting to explain to the user, a storage and
invalidation-tracking multiplier on P1's privacy argument, and a rung-selection
policy that has to be written. Recorded as **open decision 9**.

### P3 — Detect failure and escalate promptly

A retry loop that believes it has broadcast something turns a strictly-better
optimisation into a **delay**. The broadcast attempt needs an explicit success
criterion (accepted into a mempool, then mined within a bounded window), a
bounded number of attempts, and a prompt transition to the
ask-the-user-to-sign-a-fresh-refund path on failure. Failure must be loud in
the swap's event history: a user who is told "refunding" while nothing is
happening is worse off than one who is told "please sign".

### P4 — Non-SegWit coins: pre-sign after the funding transaction confirms

For a non-SegWit coin the funding txid is malleable until it is mined, so a
refund pre-signed at setup can be invalidated by a change in the transaction it
spends. **Pre-sign once the funding transaction confirms**, not at setup. Once
the txid is final, malleability is moot, and the goal — ask the wallet once,
while the user is present — still holds, because the user is still at the
screen watching the payment confirm. This is a sequencing constraint, not an
obstacle.

### P5 — EVM

Per participant per chain it is refund **or** claim, never both: the party who
**funds** a chain refunds on it, and the party who **receives** claims on it,
which is the *other* chain. So there is **no refund/claim nonce conflict**,
except in one case: **both legs on the same EVM chain** (ETH against an ERC-20
on the same chain, for instance). That case needs its own handling or its own
refusal.

Holding an unbroadcast pre-signed transaction does **not** stall the account:
nothing is pending at that nonce, so ordinary wallet use proceeds normally. The
real fragility is the mirror of that — **any other use of that nonce
invalidates the held transaction**, including the user's own activity in a
general-purpose wallet, which is exactly this feature's population. A held EVM
refund should therefore be treated as best-effort from the moment it is signed,
and its invalidation must be detected (P3) rather than discovered at refund
time.

Additionally, **concurrent swaps on the same chain and account couple by nonce
ordering.** If two held refunds sit at N and N+1 and the first swap *succeeds*,
nonce N is never consumed, and N+1 cannot confirm until something fills N —
which needs the absent wallet. Mitigation: **pre-sign only for the
lowest-nonce outstanding swap on a chain**, or accept the degradation
knowingly and document it. Do not leave the choice implicit.

**Sequence EVM second.** It is safe — the nonce issues degrade to today's
behaviour, they do not create a hazard — but it will often not fire, so it is a
**cost/benefit** question rather than a safety one. UTXO first, where the
artifact is robust and the win is clean.

### P6 — Correcting a misconception that affects urgency

**An absent user does not lose coins.** The refund branch has **no upper
bound**: once the timelock passes, the output stays spendable by the refund key
indefinitely. A user whose wallet is offline for a week, a month, or a year can
still refund when it comes back.

The residual risk is the **counterparty claiming with the secret** in the
interim — which requires them to actually hold the secret, i.e. the swap
reached the point where it was revealed. So the fallback being slow is **far
cheaper than it first appears**, and this feature is a convenience and
availability improvement rather than a loss-prevention fix. Anyone writing the
chapter or the user-facing copy should say so, because "you could lose your
coins" is both false and the kind of false that gets a worse design shipped
faster.

### P7 — Open question, not a recommendation

Because the artifact is **harmless to leak** (P1), it could in principle be
handed to a **watcher** to broadcast on the user's behalf, which would close
the wallet-offline-at-expiry gap entirely rather than narrowing it. The usual
objection to giving a watcher signed material does not apply here: the watcher
could only return the user's funds to the user, after the user was entitled to
them, and only after a consensus-enforced timelock.

That is not the same as it being a good idea. Whether a **watcher should ever
decide to refund** is a separate question with its own answer — it interacts
with ch.09's reward model, with ch.22 RP10 (which today reports a key-less coin
*unsupported* by the watcher subsystem precisely because no detached signature
can be produced for it — this would be the first material that changes that
premise), and with whether a refund the user did not ask for at that moment is
ever unwelcome. **Recorded as an open question. Do not build it as part of
Phase 2.**

### Does Phase 2 deserve its own CRD chapter?

**Yes — once it grows beyond this document, and before it is implemented.** My
reasons:

1. It touches the **swap protocol**, not only WalletConnect. The artifact is
   created in the v1 state machine's lifecycle, persisted in swap storage,
   consumed on the refund path, and bears on the watcher model. ch.22 is the
   WalletConnect chapter and cannot bind requirements in ch.51 (v1 state
   machine), ch.09 (watcher) and ch.49 (withdrawal/task paths); a plan cannot
   bind requirements anywhere.
2. The safety premise **must be binding**, not advisory. P0's corollary — never
   pre-sign a spend condition that is not consensus-enforced — is exactly the
   kind of invariant that belongs in an R-numbered requirement with an
   acceptance criterion, because the failure mode is a bearer instrument and
   the failure is silent. A bullet in a plan does not survive three refactors;
   a CRD requirement with a test does. **P2.2's nSequence belongs in the same
   class and for the same reason**: the failure is silent, the inherited
   default is the wrong one, and an implementer working inside
   `utxo_common_swap.rs` will reach for `SEQUENCE_FINAL - 1` without noticing
   it is a decision. An R-numbered requirement with an acceptance criterion
   asserting the signalled value on the pre-signed artifact is what stops that.
3. P2's broadcast gate and P3's failure detection are **conformance
   properties** with testable acceptance criteria, which is what a CRD chapter
   is for.

Shape I would expect: a new chapter for pre-signed refund artifacts across
families (not a subsection of ch.22, since it is not WalletConnect-specific —
hardware wallets are the same population and the same design), cross-referenced
from ch.22 §22.9B, ch.51 and ch.09. It is **clean-room-original design** on the
same footing as ch.22 §22.9C: it derives from no forbidden-class input, so it
needs no Spec Reader pass and no Dirty Gate, and its provenance footer should
say so in those terms. **If writing it ever seems to require knowing what
upstream does, stop** — that would change the clean-room handling of the whole
chapter.

---

## Sequencing

0. **ch.22 D8 — the encrypted at-rest session record** — and **ch.22 R5 / D3 —
   zeroize-on-drop for session-key material.** Neither is part of this plan and
   both are ahead of all of it. Stated here so the ordering is not lost. D8 is
   the one that closes the plaintext-on-disk exposure; R5 / D3 closes in-memory
   disclosure. Which of the two comes first is a maintainer choice between two
   threats, not something this plan decides.
1. **Phase 1 S1, S1a, S2, S3, S6.** The resolved config type, the startup
   resolution point, the global switch at the session enforcement point, the
   chokepoint, and the independent Tendermint gate. S1a is a prerequisite of
   S2, not a refinement of it: the switch must be resolved before the handle is
   built, because when it is off no handle is built at all. S6's Tendermint
   half is independently shippable and independently useful; its UTXO half
   (RS6.2 / D15) is not, and goes with the policy arm.
2. **Phase 1 S4–S5**, each piece with the ch.22 D10 / D11 wiring that makes its
   operation reachable. Not before.
3. **Phase 1 S7** documentation, with the phase it documents.
4. **Phase 2 chapter** written and reviewed before Phase 2 code.
5. **Phase 2 UTXO** (P0–P4). SegWit first; non-SegWit adds P4's
   post-confirmation timing.
6. **Phase 2 EVM** (P5), as a cost/benefit decision, not a safety one.

## Risks

| Risk | Mitigation |
| --- | --- |
| Disabling leaves the operator unable to delete the at-rest session keys the incident was about | RS4.3 / ACS12 — storage-only enumeration and deletion stay available while disabled, and §22.9C.1 states which part of the exposure the switch does and does not remove |
| A per-family switch ships before its operation is reachable and is dormant | RS5.1 and the S5 table; ACS4's "proceeds" half cannot be written for a dormant switch, so the test matrix catches it |
| Enforcement leaks to a second call site over time | RS7.2 / ACS7 — a capability witness the compiler enforces, over the funnel as well as the trait, not a review convention |
| A check passes on `WcCoinSigner` while `send_session_request` stays open to every holder of the handle | RS7.2's funnel clause and ACS7's three compile-fail cases; a trait-only check certifies a property that does not protect the channel |
| A fund-moving route reaches the wallet through the chokepoint's residual operation class while a per-family switch reports it blocked | RS7.3 — the residual class is refused unconditionally, and ACS11(b)'s mechanical call-site enumeration is reviewable against the fund-moving RPC routes |
| Strict config parsing breaks a config shared across node versions | Accepted deliberately and documented: a security switch the operator believes they set is worth more than a config that loads everywhere |
| Phase 2 pre-signs something not consensus-enforced | P0's corollary as a binding CRD requirement with a named consensus rule per pre-sign site |
| Phase 2 broadcast gets stuck in the mempool | P2.1's pre-flight feerate gate as a **precondition** of broadcast. This **narrows** the window and does not close it — feerates rise after broadcast — so it is not a retired risk |
| A stuck pre-signed refund is unreplaceable even after the wallet returns | P2.2 — an RBF-signalling nSequence (`0xffff_fffd`) chosen deliberately instead of inheriting `SEQUENCE_FINAL - 1`, which is **not** BIP-125 opt-in. Open decision 9 (P2.3's fee ladder) would remove the need to wait for the wallet at all |
| Phase 2 held EVM refund silently invalidated by the user's own wallet activity | P5 best-effort framing plus P3 detection; prefer lowest-nonce-only |
| Phase 2 retry loop silently delays a refund | P3's bounded attempts and loud escalation |
| The leaked artifact's privacy cost is dismissed because it is not a fund risk | P1; encrypt at rest, plausibly reusing ch.22 D8's machinery |

## Out of scope

- **ch.22 D8** (encrypted at-rest format — the fix for the plaintext session key
  on disk) and **ch.22 R5 / D3** (zeroize-on-drop — the fix for in-memory
  disclosure). Higher priority, separate work, two different threats, explicitly
  not addressed by either phase.
- **ch.22's existing implementation-status claims.** The accuracy of the chapter's
  `Port status. Implemented.` / `STATUS. Implemented.` lines against unmet AC1
  and R5 / D3 is a separate, already-tracked question and is untouched here.
- **A runtime or RPC-level kill switch** (ch.22 D13). Needs its own
  authorization story first. Note that RS4.3's storage-only enumeration and
  deletion are *not* this: they cannot change the switch, cannot start the
  subsystem, and emit nothing on the relay.
- **A per-family activation switch** (ch.22 D13).
- **Giving a watcher the pre-signed artifact** (P7). Open question only.
- **Pre-signing the claim/spend side.** Forbidden by P0's corollary, not merely
  deferred.

## Open decisions

1. **Phase 1 fail-closed form: closed — refuse startup.** RS3 permits either
   refusing startup or starting disabled, and ch.22 RS10 / RS11 settle it as
   refuse-startup for the keys this section introduces; ACS3 is written to the
   property ("never yields *available*") so it is satisfiable either way.
   Recorded as closed because two documents disagreeing about which arm is
   normative is how an implementer ends up writing a test that asserts
   something other than what it claims. Note that the original justification
   for this choice — "matching `wc_session_persistence`'s existing treatment of
   a bad value" — was **false**: that value does not refuse startup today (see
   *Current state* and ch.22 §22.9C.9 item 1). The decision stands on RS3's
   fail-closed reasoning and on loudness, not on that precedent.
2. **Phase 1 chokepoint home:** in `kdf_walletconnect` (keeps the subsystem's
   policy with the subsystem, but the crate must then know the operation
   taxonomy) or in `coins` (knows the operations, but spreads the policy). The
   `WcOperation` enum makes either workable. RS7.2's capability witness is
   cross-crate by construction, so it no longer argues for one home over the
   other — but the funnel half of RS7.2 does: `send_session_request` and the
   session-key accessor live in `kdf_walletconnect`, and the witness type has
   to be visible to them, so putting the chokepoint there keeps the whole
   restricted surface in one crate. **Recommendation: `kdf_walletconnect`.**
3. **Phase 1 `unnamed_family_policy` spelling and placement:** a sibling key, as
   specified, versus a reserved `default` entry inside `families`. Specified as
   a sibling because a CAIP-2 namespace could in principle be spelled
   `default`; revisit only if an operator-facing reason appears.
4. **Phase 2 storage:** does the artifact live in the swap's own storage or in a
   separate store? Encryption at rest (P1) may decide it.
5. **Phase 2 same-chain two-leg EVM swaps** (P5): handled, or refused with a
   structured error? Refusing is honest and cheap; handling needs a nonce
   reservation scheme.
6. **Phase 2 lowest-nonce-only** (P5): adopt as a rule, or pre-sign every
   outstanding swap and accept the N / N+1 coupling knowingly?
7. **Phase 2 chapter number and scope:** a new chapter for pre-signed refund
   artifacts across families, versus a §22.9D. Argued above for the former.
8. **S7:** when (not whether) `docs/OPERATOR_SECURITY_CONTROLS.md` is created.
   Proposed trigger: a third control.
9. **Phase 2 fee ladder** (P2.3): one pre-signed refund, or N at escalating
   feerates signed in the same user session? The ladder is the only measure
   that removes the stuck state rather than narrowing it (P2.1) or making it
   recoverable once the wallet returns (P2.2), and it costs nothing extra in
   *safety* — every rung is inert under the same CLTV argument as the first.
   It costs user-visible prompting, a storage and invalidation multiplier on
   P1's privacy concern, and a rung-selection policy. **Not** decided here.
   P2.2 is decided (RBF-signalling nSequence); this is the question of whether
   P2.2 is enough.

## References

- CRD [ch.22 §22.9C](../reloaded-rewrite/22-walletconnect-v2.md) — the Phase 1
  specification (RS1–RS10, ACS1–ACS9); §22.9B (RP8–RP13) for reachability;
  §22.5.1 / R5 / R7 / R8 / D3 / D8 for the at-rest and persistence contract;
  D12–D14 for the tracked work.
- CRD [ch.47](../reloaded-rewrite/47-metamask-integration.md) R47.5.13 /
  R47.5.13a — why the EVM-family HTLC paths are gated for every non-keypair
  policy, and the carve-out that makes UTXO the exception.
- CRD [ch.09](../reloaded-rewrite/09-watcher-reward-infrastructure.md) — the
  watcher model P7 would have to argue with.
- CRD [ch.49](../reloaded-rewrite/49-withdrawal-task-path.md) — coordinate the
  `bip122` withdrawal wiring (ch.22 D11) with the withdrawal task path.
- CRD [ch.51](../reloaded-rewrite/51-legacy-v1-swap-state-machine.md) — the v1
  machine Phase 2's artifact lives inside; R63 for the 33-byte
  `derive_htlc_pubkey` contract the Tendermint gate (S6) interacts with.
- [`COMPAT_SWITCHES.md`](../COMPAT_SWITCHES.md) and
  [`GLEEC_COMPATIBILITY.md`](../GLEEC_COMPATIBILITY.md) — the convention the
  Phase 1 switches are deliberately **not** part of. Its
  [default-side rule](../COMPAT_SWITCHES.md#the-default-side-rule) and its
  *"What this convention is not"* carve-out for operator-facing security
  controls are what ch.22 §22.9C.2 cites. **Both land with PR #45**, which is
  open and unmerged at the time of writing; the citation resolves when it
  merges.
- [`walletconnect-fork-upgrade.md`](walletconnect-fork-upgrade.md) — the
  subsystem's dependency work; unrelated to either phase but the same code.
