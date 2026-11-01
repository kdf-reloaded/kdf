# Third-party names in interop surfaces

This chapter states when KDF Reloaded may use a third party's name — a
vendor, product, organisation, or service name — inside code, configuration,
URLs, wire fields, and CRD chapters, and when it may not.

It exists because the two rules that normally govern naming pull in opposite
directions here. The clean-room wall
([`../AGENTS.md`](../AGENTS.md) §2) and the no-embedded-endpoints rules found in
several CRD chapters both push names out of the tree. Interoperability pushes
some of them back in, because a service that can only be addressed by a literal
containing a proper noun cannot be addressed at all without it.

## The rule

A third-party name may be used **when it is functionally necessary to
interoperate, in the position the counterparty dictates, and nowhere else.**

The test is functional necessity, not the token's spelling. Ask: *if this token
is withheld, does the thing still work?* If the answer is no, the token is
interop surface and may be stated — in code, in configuration, and verbatim in a
CRD chapter. If the answer is yes, it is decoration and stays out.

This holds regardless of whether the token looks like branding. A path segment,
query-parameter name or value, header name, protocol method string, chain or
network identifier, on-disk schema name, or enum discriminant does not stop
being dictated interop because the counterparty named it after themselves.

## What the rule does not authorise

Necessity is scoped to the position the counterparty dictates. Being permitted
to send a dictated token never extends to:

- **hostnames, default base URLs, or fallback endpoints** compiled into the
  tree — these stay caller-supplied at runtime, per the per-chapter
  no-embedded-endpoints requirements;
- **branding** — logos, marks, styling, or presenting a third party's name to
  users as though it were ours or ours as though it were theirs;
- **any claim of affiliation, endorsement, partnership, or certification**;
- **protected expression** — the wall is unaffected. A name being dictated says
  nothing about whether surrounding implementation detail may cross.

## Conditions

Use is conditional on not breaching the counterparty's **licence, terms of
service, or trademark rights**. Where any of those would be breached, the token
stays out and the requirement is expressed without it — accepting that the
integration may then be unimplementable as specified, which is the correct
outcome rather than a problem to route around.

Trademark law generally permits naming a product to describe interoperation
with it (nominative use); it does not permit implying endorsement. That
distinction is the practical line and it matches the scope above.

When the necessity is not obvious — the token is avoidable, or the terms are
unclear or restrictive — record the reasoning where the decision is applied
rather than relying on this chapter alone.

## Worked example

The NFT subsystem talks to a third-party EVM indexer whose HTTP API dictates a
path token carrying the operator's name. Without it the service cannot be
addressed, so it is interop surface: it appears in the request path and is
stated in CRD chapter 19. That authorises nothing further — the base URL
remains caller-supplied, no hostname is embedded, and no affiliation is implied.
See [`reloaded-rewrite/19-nft-module-layout.md`](reloaded-rewrite/19-nft-module-layout.md)
§19.9 R1 for the chapter-local form.

## See also

- [`../AGENTS.md`](../AGENTS.md) §2 — the clean-room wall and forbidden corpus.
- [`COMPAT_SWITCHES.md`](COMPAT_SWITCHES.md) — the convention for deliberate
  behavioural divergence.
- [`reloaded-rewrite/34-provenance-ledger.md`](reloaded-rewrite/34-provenance-ledger.md)
  — per-component provenance and licence classification.
