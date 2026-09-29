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

No published release is implied by the Cargo package version.
