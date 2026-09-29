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

No published release is implied by the Cargo package version.
