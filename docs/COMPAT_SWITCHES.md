# Compatibility convention

KDF Reloaded is a continuation of the upstream Komodo DeFi Framework and aims to remain operationally compatible with the GLEEC KDF fork wherever practical. When our behaviour intentionally diverges, we follow a **documentation convention** so that every operator and every third-party integration can recover the GLEEC-compatible behaviour by setting one or more configuration values.

This document defines that convention. **It deliberately does not define a common code construct, schema, or JSON object.** Each divergent feature implements its own switch in whatever shape fits that feature best (a config key, an RPC argument, an env var, a Cargo feature, a startup flag, etc.). The unified surface is the *documentation*, not the code.

## The rule

> Any change to KDF Reloaded that diverges from upstream / GLEEC KDF behaviour in a way that can break an existing third-party integration, change the shape or meaning of an RPC, change order-matching / fee / settlement semantics, surprise an operator who is migrating from GLEEC KDF, or — in the worst case — lead to coin loss, **must ship together with a way to opt back into the original behaviour**, and that opt-in must be documented in both places listed below.

For human contributors, this is a **strong recommendation** — the reviewer should push back on any divergent change that fails the rule without a justification.

For AI assistants working on this codebase, this is a **strict, mandatory rule** — divergent changes that omit the opt-in or its documentation must not be proposed or committed.

## The two documentation locations

Every divergent change must be marked in **both** of the following places. There is no other unifying surface — there is no global enum, no `compatibility` object, no `kdf_compat_mode` value, no central registry in code.

### 1. Next to the setting itself

Wherever the new or changed setting is documented (the RPC reference, the configuration reference, the relevant in-tree `AGENTS.md`, an admin-facing README chapter, a Cargo-feature list, etc.), include a short, clearly visible note of the form:

> **Compatibility:** for behaviour matching GLEEC KDF (and the upstream pre-divergence Komodo DeFi Framework), set this to `<value>`. *(Optional one-line rationale.)*

If the setting is new and has no GLEEC counterpart, say so explicitly:

> **Compatibility:** GLEEC KDF has no equivalent. Leave at the default to retain GLEEC-equivalent behaviour.

### 2. Central admin chapter

Add or update an entry in [`docs/GLEEC_COMPATIBILITY.md`](GLEEC_COMPATIBILITY.md) — the user/admin-facing chapter listing **every** setting an operator must configure to run a KDF Reloaded node in full compatibility with GLEEC KDF. One row per divergent setting; the row points back to the per-setting documentation in (1).

This central chapter is the single document an operator migrating from a GLEEC KDF deployment needs to read end-to-end.

The same chapter pattern can be reused for other significant forks or versions in the future — for example, a `docs/UPSTREAM_COMPATIBILITY.md` if a meaningful divergence from upstream Komodo DeFi Framework ever emerges. Each such chapter is independent.

## Defaults

Each divergent change has its own default, and that default does **not** have to be the GLEEC-compatible value. Which side it takes is not a free choice and not the implementer's preference: it follows the default-side rule below. Whichever side wins, the documentation in both locations above must make the GLEEC-compatible value unambiguous.

### The default-side rule

This is the project's standing policy on which side of a compatibility divergence the default falls. Cite it as *`docs/COMPAT_SWITCHES.md`, "The default-side rule"* (anchor `#the-default-side-rule`).

> 1. **Parity by default wherever the change can reach anyone else.** Where a deferral or divergence touches wire compatibility, or would clearly break things for users or nodes other than our own, parity with GLEEC KDF is the default.
> 2. **Reloaded by default where it cannot.** Where the divergence does **not** touch wire compatibility, does **not** affect other users or nodes, and only improves user experience or security, the KDF Reloaded variant is the default.
> 3. **Anything in between is a project-team decision.** A change that is not clearly one of the two cases above must be weighed and decided by the project team. It is explicitly **not** a decision for whoever happens to be implementing it.

Arm 3 is the operative one in practice: an implementer who cannot place a change squarely in arm 1 or arm 2 stops and asks, rather than picking a default and documenting it afterwards. A deferral counts the same as a divergence here -- choosing not to implement the GLEEC behaviour yet is itself a choice of default.

The two subsections below are standing project-team decisions for their own classes of divergence. A change that falls into one of them follows that subsection directly and is not re-weighed under arm 3.

The rule has governed the project's divergence decisions since before it was written down, and CRD chapter material already appeals to it as the project's standing rule -- but it existed only as a shared understanding, so a reader had no way to verify it. An audit flagged exactly that gap; this section closes it. The rule itself is unchanged by being recorded.

The rule is about **compatibility-divergence defaults**. It is not a rule about operator-facing security controls; see [What this convention is not](#what-this-convention-is-not).

### Departures from GPLv2-or-fair-trading principles

If the upstream / GLEEC behaviour conflicts with this project's principles — non-GPLv2 distribution, restrictions on free trading, restrictions on participation, or similar — the divergent setting:

- defaults to the KDF Reloaded behaviour;
- still exposes a way to reach the original-compatible behaviour, but **only behind an explicit acknowledgement** (a second key, an env var, a CLI flag — whatever fits the feature) named so that selecting it is a deliberate operator act; and
- emits a prominent runtime warning whenever the original-compatible value is selected.

The per-setting documentation must spell out the rationale; the central chapter must flag the entry as acknowledgement-gated.

### Security-versus-compatibility departures

A different case arises when matching GLEEC / upstream behaviour requires a *less secure* implementation than KDF Reloaded would otherwise choose — for example, persisting a secret at rest in plaintext because the on-disk format must stay byte-compatible with an existing deployment. Unlike the principle-based departures above, such a setting **may default to the compatible (less-secure) value** when:

- the security exposure is clearly bounded and is *not* a fund-controlling secret (wallet seeds and private keys keep their stronger protection regardless of compatibility); and
- the governing CRD chapter explicitly describes the exposure and the reason the weaker behaviour is required for interoperability (see [`CODING_STANDARDS.md`](CODING_STANDARDS.md) §5.1).

The reduced-security cost must be stated in the per-setting documentation and in `GLEEC_COMPATIBILITY.md`. A stronger-security value of the same setting should be offered as an alternative where practical. Because the compatible value is the default here, the entry is **not** acknowledgement-gated; a runtime warning and acknowledgement key become appropriate only if and when the stronger-security value is made the default.

**Fund-controlling-secret carve-out.** The exclusion above (wallet seeds and private keys keep their stronger protection regardless of compatibility) has exactly one CRD-authorised exception: the `allow_insecure_key_export` switch governed by CRD chapter 07. Unlike the security-versus-compatibility case described in this section, this switch **keeps the secure value as the default** (`false`, the GLEEC export superset refused) and reaches GLEEC parity only through an explicit operator opt-in. Because the less-secure value is *not* the default, the entry **is** acknowledgement-gated and the node emits a prominent warning when the switch is enabled. No other compatibility setting may relax the protection of a fund-controlling secret.

## What this convention is not

- **Not** a JSON schema. There is no `compatibility: {}` object in `MM2.json`.
- **Not** a code interface. There is no `CompatMode` enum, no `compat_value_for(...)` helper, no `MmCtx::compat_*` field common to all switches.
- **Not** a single global mode. An operator who wants GLEEC behaviour in one area and KDF Reloaded behaviour in another sets exactly the values they want and leaves the rest alone.
- **Not** a versioned bundle. There is no "GLEEC-classic profile". The central chapter lists settings; the operator picks the ones they need.
- **Not** a rule about operator-facing security controls. The two are different mechanisms with different purposes and must not be conflated: a compatibility switch exists to let an operator recover GLEEC-compatible behaviour from a deliberate divergence, whereas a security control exists to let an operator reduce attack surface on their own node. A security control's default is a security decision for the feature that owns it, not a parity question, and [the default-side rule](#the-default-side-rule) does not reach it. Such a control enters this convention only if enabling or disabling it also diverges from GLEEC KDF behaviour in the way [the rule](#the-rule) describes -- in which case it needs the usual opt-back mechanism and both documentation entries for *that* divergence, and the default-side rule applies to that divergence alone.

## Removing a divergence

A divergent setting (and its corresponding rows in both documentation locations) may be removed when:

- the divergence has been retired and the implementation no longer supports the alternative behaviour at all; or
- a deprecation has been published in `CHANGELOG.md` for at least one minor release with a documented migration path.

When a row is removed from `docs/GLEEC_COMPATIBILITY.md`, note the removal in `CHANGELOG.md`.

## Related documents

- [`GLEEC_COMPATIBILITY.md`](GLEEC_COMPATIBILITY.md) — the central admin chapter (user-facing list of every setting).
- [`../RELOADED_VS_GLEEC.md`](../RELOADED_VS_GLEEC.md) — public catalogue of the divergences themselves.
- [`../CHANGELOG.md`](../CHANGELOG.md) — every divergent change is logged here.
