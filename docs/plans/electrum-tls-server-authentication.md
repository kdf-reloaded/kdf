# Plan: Electrum TLS server authentication and the key pin

**Status:** in progress (the rules are written; the warning and the pin are built, PR #273; the pass-through of step 4 is open) — **Last reviewed:** 2026-11-07

**CRD:** ch.38 R38.6.4m and R38.6.4n (Reloaded-original), with T38.6.4m, T38.6.4n and the open items
D38.6.4m.1, D38.6.4n.1 and D38.6.4n.2; ch.55 R55.5.10 (the `mode.rpc_data.servers[]` row).

> **Context.** A coin's Electrum server entry with `protocol` `SSL` is checked against the bundled
> web roots. `disable_cert_verification` turns that check off. It was reported that a server with
> the flag set was still refused, and the question was whether the flag accepts self-signed
> certificates. It does, for a version 3 certificate with a key the TLS library accepts. The
> refusals that remain are by design: a version 1 certificate, an RSA key below 2048 bits, an IP
> literal as the host, or a TLS version below 1.2.
>
> The maintainer decided on 2026-11-04 to document the flag exactly, to warn at activation when it
> is set, and to add an optional per-server pin on the server's key, `spki_sha256`. A pinned server
> can use a self-signed certificate and still be authenticated.

## Steps

1. **Rules (this step).** R38.6.4m describes what the client offers and checks, with and without
   the flag. It names the refusals that stay, and specifies the activation warning. R38.6.4n
   specifies the pin: its value, the verification, the error classes, the browser build and the
   operator guidance. The ch.55 row points to both.
2. **Lab run.** *Done 2026-11-05 (D38.6.4m.1 resolved).* Seven certificates, three builds, 42 runs:
   accept and refuse are identical on KDF Reloaded and both references. Under the flag only the
   version 1 and RSA-1024 certificates are refused, by design (R38.6.4m items 3 and 5).
3. **Code.** *Built and tested 2026-11-07 (PR #273).* The
   warning of R38.6.4m item 6 and the pin of R38.6.4n, with T38.6.4m (c) and (e) and T38.6.4n. The
   code PR carries the CHANGELOG entries, a `docs/GLEEC_COMPATIBILITY.md` row (the reference
   ignores the key, measured 2026-11-07), a `RELOADED_VS_GLEEC.md` bullet and a release-tests row.
   The operator description of the key, with the `openssl` pipelines of R38.6.4n item 7, is the
   section "Electrum Servers over TLS" in `docs/NETWORK_CONFIG.md`.
4. **Pass-through** (D38.6.4n.1): whether the coins file and the wallets pass `spki_sha256` through
   to the activation request.

## Open points

- D38.6.4n.1: pass-through by the coins file and the wallet GUIs.
- D38.6.4n.2: trust on first use. It is not part of this design, because it is weaker than a
  published pin.
- Follow-ups from the review of PR #273, neither of which changes what is accepted:
  - The pin parser removes every colon, as R38.6.4n item 5 says, while the user docs describe
    colons between byte pairs. Either tighten the parser to that form or keep the looser reading.
  - The mismatch warning is chosen by comparing the error text. A typed marker would keep the
    warning if a later layer wraps that text; the suspension does not depend on it.
- Seen 2026-11-07: the coins file's `SSL` entry for tQTUM, `s6.electrum.qtum.site:51002`, presents a
  version 1 self-signed certificate, so KDF Reloaded never connects to it (R38.6.4m item 5, also with
  `disable_cert_verification` or a pin). The plain entry on port 51001 works. No coins-file change is
  proposed from here.
