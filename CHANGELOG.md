# Changelog

## Unreleased

### Added

- Family-standard repository infrastructure: Cargo workspace with pinned Rust
  1.98.1 toolchain, shared lints, `family.toml` profile metadata (resident +
  external state), `cargo xtask` gate with schema-first contract checking, and
  supply-chain configuration (`deny.toml`).

### Changed

- The Node catalogue generator (`scripts/catalog.mjs`) was removed;
  `schemas/tools.json` is the single documented schema-first authority,
  verified by Rust tooling against actual discovery and dispatch.

### Added

- Family-standard CLI vocabulary: `mcp` (stdio bridge, `stdio` kept as a
  legacy alias), `init` (alias of `init-config`), `config check`, and a local
  read-only `doctor [--json]` that needs no Linear credential or network;
  `doctor --online` performs the explicit authenticated viewer check.
- Presentation hardening under the rust-minijinja-v1 profile: bounded
  recursion and execution fuel, rendering into a private bounded buffer with
  the documented 2 MiB product reply budget, and a truthful status-preserving
  fallback that never truncates exact Document/context content.
- Single-binary delivery: `crates/family-delivery` (template helper with a
  tested external-state adaptation, `state_schema = 0`), `self-install` /
  `releases use` CLI including an identity-checked, byte-exact-backed-up
  legacy-launcher adoption route, `cargo xtask package [verify]`, `cargo xtask
  release prepare/publish/wait`, `install.sh`, `scripts/wait-release.sh`, and
  SHA-pinned CI/release workflows that build once, verify exact payload bytes
  (including contract/transport/CLI acceptance through `MCP_TEST_BINARY`) and
  publish through a complete draft behind the reviewed `release` environment.
  Qualification flags remain honest declarations; publication stays disabled
  until native host evidence exists.

No published release is implied by the Cargo package version.
