# Release Checklist

A release is not cut until every box below is ticked. This checklist applies to all tagged releases (`v0.x.y-alpha.N`, `v0.x.y-beta.N`, and `v1+` stable). Pre-release builds (CI artifacts, dev builds) are exempt.

## Code

- [ ] `cargo fmt --all -- --check` passes.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes.
- [ ] `cargo test --bins --lib` passes.
- [ ] Integration tests pass (`cargo test --test 'mm2_tests_main'`).
- [ ] Docker tests pass (`cargo test --bin docker_tests --features regtest-netid -- --test-threads=1`).
- [ ] WASM build succeeds (`cargo build --target wasm32-unknown-unknown -p mm2_bin_lib`).
- [ ] No `unwrap`/`expect`/`panic!` introduced in RPC paths without justification.
- [ ] No new `unimplemented!()` in code paths reachable without an explicit experimental Cargo feature.

## Documentation

- [ ] `CHANGELOG.md` has an entry for the new version with an accurate date.
- [ ] `RELOADED_VS_GLEEC.md` Added / Removed / WIP lists are up to date.
- [ ] `ROADMAP.md` reflects items moved between sections.
- [ ] `README.md` build and configuration instructions still apply verbatim.
- [ ] AGENTS.md files updated if module structure or conventions changed.

## Compatibility

For each behavioural divergence from GLEEC KDF introduced in this release (see [`docs/COMPAT_SWITCHES.md`](docs/COMPAT_SWITCHES.md) for the developer rule):

- [ ] The per-setting documentation includes a clearly visible "set this to `<value>` for GLEEC compatibility" note (or an explicit "GLEEC has no equivalent" note for net-new settings).
- [ ] A row exists in [`docs/GLEEC_COMPATIBILITY.md`](docs/GLEEC_COMPATIBILITY.md) pointing back to the per-setting documentation.
- [ ] If the GLEEC-compatible value is acknowledgement-gated, both the per-setting docs and the central-chapter row spell out the acknowledgement requirement and the runtime warning.
- [ ] The change is referenced from `RELOADED_VS_GLEEC.md` (Added / Removed / WIP lists, as appropriate).
- [ ] Any retired divergence is noted in `CHANGELOG.md`, the corresponding row in `docs/GLEEC_COMPATIBILITY.md` is removed, and the per-setting docs are updated.

## Security

- [ ] Counsel review on file for the licensing posture of this release (or, for an interim release, an explicit counsel waiver).
- [ ] Release artifacts signed with the published GPG/minisign key. Signature files attached to the GitHub release.
- [ ] Vulnerability disclosure mailbox active and monitored.
- [ ] No known unfixed high-severity advisories against direct dependencies (verified via `cargo audit` or equivalent).
- [ ] No secrets present in the tree (sweep for accidental commits of `MM2.json`, `.env`, mnemonics, private keys).

## Release artifacts

- [ ] Linux x86-64 release binary built via `.github/workflows/build-linux.yml` (Debian 11 container, glibc 2.31 floor for broad backwards compatibility) and uploaded.
- [ ] Other-platform binaries built via the umbrella `dev-build.yml` if they are part of this release's matrix. Note: `dev`/`staging` `v*` tags auto-trigger the unsigned `dev-build.yml` snapshot; only `main` `v*` tags trigger the signed `release.yml`.
- [ ] Artifact filenames include the version tag.
- [ ] SHA-256 checksums published alongside artifacts.

## Tagging and announcement

- [ ] Version bumped in `Cargo.toml` workspace members where applicable.
- [ ] Annotated git tag created (`git tag -a v0.x.y-... -m '…'`).
- [ ] GitHub Release drafted referencing the `CHANGELOG.md` entry and `RELOADED_VS_GLEEC.md` delta.
- [ ] Release announcement drafted (does not promise audited security; explicit alpha/beta status).
- [ ] First-72-hour issue triage owner identified.
