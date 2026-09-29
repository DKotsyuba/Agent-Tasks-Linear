# agent-tasks — agent instructions

rust-minijinja-v1 response profile. Read docs/MCP_RESPONSE_STANDARD.md;
docs/architecture.md is the behavior truth source.

## Single workflow

`cargo xtask check` is the full non-mutating gate (fmt, clippy default and
all-features, tests default and all-features, rustdoc, contract, standard).
`cargo fetch --locked` prepares dependencies first.

The tool contract is **schema-first**: `schemas/tools.json` is the single
authority; `src/catalog.rs` embeds it and discovery serves it verbatim. The
Node catalogue generator was removed. `cargo xtask contract check` proves the
committed schema equals the embedded catalogue, the gateway dispatch
vocabulary and real-binary MCP discovery over stdio. Edit the JSON schema
directly and commit; there is no generator and no second source.

## Invariants

Rust 2024, resolver 3, pinned toolchain 1.98.1, committed application lock,
publish=false, shared workspace lints. No Python/Node scripting dependencies.
One resident loopback writer plus a stdio bridge; durable workflow state lives
only in Linear (external-state profile — never add a local workflow database,
queue or scheduler). Normal MCP text uses strict embedded MiniJinja over small
typed views. No raw JSON dumping. Preserve execution outcome, exact
identifiers, pagination and recovery even when rendering fails. stdout of the
stdio bridge is protocol-only. Secrets never enter responses or diagnostics.

## Delivery and updates

This product's profile is resident + external state + single-binary-v1
delivery with `state_schema = 0` (no local business state — not `state =
none`). Package locally only after committing source. Release prepare defaults
to preview and never pushes. The publisher checks qualification, tag/source/run
identity, cargo-deny and the actual payload bytes before publishing a complete
draft. Never flip qualification to make a pipeline green; native host evidence
stays a separate record. No pruning, service restart, host registration or
secret migration is implicit. Do not claim native/host tests passed without
evidence from those runs.

## Legacy adoption note

The owner's machine may still hold a foreign, unrelated executable at the new
`~/.local/bin/agent-tasks` launcher path (for example an old Python CLI from
before this rename). `self-install --adopt-existing` refuses anything whose
`--version` output does not identify as this product; adoption is explicit,
backed-up and identity-checked, owned by the operator (see docs/releasing.md).
The existing `agent-tasks-linear` 0.5 launcher and home are a separate,
untouched installation; nothing here migrates or overwrites it.
