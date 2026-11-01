# Clean-room tooling

## `clean_room_gate.py` — R35 / R36 residual-similarity gate

Automated implementation of the residual-similarity gate that
[`docs/reloaded-rewrite/01-clean-room-rules.md`](../docs/reloaded-rewrite/01-clean-room-rules.md)
defers (rule **R35**, deferral **D1**, and chapter 30). The gate scores a
reloaded file's **discretionary expression** against a *relicensed historical
record* reference. That reference **is** the corpus forbidden under R8 — CRD
chapter 01 R31 defines the two terms as the same material — so the gate is a
corpus-side tool, not a clean-side one. It reads the reference only inside a
gated reference repository held outside this checkout; no reference content is
vendored here, and only similarity scores cross back into the clean side. In
practice it is run by the maintainer. Note that no role CRD chapter 01 R37
separates carries a bound mandate covering the measurement of a file of source
— the gate role it defines is bound to a candidate chapter — and this file is
not part of the operational elaboration R44 subordinates, so nothing stated here
widens any mandate chapter 01 binds. It must never be run by a clean-side
context.

### What it measures

R35 forbids using a whole-file similarity figure as a clean-room gate for
interface-dense files: signatures, serde renames, schema names, `#[repr]`
discriminants, pinned constants, and import lists are *required* to match, so
their matching proves nothing. The gate therefore scores the **discretionary
body** (function bodies, helper decomposition, control flow, local naming,
comment/diagnostic wording) after stripping comments and normalizing
whitespace, with pinned-interface / R28–R33 lines excluded from the measured
set.

A file **passes** when its discretionary-body similarity is materially below its
whole-file similarity (configurable margin) and under an absolute cap. Residual
byte-identical discretionary lines are listed so a reviewer can confirm each is
attributable to an excluded fragment, as R35 requires.

### Marking excluded (pinned) lines

Exclusion is explicit, matching the CRD requirement that reused fragments be
*marked* at the point of reuse:

| Marker | Effect |
| --- | --- |
| `// crd:pin` (end of line) | exclude that single line |
| `// crd:pin-begin` … `// crd:pin-end` | exclude the enclosed block |
| `// crd:r28` … `// crd:r33` | exclude one line, recording its fragment class |

`#` comment markers work too (for non-Rust files). `--auto-pin` additionally
excludes obvious structural interface lines (imports, `fn`/`struct`/`enum`/
`trait`/`impl` signatures, `#[repr(...)]`, serde `rename`); it is advisory —
explicit markers are authoritative.

### Usage

```sh
# Gate everything listed in the manifest (the canonical invocation)
python3 tools/clean_room_gate.py \
  --manifest tools/clean_room_gate.manifest.json --auto-pin

# One-off check against the relicensed historical record
python3 tools/clean_room_gate.py \
  --reloaded mm2src/foo/src/bar.rs \
  --reference git:c1d46c0c1592faa0860f704008b2b2381bc3840f:mm2src/foo/src/bar.rs \
  --verbose
```

### Run it on *formatted* code

The gate compares **normalized line lists**, so how source is split across lines
affects the score. This repository's `rustfmt.toml` is the same canonical style
upstream uses, so leaving code hand-formatted can mask real structural
similarity behind cosmetic line-break differences. The honest, format-invariant
standard is therefore:

```sh
cargo +nightly-2026-05-08 fmt --all
python3 tools/clean_room_gate.py \
  --manifest tools/clean_room_gate.manifest.json --auto-pin
```

A file is only considered to pass when it passes **after** `cargo fmt`. The
whole manifest currently passes 51/51 under this format-invariant standard.

### Manifest format

`clean_room_gate.manifest.json` is an object: `reference_origin` pins the
relicensed historical-record tree independence is asserted from, `defaults`
holds the thresholds (`margin`, `max_discretionary`, `margin_floor`), and
`files` is the list of gated entries. Each entry names a reloaded file and its
upstream `reference` path; the gate expands it to
`git:<reference_origin.commit>:<path>` and resolves it inside a gated reference
repository (the upstream tree is never vendored here — only similarity scores
cross back). Per-file `interface_only: true` waives the cap/margin for
re-export / module-glue shims that are identical by structural necessity.

```json
{
  "reference_origin": { "commit": "<upstream commit>", "...": "..." },
  "defaults": { "margin": 0.15, "max_discretionary": 0.50, "margin_floor": 0.50 },
  "files": [
    { "reloaded": "mm2src/foo/src/bar.rs", "reference": "mm2src/foo/src/bar.rs" }
  ]
}
```

### How the gate is run

The gate is run **locally** at chapter-edit and pre-publication time — this is
the manual audit boundary that chapter 30 (D1) describes as deferred. It is not
wired into CI: the reference comparison needs the relicensed historical record,
which is not vendored into this repository, so the gate is exercised by the
maintainer against a local reference checkout rather than on hosted CI runners.
