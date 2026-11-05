# Plan: Electrum TLS server authentication and the key pin

**Status:** planning (the rules are written; the warning and the pin are not built) — **Last reviewed:** 2026-11-05

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
2. **Lab run.** A SwapLab run with self-signed lab Electrum servers, using the current flag. A
   refusal it shows is classified by R38.6.4m items 3 and 5 (D38.6.4m.1).
3. **Code**, after the maintainer approves the implementation. The warning of R38.6.4m item 6 and
   the pin of R38.6.4n, with T38.6.4m (c) and (e) and T38.6.4n. The code PR carries the
   CHANGELOG entries, a `docs/GLEEC_COMPATIBILITY.md` row, a `RELOADED_VS_GLEEC.md` bullet, a
   release-tests row and the operator description of the key with the `openssl` pipeline.
4. **Pass-through** (D38.6.4n.1): whether the coins file and the wallets pass `spki_sha256` through
   to the activation request.

## Open points

- D38.6.4m.1: what the lab run shows.
- D38.6.4n.1: pass-through by the coins file and the wallet GUIs.
- D38.6.4n.2: trust on first use. It is not part of this design, because it is weaker than a
  published pin.
