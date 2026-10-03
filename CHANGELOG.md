# Changelog

## Unreleased

- Adopt rmcp 3.4.0. Both HTTP and stdio catalogues follow the caller's revision:
  MCP 2026-07-28 returns complete results with a 60-second private cache lifetime;
  explicit legacy sessions omit the modern fields. Unknown tools return protocol
  errors while expected workflow/input rejections retain tool-result `isError`.
- Exercise all five declared protocol revisions through raw HTTP and real-binary
  stdio, including discovery, calls, bounded replies and EOF; preserve the 25-tool
  authoritative schema and the resident writer/external Linear state profile.
- Export the current family response standard, correct schema-first architecture
  documentation, and record reviewed incremental template provenance without
  replacing historical adoption baselines. Reset new-candidate qualification
  and disable publication until exact-payload/native-host acceptance.
- Replace the yanked `yoke-derive 0.8.3` lock entry with compatible patch `0.8.4`
  while retaining the required cargo-deny gate.

## 0.6.0

Product rename: Cargo package/binary, crate imports, `serverInfo`,
`family.toml` identity (product/repository/env-prefix), the registration
filename and the two committed role skills now say `agent-tasks`, matching
the renamed `DKotsyuba/agent-tasks` GitHub repository. The 25-tool
schema-first API, `ATL_CONFIG`, and the existing `agent-tasks-linear` 0.5
installation are unchanged. `family.toml`'s qualification/qualified
targets-hosts/`release.enabled` are reset to unverified: the 0.5.0 native
evidence below qualified the old identity, not this one.

### Changed

- Renamed the Cargo package/binary, crate imports and `serverInfo`/health/log
  identity literals from `agent-tasks-linear` to `agent-tasks`.
- Renamed `family.toml`'s `product`/`repository`/`env_prefix`, the
  registration descriptor file, `install.sh`'s `PRODUCT`/`REPO`/home
  variable, and the two committed role skills' directories and frontmatter.
- The protected config default now resolves `~/.config/agent-tasks/config.toml`
  first, falling back read-only to the pre-rename
  `~/.config/agent-tasks-linear/config.toml` only when a config already
  exists there and not at the new default; explicit `--config`/`ATL_CONFIG`
  are unaffected.

## 0.5.0

First standardized infrastructure release of the existing 25-tool Linear
workflow MCP: family-standard workspace, gates, delivery, installer and CI,
with the public tool contract, resident writer/stdio bridge architecture and
protected configuration preserved unchanged. Publication is gated: the
release workflow stays disabled until native host evidence is recorded by
the owner.

### Added

- Family-standard repository infrastructure: Cargo workspace with pinned Rust
  1.98.1 toolchain, shared lints, `family.toml` profile metadata (resident +
  external state), `cargo xtask` gate with schema-first contract checking, and
  supply-chain configuration (`deny.toml`).
- Family-standard CLI vocabulary: `mcp` (stdio bridge, `stdio` kept as a
  legacy alias), `init` (with `init-config` as its legacy alias), `config
  check`, and a local read-only `doctor [--json]` that needs no Linear
  credential or network; `doctor --online` performs the explicit
  authenticated viewer check.
- Presentation hardening under the rust-minijinja-v1 profile: bounded
  recursion and execution fuel, rendering into a private bounded buffer with
  the documented 2 MiB product reply budget, a truthful status-preserving
  fallback that never truncates exact Document/context content, and visibly
  escaped control/bidi characters in short title/name display labels.
- Single-binary delivery: `crates/family-delivery` (template helper with a
  tested external-state adaptation, `state_schema = 0`), `self-install` /
  `releases use` CLI including an identity-checked, byte-exact-backed-up
  legacy-launcher adoption route, `cargo xtask package [verify]`, `cargo xtask
  release prepare/publish/wait`, `install.sh`, `scripts/wait-release.sh`, and
  SHA-pinned CI/release workflows that build once, verify exact payload bytes
  (including contract/transport/CLI acceptance through `MCP_TEST_BINARY`) and
  publish through a complete draft behind the reviewed `release` environment.
  The managed launcher pins the installation default through `ATL_CONFIG`
  only when the caller has not set it and forwards argv unchanged, so
  configuration precedence stays explicit `--config` > explicit `ATL_CONFIG` >
  pinned installation default > `HOME`-based default, existing wrappers that
  pass their own `--config` keep working, and child processes that change
  `HOME` cannot silently redirect the installed product to an empty
  configuration. Qualification flags remain honest declarations; publication
  stays disabled until native host evidence exists.

### Changed

- The Node catalogue generator (`scripts/catalog.mjs`) was removed;
  `schemas/tools.json` is the single documented schema-first authority,
  verified by Rust tooling against actual discovery and dispatch.
