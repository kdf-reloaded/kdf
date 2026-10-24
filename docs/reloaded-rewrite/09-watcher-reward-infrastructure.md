# Chapter 09 -- Third-Party Swap-Watcher Infrastructure

**Status:** driving-spec

> **One-sentence claim:** the codebase shall carry a
> third-party "watcher" substrate that lets any node on the
> gossip overlay (a) subscribe to a per-coin watcher topic,
> (b) receive a signed envelope from a taker carrying
> precomputed spend and refund preimages, and (c) on the
> taker's behalf broadcast the appropriate preimage if the
> taker disappears mid-swap, so that the maker is paid and
> the taker is refunded without the taker process being
> online; watcher operation requires no private-key material
> from either party.

## 9.0 Executive Summary

The atomic-swap protocol bound by this codebase is strictly
two-party at the cryptographic layer: a maker and a taker
exchange hash-time-locked-contract transactions, and if
either side disappears mid-swap the remaining party must
wait out the timelock and broadcast the refund itself.
Without an additional substrate, a taker that disconnects
between sending its payment and observing the maker's spend
imposes a multi-hour delay on the maker before the
maker-payment can be refunded.

The watcher substrate closes that gap by allowing any
volunteering node to monitor a specific in-flight swap and
to broadcast the taker's own precomputed preimages on the
taker's behalf. The substrate is **trustless**: the taker
publishes signed transactions before going offline; the
watcher only ever rebroadcasts them. A watcher does not
need (and cannot derive) any signing key from either party.

Two surfaces are bound:

- A wire surface: a per-coin gossip topic, a versioned
  message envelope, a signed-publish authentication
  contract, and the watcher's broadcast triggers.
- A coin-trait surface: a single per-coin opt-in predicate
  declaring whether the coin family permits third-party
  rebroadcast of its swap transactions.

A separate placeholder surface, the per-refund
`watcher_reward` boolean (§9.9), is present at the
coin-trait level but **inactive** in the codebase at the
time of writing: every construction site sets the field to
`false`, so no on-chain reward is ever owed to a watcher.
The watcher service in this codebase is voluntary and
unpaid.

## 9.1 Subsystem Shape

The substrate has the following behavioural regions:

| Region                        | Effect                                       |
|-------------------------------|----------------------------------------------|
| Per-coin gossip topic         | One topic per taker-coin ticker              |
| Watcher message envelope      | Versioned enum; presently one variant        |
| Taker-published payload       | Preimages + per-swap context                 |
| Signed-publish authentication | Sender authenticated via the gossip key      |
| Watcher state machine         | Six states; compile-time fixed transitions   |
| Per-node de-duplication       | RAII lock keyed by the taker-fee transaction |
| Coin-trait opt-in             | Single per-coin predicate                    |
| Inactive reward field         | Boolean placeholder, always `false` today    |

## 9.2 Wire Topic

R1. **Per-coin watcher topic.** The substrate shall expose
    one gossip topic per taker-coin ticker, derived from a
    fixed prefix and the ticker:
    `<watcher-prefix>/<TICKER>`.

R2. **Bound prefix.** The watcher topic prefix is the
    eight-byte ASCII string `swpwtchr`. The full topic for
    Bitcoin would therefore be `swpwtchr/BTC`. The prefix
    is a wire fact and shall not change without coordinated
    update across every participating node.

R3. **Subscription model.** A node shall subscribe to the
    watcher topic for a given ticker when, and only when,
    the corresponding coin is activated locally on that
    node. This gives operators per-coin opt-out by simply
    declining to activate the coin.

## 9.3 Message Envelope

R4. **Versioned envelope.** The watcher payload shall be
    carried inside a versioned message enum so that
    additional watcher message shapes can be added without
    a topic split. At the time of writing exactly one
    variant exists, carrying the taker-published payload.

R5. **Signed publish.** Every watcher message shall be
    wrapped in the codebase's standard signed-envelope
    format and authenticated against the publisher's gossip
    identity at decode time. A receiver shall discard any
    message whose signature does not verify or whose
    embedded coin tickers it does not have enabled locally.

## 9.4 Taker-Published Payload

R6. **Self-contained per-swap context.** The taker's payload
    shall carry everything a watcher needs to act
    autonomously: the swap identifier; the secret hash; the
    two preimages (§9.5); the swap-start timestamp and lock
    duration; both coin tickers; the maker's public key; the
    transaction hashes for the taker-fee and the
    taker-payment; per-coin start-block, required-
    confirmations, and notarisation-required fields for the
    taker coin; and the start-block for the maker coin.

R6a. **The two transaction-hash fields carry hashes, and the
     fee hash is length-checked.** The taker-fee and
     taker-payment fields of R6 shall carry the respective
     transactions' **hash bytes**, not their serialized
     bodies. A watcher that receives a payload whose
     taker-fee field is not exactly 32 bytes shall refuse
     the payload and **not** spawn a state machine for it;
     the refusal happens before the Validate-taker-fee state
     of §9.6 is entered. This is a hard length check on the
     field, not a best-effort parse.

> **Code-quality finding (informative) — reloaded violates R6a
> today.** Reloaded's taker populates both fields with the
> **serialized transaction** rather than with its hash: the
> taker-fee field receives the fee transaction's raw bytes and
> the taker-payment field receives the taker-payment
> transaction's raw bytes. A watcher of the reference lineage
> receiving such a payload fails R6a's 32-byte check on the
> fee field and silently declines to help; a reloaded watcher
> receiving a *reference* taker's payload gets 32 bytes where
> its own taker would have put a whole transaction. So no
> watcher assists any reloaded taker's swap, in either
> direction, for any coin — which also means the defect has
> been invisible, because a watcher declining to help looks
> exactly like no watcher being present. No funds are at risk
> (the watcher substrate is a fallback, and both parties can
> always act for themselves), but the whole substrate is inert
> across the wire until the two fields carry hashes.
>
> The fix is to put each transaction's hash in its field; the
> hash is already computed on both paths. It is a parity
> **fix** with no compatibility switch, because R6a is both the
> chapter's existing rule (R6 already says "transaction
> hashes") and the reference's behaviour. It should be done
> together with a test that asserts the field's length is 32,
> since a test that only round-trips the payload through the
> project's own code cannot see the difference.

R7. **Public-key formats.** The maker's published public key
    on the canonical taker payload shape shall be the
    public-key format the underlying coin's swap script
    uses (33-byte compressed secp256k1 for UTXO-family
    coins, etc.). The substrate does not constrain the
    public-key bytes further; the validator at the watcher
    side delegates the format check to the coin family.

## 9.5 Two Preimages

R8. **Maker-payment-spend preimage.** The first preimage is
    a transaction that, when augmented with the secret
    revealed by the on-chain taker-payment spend, lets the
    maker claim the maker payment. The watcher fills in
    the secret and rebroadcasts.

R9. **Taker-payment-refund preimage.** The second preimage
    is the transaction that, after the timelock elapses,
    refunds the taker payment back to the taker. The
    watcher rebroadcasts it after the refund deadline.

R10. **Both preimages are precomputed by the taker.** Both
     preimages shall be authored and signed by the taker
     **before** the taker can go offline. The watcher
     therefore never holds or derives a taker private key.

## 9.6 Watcher State Machine

The watcher shall be implemented on top of the codebase's
state-machine runtime ([Chapter 14](14-state-machine-runtime.md))
with the following bound states and transitions:

| State                       | Role                                                   |
|-----------------------------|--------------------------------------------------------|
| Validate-taker-fee          | Locate the taker-fee transaction on-chain; validate it |
| Validate-taker-payment      | Wait for the taker-payment transaction with the        |
|                             | configured confirmation count; validate it             |
| Wait-for-taker-payment-spend| Poll for spend (success path) or refund deadline       |
| Spend-maker-payment         | On spend: extract secret; broadcast first preimage     |
| Refund-taker-payment        | On deadline: broadcast second preimage                 |
| Stopped (with outcome)      | Terminal; log the outcome                              |

R10a. **No watcher assists a no-fee swap.** A taker whose
      swap carries no DEX fee — the network burn/waiver key
      case of [Chapter 8](08-fee-routing-engine.md) and
      [Chapter 51](51-legacy-v1-swap-state-machine.md) R14 —
      sends no fee transaction and therefore records an
      **empty** fee-transaction identifier. Its watcher
      payload consequently carries an empty taker-fee field,
      which fails the length check of R6a, and no watcher
      spawns a state machine for that swap.

      This is the **bound** behaviour and it is **parity**:
      the reference reaches exactly the same outcome by the
      same route — it records an empty fee-transaction
      identifier for a no-fee taker, publishes it in the
      payload, and refuses the payload on the fee-field
      length check before spawning. There is no special
      no-fee watcher path, no sentinel value, and no
      fee-validation bypass on either side.

      Consequences a Coder must not "fix" unilaterally:

      - The affected population is exactly the holders of the
        two network burn/waiver keys, so the practical loss of
        watcher coverage is negligible.
      - Making a reloaded watcher accept an empty fee field
        would be a **divergence**: a reference watcher would
        still refuse, so only reloaded watchers would act, and
        the asymmetry needs a `docs/COMPAT_SWITCHES.md` entry
        and a `docs/GLEEC_COMPATIBILITY.md` note. It would also
        require deciding what the Validate-taker-fee state of
        §9.6 means when there is no fee transaction to find —
        a skip is a real weakening, because that state is also
        where the watcher establishes that the publisher is
        the taker of a swap that actually began.
      - The honest minimum, which R6a already gives, is that
        the refusal is a deliberate decision on a
        length-checked field rather than an accident.
        Recorded as D5.

R11. **Compile-time legal transitions.** The set of legal
     transitions shall be fixed at compile time per the
     state-machine runtime's contract; no runtime-only
     transition decision is acceptable.

R12. **Four-variant terminal outcome.** The Stopped state
     shall carry a four-variant outcome: maker-payment-
     spent, taker-payment-refunded, completed-normally
     (the original parties beat the watcher to the
     broadcast), or stopped-on-error (with diagnostic
     payload).

## 9.7 Bound Constants and Defaults

R13. **Conservative timing defaults.** The substrate's
     defaults shall be conservative so that, under normal
     latency, the original parties always beat the watcher
     to the broadcast and the watcher is a fallback rather
     than a competitor. The bound defaults are:

| Constant                              | Value      | Purpose                                                                 |
|---------------------------------------|------------|-------------------------------------------------------------------------|
| Watcher message re-broadcast interval | 10 s       | Taker re-publishes its watcher payload at this cadence                  |
| Taker-fee validation attempts         | 6          | Validation retries before giving up                                     |
| Taker-fee validation retry delay      | 10 s       | Delay between validation retries                                        |
| Wait-for-taker-payment default        | 60 s       | Per-state wait before polling                                           |
| Spend/refund search poll interval     | 300 s      | Default poll cadence for the wait-for-spend state                       |
| Refund-start factor                   | 1.5 ×      | Refund window opens at `start + 1.5 × lock-duration`                    |
| Per-fee-hash watcher lock timeout     | 6 h        | RAII lock expiry guarding against orphaned locks                        |

These are first-party tuning choices; they are not derived
from outside literature.

## 9.8 Per-Node De-duplication

R14. **One state machine per taker-fee hash per node.**
     A node shall not spawn two parallel watcher state
     machines for the same taker-fee transaction hash. The
     substrate shall enforce this via an RAII lock keyed
     by the fee hash and held in the swap context.

R15. **Lock expiry to handle abrupt termination.** The lock
     entry shall carry an expiry timestamp (R13: 6 hours)
     so that an entry orphaned by a process kill before
     its RAII drop ran is eventually reclaimed without
     operator intervention.

## 9.9 Coin-Trait Surface and the Inactive Reward Field

R16. **Per-coin opt-in predicate.** The swap-operations
     coin trait shall expose a single read-only predicate
     declaring whether the coin family permits third-party
     rebroadcast of its swap transactions. The default
     shall be **opt-out** (no watcher activity); coin
     families opt in by overriding the predicate to true.
     The watcher substrate shall short-circuit on any coin
     pair where either side reports opt-out.

R17. **UTXO-style families and Siacoin are the eligible set
     today.** At the time of writing the eligible families are
     UTXO-standard, BCH (Bitcoin Cash), QTUM, and Siacoin
     ([Chapter 20](20-siacoin-integration.md) §20.10 D7 — a
     non-UTXO family that opts in via the same per-coin
     predicate of R16 despite its structurally different
     native-spend-policy HTLC). Other coin families inherit the
     opt-out default. Opting in via the predicate is independent
     of how many of the coin-trait's other watcher methods a
     family actually implements: the three named UTXO-style
     families opt in with none of those methods overridden,
     relying entirely on the trait's own graceful-error defaults,
     while Siacoin's opt-in is paired with real implementations of
     four of the five non-trivial methods (Chapter 20 §20.10 D7).

R18. **`watcher_reward` field placeholder.** The codebase
     shall carry a `watcher_reward` boolean field on the
     V2-swap refund-argument structures. At the time of
     writing every construction site sets this field to
     `false`; coin implementations may read it but shall
     only ever observe `false`. The field is bound as an
     **interface** so that adding a per-network reward
     policy in the future is an additive change at known
     construction sites; activating the field is deferred
     work (D1).

## 9.10 Entry-Point Wiring

R19. **Inbound dispatch on prefix match.** The codebase's
     incoming gossip-message handler shall route any
     message whose topic begins with the watcher prefix
     (R2) to the substrate's signed-envelope decoder.

R20. **Outbound broadcast cadence.** A taker, after sending
     its payment on-chain, shall populate the watcher
     payload (§9.4) and publish it on the appropriate
     watcher topic at the bound re-broadcast interval (R13:
     10 s) until the swap concludes. The cadence ensures
     newly-joining watchers can pick up an in-flight swap.

R21. **Coin-activation-time subscription.** A node shall
     subscribe to a coin's watcher topic at coin-activation
     time, per R3.

## 9.11 Tests

T1. **Topic-shape test.** The substrate shall include a
    unit test asserting that the topic constructor returns
    the expected `<prefix>/<TICKER>` shape for at least
    one well-known ticker (e.g. `swpwtchr/BTC` for the
    Bitcoin ticker).

T2. **Versioned-envelope round-trip.** The envelope shall
    round-trip through the codebase's standard serde
    format for all defined variants.

T3. **State-machine transition table coverage.** Each
    legal transition listed in §9.6 shall have at least
    one unit test that exercises it; each illegal
    transition shall be a compile-time error per R11.

## 9.12 Deferred Work

D1. **Activate the `watcher_reward` field.** Requires (a) a
    per-network policy parameter on the registry of
    [Chapter 6](06-network-id-seed-node.md) parallel to
    the burn-related entries; (b) code at the three
    refund-argument construction sites that derives the
    boolean from the policy; (c) coin-side handling for
    the `true` branch in each refund implementation. None
    of this is in scope at the time of writing.

D2. **Cryptographically-bound watcher attribution.** R5
    authenticates the publisher of the watcher message,
    not the watcher that subsequently broadcasts a
    preimage. A receipt-style mechanism that lets the
    network attribute a specific rebroadcast to a specific
    watcher would be required before any reward economy
    (D1) could distinguish honest watchers from
    free-riders. Not in scope today.

D3. **Eligible-coin-family expansion.** *Partially
    discharged.* R17 lists four eligible families today,
    up from the three UTXO-style families this entry
    originally described; Chapter 20 §20.10 D7 is the record
    of Siacoin's addition, including which of the coin-trait's
    non-trivial watcher methods it implements and which one
    (`create_maker_payment_spend_preimage`) it judges
    infeasible and deliberately leaves on the trait's own
    graceful default. Adding any further family still requires
    the family's transaction format to permit deterministic
    third-party rebroadcast given the watcher payload; each new
    family remains an additive opt-in (R16).

D4. **Operator-tunable timing.** R13's constants are
    literals. Exposing them as operator-tunables (per-coin
    or per-network) would let operators trade aggression
    for safety margin in environments with non-default
    latency profiles. Not in scope today.

D5. **Watcher coverage for a no-fee swap.** R10a records that
    neither this project nor the reference lets a watcher assist
    a swap that pays no DEX fee, and that the gap is structural
    rather than accidental: the watcher's first state is a
    fee-transaction validation, and that validation is also what
    binds the publisher to a swap that really started. Giving
    no-fee swaps watcher coverage therefore needs a replacement
    for that binding — the obvious candidate being the taker
    payment itself, validated first instead of second — and it
    needs the divergence treatment named in R10a, because a
    reference watcher would go on refusing. It is deferred, not
    rejected: the population affected is the two network
    burn/waiver keys, so the cost of leaving it open is small
    and the cost of getting the replacement binding wrong is
    not. Also, nothing in it should be attempted before the R6a
    defect is fixed, since no watcher assists any reloaded swap
    at all until then.

D6. **Per-node de-duplication keyed on a variable-length field.**
    R14 keys the one-state-machine-per-swap lock on the
    taker-fee hash. Under R6a that key is a fixed 32 bytes, so
    the keying is sound once the fee field carries a hash. Two
    residual cases are unaddressed and are not worth separate
    rules until R6a lands: a no-fee swap's empty field, which
    would collide across all such swaps if the lock were ever
    taken before the length check (this project should take it
    after), and a payload whose fee field is 32 bytes of a
    hash that names no on-chain transaction, which the lock
    accepts and the Validate-taker-fee state then rejects
    after its retry budget (R13) has elapsed.

## 9.13 External References

- The atomic-cross-chain-swap pattern (informal references
  and public wiki) on which the maker/taker
  secret-reveal protocol is based.
- The publish-subscribe overlay protocol the substrate
  uses for topic-based gossip (referenced via
  [Chapter 28](28-libp2p-modernization.md)).
- The gossip peer-identity scheme used by R5 for
  signed-envelope authentication.
- The state-machine runtime
  ([Chapter 14](14-state-machine-runtime.md)) on which the
  watcher state machine of §9.6 is built.
- The per-network-id configuration registry
  ([Chapter 6](06-network-id-seed-node.md)) on which D1's
  reward-policy parameter would be added.
- The fee-routing substrate
  ([Chapter 8](08-fee-routing-engine.md)) whose
  validate-fee surface the watcher invokes during the
  validate-taker-fee state of §9.6.
- The Siacoin integration
  ([Chapter 20](20-siacoin-integration.md) §20.10 D7), the
  fourth family added to R17's eligible set and the record
  of which coin-trait watcher methods it implements.

## 9.14 Baseline Verifications

The following are verifiable from the baseline state defined
in [Chapter 02](02-baseline-state.md), commit
`c1d46c0c1592faa0860f704008b2b2381bc3840f`:

V1. The baseline tree carries no watcher substrate. A
    tree-wide
    `git grep -E 'swap_watcher|swpwtchr|is_supported_by_watchers|watcher_reward'`
    against the baseline returns no matches.

V2. The baseline atomic-swap path treats refunds as the
    exclusive responsibility of the original transaction
    sender; the gap that R6-R10 close is therefore a
    pre-existing operational property, not one introduced
    after the baseline.

V3. The publish-subscribe overlay the substrate rides on is
    present at the baseline; the watcher substrate does
    not introduce the overlay, only a new topic family on
    it.

## 9.15 Provenance Footer

- *Status:* driving-spec.
- *Version:* v2.
- *Verified against:* baseline commit
  `c1d46c0c1592faa0860f704008b2b2381bc3840f`; absence of
  the watcher substrate and its identifiers at baseline
  verified via tree-wide `git grep`; the
  publicly-documented atomic-cross-chain-swap protocol
  pattern; the publish-subscribe overlay protocol used
  by the codebase; the state-machine runtime substrate
  bound by Chapter 14; the per-network configuration
  registry of Chapter 6 (referenced by D1); the fee-
  routing substrate of Chapter 8 (referenced by §9.6's
  validate-taker-fee state); the Siacoin integration of
  Chapter 20 §20.10 D7 (R17's fourth eligible family,
  cross-checked against this repository's own
  `is_supported_by_watchers` overrides across
  `mm2src/coins/`, not the forbidden corpus).
- *Forbidden corpus:* not consulted for anything in this chapter
  **except** R6a, R10a and deferred items D5 and D6, which were
  authored under the chapter-01 §1.4A two-team workflow and are
  scoped to three facts: that the taker-fee field of the watcher
  payload carries 32 hash bytes and that a watcher length-checks
  it and declines the payload before entering its first state;
  that a taker paying no DEX fee records an empty
  fee-transaction identifier and therefore publishes an empty
  field, so no watcher assists such a swap; and that the
  reference has no no-fee-specific watcher path, sentinel, or
  fee-validation bypass. Established under the public release
  designations `v2.6.0-beta` and `v3.0.0-beta`; the record is no
  more precise than those designations. The code-quality finding
  under R6a concerns **this repository's** own code and takes no
  forbidden-class input. No discretionary expression — no private
  type, field or function name, no body, control-flow
  transcription, local name, or error literal — was reproduced.
- *Dirty Gate clearance (chapter 01 T1, R43) for R6a, R10a, D5 and
  D6:* **not yet recorded.** Those additions are post-adoption
  revisions carrying forbidden-class-sourced material, so T1
  reaches them; the gate has not yet returned a verdict and the
  extent, method and measure are to be written into this item
  when it does. The rest of the chapter keeps the
  `not consulted` position stated above.
