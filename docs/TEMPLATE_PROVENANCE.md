# Template adoption provenance

This repository adopts infrastructure from the family template
`DKotsyuba/agent-mcp-template` at pinned commit
`7f094e0463c0a6d5cf52d68d91ca32f6bc0f465f` (devkit 0.2.0, family standard
1.0.0-rc.2). Adoption was done by copying and adapting the specific files
listed below into this existing product — the template's `init` generator was
never run over this checkout, no `.family/` managed tree was imported
wholesale, and nothing in this repository depends on the private template at
build or runtime. Copied helper code retains the template's MIT license
notice (see `docs/TEMPLATE_MIT_LICENSE.txt`); the product's own sources
remain proprietary to the owner with no open-source license granted.

## Imported files and local adaptations

| File in this repository | Template source at 7f094e0 | Adaptation |
|---|---|---|
| `rust-toolchain.toml` | `scaffold/rust-toolchain.toml` | Verbatim. |
| `.cargo/config.toml` | `scaffold/.cargo/config.toml` | Verbatim (xtask alias only). |
| `deny.toml` | `scaffold/deny.toml` | Verbatim starting allowlist; entries adjusted only for this product's actual dependency licenses, each adjustment noted below. |
| `docs/MCP_RESPONSE_STANDARD.md` | `standard/MCP_RESPONSE_STANDARD.md` | Verbatim export of the canonical standard; keep in sync with the template in the same change when the profile rules change. |
| `docs/TEMPLATE_MIT_LICENSE.txt` | `LICENSE` | MIT notice covering the copied helper code only. |
| `docs/FAMILY_CONTRACT.md` | `scaffold/docs/FAMILY_CONTRACT.md` | Verbatim summary. |
| `family.toml` | `scaffold/family.toml` | Product identity (agent-tasks / DKotsyuba/agent-tasks / AGENT_TASKS_) and this product's honest profile: `process = "resident"`, `state = "external"`, transports stdio + streamable-http. `state_schema = 0` in delivery manifests therefore means "no local business state", not `state = "none"`. |
| `xtask/` | `scaffold/xtask/src/main.rs`, `scaffold/xtask/Cargo.toml` | Rust-only gate (prepare/check/standard/contract). Contract checking rewritten for the existing schema-first model: `schemas/tools.json` stays authoritative, there is no generator, and `contract check` runs Rust tests comparing the file with the embedded catalogue, the gateway dispatch vocabulary and real-binary MCP discovery. add-tool/template-upgrade commands were not imported (typed-generator workflow does not apply to the existing 25-tool product). |
| `crates/family-delivery/` | `scaffold/crates/family-delivery/src/lib.rs`, `Cargo.toml` | Single-binary delivery helper with an explicit tested `external` state-profile adaptation (packaging stamps `state_schema = 0`); the managed launcher pins the product home and its `config.toml` when present, and an identity-checked, byte-exact-backed-up legacy-launcher adoption route was added. See the crate's own notes. |
| `install.sh` | `scaffold/install.sh` | Product name/repository/env prefix substituted; the repository is public so the unauthenticated HTTPS path is the default and `gh` remains the authenticated option. Includes the `--adopt-existing` passthrough for the backed-up legacy adoption route. |
| `scripts/wait-release.sh` | `scaffold/scripts/wait-release.sh` | Verbatim wrapper forwarding to `cargo xtask release wait`. |
| `.github/workflows/ci.yml` | `scaffold/.github/workflows/ci.yml` | Same SHA-pinned actions and job split; product gate invoked via `cargo xtask check`; no secrets required for PRs. |
| `.github/workflows/release.yml` | `scaffold/.github/workflows/release.yml` | Same SHA-pinned actions, build-once artifact, hash verification before executable permission, exact-payload tests via `MCP_TEST_BINARY`, `environment: release` approval pause, complete-draft publication. |

## Machine-checkable baseline (`.family/`)

`.family/origin.json` records the pinned template revision, product and
repository in the template generator's own format; `.family/manifest.json`
lists every managed import with its template source path, the SHA-256 of its
baseline bytes and whether this repository deliberately adapted it;
`.family/baseline/` holds those baseline bytes (the pinned template's files
after the generator's identifier substitutions). This is the one-time
adoption equivalent of the template's init-time baseline: a future template
upgrade compares template ↔ baseline ↔ local and must treat every
`adapted: true` file as a conflict to review, never an overwrite. The
template's `template diff/upgrade` xtask commands themselves were not
imported (see Not imported); the baseline is complete so a future reviewed
wave can add them without guessing ownership. Nothing here implies release
qualification.

## Documented migration exceptions (GOV-02)

- **Clippy `unwrap_used`/`expect_used` stay `allow` at workspace level.** The
  template's scaffold denies them for new products; this existing product holds
  many post-validation JSON unwraps predating adoption, and converting them in
  one infrastructure wave would risk the working 25-tool workflow for no
  observable guarantee. Presentation code (`src/render.rs`) is held to the
  response standard and is unwrap/expect-free in production paths. New code
  must not add unwraps; a follow-up wave can migrate extraction to typed
  helpers and then flip the lints to deny.
  Rule: **LANG-04**, pinned scaffold workspace lint baseline. Risk: existing
  unchecked post-validation assumptions may panic; schema/workflow regressions
  exercise them, and presentation remains free of production unwrap/expect.
  Owner: repository maintainer **DKotsyuba**. Review by **2026-11-03**, and
  before enabling a release; an expired exception cannot qualify a release.
- **`deny.toml` license additions.** `MIT-0` (borrow-or-share → jsonschema)
  and `CDLA-Permissive-2.0` (webpki-root-certs → rustls → reqwest) are
  reviewed permissive licenses in this product's existing dependency tree, and
  workspace-internal crates are excluded from license evaluation because the
  product deliberately declares no open-source license.
  Rules: **DEP-06** and **TPL-04**, product-specific supply-chain/license policy.
  Rationale: these dependencies are already required by schema/HTTPS handling;
  copied MIT helpers retain their notice, while proprietary workspace code is
  not granted an open-source license. Risk: future dependency/license drift;
  cargo-deny still checks every external dependency and disallows unknown
  sources. Owner: repository maintainer **DKotsyuba**. Review by **2026-11-03**,
  and before enabling a release.

## Reviewed incremental adoption

The current reviewed reference is template commit
`5b5cd784d59139d2f3fb4acf9943af2ff235f347`. This increment adopts the exact
`rmcp =3.4.0` SDK pin (with the required `process-wrap 10.0.1` lock update),
the revision-dependent complete catalogue/60-second private cache policy,
raw modern discovery and legacy compatibility checks, supported revision
declarations, and the verbatim current `standard/MCP_RESPONSE_STANDARD.md`
export. The response profile, compiler, workspace, delivery helper and CI
baseline require no replacement.
The existing xtask test selectors include the new raw protocol suite in both
`contract check`/`test contract` and `test protocol`; the full workspace gate
also executes it in default and all-features configurations.

The application lock also replaces yanked `yoke-derive 0.8.3` with compatible
non-yanked patch `0.8.4`; no other packages change in that supply-chain repair.
The yanked-dependency gate remains enabled without an advisory suppression.

The product retains its schema-first authority, explicitly permitted by the
template's BOOT CONTRACT-01 and MCP-03: `schemas/tools.json` is unchanged and
no Rust-catalogue conversion or generator is introduced. The resident HTTP
writer, stdio Bridge and external Linear state stay product requirements.
Unknown tool names now use SDK protocol errors at the transport boundary
(MCP-08), preserving the Gateway's internal Outcome contract.

Original `.family/origin.json`, `.family/manifest.json` and all baseline
bytes retain their historical revision, identifiers and hashes. This reviewed
increment is recorded here separately; historical import bytes must not be
restamped as the current template. Native evidence for published 0.6.0 remains
in `docs/releasing.md`; changed SDK/source qualification is `not_verified`
and publication stays disabled pending new exact-payload/native-host acceptance.

## Not imported

- Template `init.sh` bootstrap and `.family/` baseline/upgrade machinery:
  this product predates the template; adoption is the reviewed diff above.
- `scaffold/crates/mcp-presentation`: the product already embeds its own
  MiniJinja presentation (`src/render.rs`, `assets/mcp/*.j2`) under the same
  `rust-minijinja-v1` profile; it was hardened in place rather than replaced.
- `add-tool`/`template upgrade` xtask flows: they belong to the typed-first
  new-product workflow, not the existing schema-first product.
