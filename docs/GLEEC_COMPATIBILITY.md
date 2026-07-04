# Running KDF Reloaded in full GLEEC-KDF compatibility

This chapter is the single place that lists every configuration value an operator must set to make a KDF Reloaded node behave equivalently to a GLEEC KDF node, for the benefit of operators migrating an existing GLEEC KDF deployment or third-party integrations that target the GLEEC behaviour.

The convention behind this chapter — including the developer rule that requires every divergent change to land an entry here — is documented in [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md).

## How to use this chapter

1. Start from your existing `MM2.json` (or whichever configuration surface you use).
2. Walk down the table below. For each row, set the listed value if you want the GLEEC-equivalent behaviour for that area.
3. Skip any row whose KDF Reloaded default already matches what you want; rows are listed exhaustively, not by impact.
4. For acknowledgement-gated settings (marked **⚠ gated**), additionally set the listed acknowledgement key — and read the per-setting documentation linked from the row, because these settings select behaviour the project does not endorse but still supports for compatibility.

There is no global "GLEEC mode" switch and no shared JSON object — every setting is its own thing in its own place. This chapter is the unifying reference, not a code construct.

## Settings

| Area | Setting | GLEEC-compatible value | Acknowledgement-gated? | Per-setting docs |
|------|---------|------------------------|------------------------|------------------|
| Network selection | `netid` | `8762` or `6133` — GLEEC's default netid `0` is **not supported** | No | [see below](#netid) |
| WalletConnect session storage | `wc_session_persistence` | `open` *(also the default)* | No | [CRD ch.22 §22.5](reloaded-rewrite/22-walletconnect-v2.md) |
| Key export | `allow_insecure_key_export` | `true` — enables GLEEC-parity offline / no-activation / HD-range / shielded key export. Default `false`. | Yes | [CRD ch.07 §7.3A](reloaded-rewrite/07-wallet-lifecycle-and-key-export.md) |

### `netid`

KDF Reloaded compiles its per-network parameters at build time and recognises
only the netids in its registry — currently `8762` and `6133`. Any other netid
is **rejected at startup**: `lp_init` returns `MmInitError::UnsupportedNetId`
listing the supported networks, and the node does not start.

This includes the *inherited default*. Both KDF Reloaded and GLEEC KDF derive
`netid` the same way — an unset `netid` resolves to `0` — but GLEEC KDF treats
netid `0` as a live operating network, so a GLEEC node started without an
explicit `netid` simply joins netid `0`. KDF Reloaded has no compiled
configuration for netid `0` and therefore has **no equivalent to GLEEC's
default network**: it operates only on its compiled netids.

Consequences for operators and integrators:

- A GLEEC deployment that ran on the default netid `0` has no direct reloaded
  equivalent. Choose a supported network and set `netid` to `8762` or `6133`.
- Always set `netid` explicitly. Relying on the default is a startup error on
  KDF Reloaded, not a fallback to a public network.
- Test fixtures and tooling that spin up a node (anything that initiates the
  P2P network) must likewise set a supported `netid`; the inherited default of
  `0` will fail the same startup check.

**`wc_session_persistence`.** Controls whether and how WalletConnect v2
sessions are written to durable storage. It governs *saving* only — existing
stored sessions are always read and used regardless of the value. Values:

- `open` *(default)* — sessions are stored in the GLEEC-compatible plaintext
  format, including the session symmetric key. This keeps the on-disk format
  byte-interchangeable with GLEEC KDF in both directions. The key is stored
  unencrypted at rest; the bounded exposure, and why it is accepted for
  compatibility, is described in CRD chapter 22. This is **not** the
  fund-controlling wallet secret, which is encrypted independently.
- `none` — sessions are never written to storage. Existing rows are still
  read and used, but never rewritten. Choose this if you do not want the
  session key persisted at all.
- `encrypted` — reserved for a future encrypted-at-rest format; not yet
  available (selecting it stops startup with an explanatory error).

**`allow_insecure_key_export`.** A top-level `MM2.json` boolean, default
`false`. It governs how much private-key material the node will export.

- `false` *(default)* — the secure posture. The single-coin private-key reveal
  for one *activated* coin, and the reduced per-activated-coin key export,
  remain available; own-mnemonic self-export and wallet-password re-encryption
  remain available. The GLEEC export *superset* — offline export with no coin
  activation, hierarchical-deterministic per-derivation-path ranges, and
  shielded (ZHTLC) viewing-key export — is **refused**.
- `true` — full GLEEC-parity key export: offline / no-activation bulk export,
  HD per-derivation-path ranges, and protocol-specific formatting for UTXO
  (WIF), EVM (hex), Tendermint, and shielded ZHTLC viewing keys.

In both states a hardware-wallet session is always rejected (keys are never
exported off the device), secret material is never logged or persisted in
plaintext, and the surface is intended for localhost / trusted-channel use.
This is a **fund-controlling secret**: enabling the switch is a deliberate,
acknowledged operator act and the node emits a prominent warning when it is
active. The bounded exposure and rationale are described in CRD chapter 07
§7.3A. See [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md) and
[`CODING_STANDARDS.md`](CODING_STANDARDS.md) §5.1 for why this is the sole
fund-controlling-secret carve-out.

## Future fork chapters

The same chapter pattern (one document, one exhaustive table) can be reused if other significant forks or upstream divergences ever need an analogous compatibility surface — for example, `UPSTREAM_COMPATIBILITY.md` for the upstream Komodo DeFi Framework, should that ever diverge meaningfully from KDF Reloaded. Each such chapter is independent of the others and lists only the settings relevant to its named target.

## See also

- [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md) — the developer-facing rule and convention.
- [`../RELOADED_VS_GLEEC.md`](../RELOADED_VS_GLEEC.md) — the catalogue of divergences (what changed, not how to undo it).
- [`../CHANGELOG.md`](../CHANGELOG.md) — every divergent change is logged here.
