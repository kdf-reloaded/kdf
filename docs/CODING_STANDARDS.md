# Rust Coding Standards

Project-wide style and structure standards for `kdf-reloaded`.

These standards complement `AGENTS.md` and apply to newly authored or
substantially rewritten code.

## 0. Hard rules

- `cargo fmt` must pass.
- `cargo clippy --all-targets --all-features -- -D warnings` must pass.
- No `unwrap()` / `expect()` / `panic!()` outside tests, build scripts,
  and unavoidable `const` initialization.
- Prefer explicit error propagation via `MmError` and typed error enums.
- Keep attribution/license headers already present in files unless legal
  review instructs otherwise.
- Every change must be purposeful (bug fix, correctness, maintainability,
  architectural simplification, dependency hygiene, or readability).

## 1. Naming

- Types: `UpperCamelCase` with role suffixes where useful (`*Builder`,
  `*Validator`, `*Watcher`, `*Ctx`, `*Cfg`, `*Params`, `*Response`).
- Functions and variables: `snake_case`, verb-first function naming.
- Predicates: start with `is_`, `has_`, `can_`, or `should_`.
- Constants: `SCREAMING_SNAKE_CASE`.
- Modules: `snake_case`, avoid catch-all names like `utils` and `helpers`.

## 2. Documentation

- Every public item should have a `///` doc-comment with one-line summary.
- Add `# Errors` for fallible APIs and `# Panics` when applicable.
- Module docs should describe purpose, public exports, and key invariants.
- When structure is externally constrained (wire format, ABI, SQL grammar,
  FFI, protocol schema), state that binding explicitly in module docs.

## 3. In-code comments

- Comments explain *why*, not *what*.
- Use TODO/FIXME in searchable format:
  `// TODO(<issue-or-user>): ...` and `// FIXME(<issue-or-user>): ...`.
- Avoid stale commented-out code.

## 4. Layout and whitespace

- One blank line between major declarations (`struct`, `enum`, `impl`, `fn`).
- Keep functions focused; extract helpers when bodies become hard to scan.
- Group imports by standard library, third-party crates, then workspace crates.

## 5. Error handling and safety

- Prefer exhaustive matches when practical.
- Validate RPC inputs at boundaries with strong types.
- Avoid blocking operations inside async contexts.
- Keep sensitive data out of logs and error messages.

### 5.1 Documented compatibility exceptions

The default security posture is mandatory: secrets are encrypted at rest,
inputs are validated, and the OWASP Top 10 is treated as a hard bar. A
behaviour that would breach this posture is permitted **only** when all of
the following hold:

1. it is the minimum necessary to match an external on-disk or wire format
   the project must inter-operate with (an *Interop / wire-format reuse*
   fragment under clean-room rule R29);
2. the governing CRD chapter **explicitly** describes the weaker behaviour,
   why it is required, and the bounded exposure it creates; and
3. the cost is restated in the module docs next to the code and in
   [`GLEEC_COMPATIBILITY.md`](GLEEC_COMPATIBILITY.md).

Where practical, a stronger-security alternative is offered as a separate,
opt-in value of the same setting. Absent an explicit CRD authorisation of
this form, insecure behaviour is a defect and must be fixed, not shipped.

Fund-controlling secrets (wallet seeds and private keys) keep their stronger
protection **by default** regardless of compatibility. The single authorised
exception is key export, governed by CRD chapter 07: the secure default is
preserved (the offline / bulk / HD / shielded-key export superset is refused),
and GLEEC-parity export is reached **only** by an explicit operator opt-in,
the `allow_insecure_key_export` switch. Because the default stays secure and
the relaxation is a deliberate, acknowledged operator act, this does not breach
the rule that fund-controlling secrets are never weakened *by default*. Any
other extension of a compatibility exception to fund-controlling secrets is a
defect.

## 6. Tests

- Test names should describe behavior and condition:
  `should_<outcome>_when_<condition>`.
- Add focused tests for bug fixes and new behavior.
- Keep assertions clear and deterministic.

## 7. Review checklist

Before merging:

1. Formatting and clippy clean.
2. Public docs updated for changed behavior.
3. Error paths and input validation covered.
4. No sensitive data exposure in logs/errors.
5. Tests added/updated and passing.

## 8. Change discipline

- Keep commits small, scoped, and reviewable.
- Separate mechanical refactors from behavioral changes.
- Preserve public/wire contracts unless a migration is explicit.
