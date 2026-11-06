# Plan: Bitcoin II (BC2) sighash suffix in the signature hash

**Status:** planning (CRD chapter 61 written; implementation, coins-file change and live check open) — **Last reviewed:** 2026-11-05

> **Context:** since block 57,750 the Bitcoin II network checks every
> signature against a digest that carries an extra 4-byte field, the
> sighash suffix. A KDF Reloaded node and a Komodo KDF 2.6.0-beta
> node both signed a plain withdrawal from a native-segwit BC2 output the
> ordinary way and the network rejected both ("Signature must be zero for
> failed CHECK(MULTI)SIG operation"). Neither signature is wrong by the
> ordinary rule; the network's rule changed. This blocks every BC2 withdrawal
> and swap payment of this build, and of the reference builds, until the
> signers append the field. It is not the Bitcoin-Cash fork id of CRD ch.58:
> the sighash type stays `SIGHASH_ALL`.

## Where it stands (2026-11-05)

| Item | State |
|---|---|
| Analysis | **done** — the rule, its activation height (57,750) and its value (`0x01324342`) are verified against the mined chain (CRD ch.61 §61.0, T1) |
| CRD chapter 61 | **this PR** (R61.2–R61.4, tests T1–T9, decisions D1–D5) |
| Code | **open** — one new input to the digest routine, the two configuration keys, hardware refusal, activation warning, tests T1–T8 |
| Coins file | **open** — `sighash_suffix` on the `BC2` and `BC2-segwit` entries (separate change in the shared coins repository) |
| Live check | **open** — a withdrawal from a native-segwit output and from a P2PKH output accepted and mined; a swap with BC2 on the Reloaded side |
| Issue #252 | open — the reporter's failure ("Maker payment validate failed") is not yet placed; this change is one confirmed cause of BC2 transactions being rejected, not necessarily the whole report |

## Steps

1. **CRD text (this PR).** Chapter 61, the pointers in chapters 38 and 58, the
   index row in chapter 30, the Known limitation in `CHANGELOG.md`.
2. **Implementation.** The suffix enters through the one digest routine
   (R61.4.2); the two keys are parsed at activation (R61.3.1, R61.3.2); a coin
   with a non-zero suffix refuses hardware signing (R61.4.4); activation logs
   one warning for a BC2 entry without the suffix (R61.4.5). User-facing
   documents: `RELOADED_VS_GLEEC.md`, `docs/GLEEC_COMPATIBILITY.md`, the
   changelog (R61.5.2).
3. **Coins file.** Add `"sighash_suffix": "0x01324342"` to the
   `BC2` and `BC2-segwit` entries of the shared coins repository.
4. **Live check** with the untouched native-segwit output and a P2PKH output
   (T9), then a swap.

## Decisions

- **CV (decided 2026-11-06).** The key is `sighash_suffix` (optional height key `sighash_suffix_height`), a new mechanism-named key. Reusing `fork_id` was measured and rejected: it changes what the reference builds sign, and a value wider than 32 bits makes their coin-configuration parse panic.
- **D1 (CRD §61.8).** Honor `sighash_suffix_height` in the first
  implementation, or only parse and validate it. Recommendation: parse and
  validate now; both supported networks are far above the activation height.
- **D2 (CRD §61.8).** Refuse, before it starts, a swap whose BC2 side a
  reference node would have to sign. Not part of this plan's first step.

## Out of scope

Taproot digest (D3), Bitcoin II header validation under its new difficulty
rule (D4), the test network's activation height (D5).
