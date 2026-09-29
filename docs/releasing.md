# Releasing and installation

One product version comes from Cargo (`[workspace.package] version`); the
binary's `--version`, the delivery manifest, `serverInfo` and the release tag
all mirror it. The 25-tool public contract, the resident loopback writer, the
stdio bridge and the protected configuration are unchanged by release
mechanics.

## Local gate (no network, no credentials)

```sh
cargo fetch --locked
cargo xtask check      # fmt, clippy (default + all-features), tests, rustdoc, contract, standard
cargo deny --locked check
```

## Release sequence

1. **Prepare** (worker or owner, local edits only, never pushes):
   `cargo xtask release prepare X.Y.Z --apply` bumps the workspace version,
   opens the CHANGELOG section and refreshes the local lockfile. Review the
   diff, run the gate, commit.
2. **Review and merge**: the release candidate goes through the same PR gate
   (`ci.yml`: macos-26 arm64 `cargo xtask check` + ubuntu `cargo-deny`; no
   secrets are required for PR checks).
3. **Qualify the candidate natively, then enable the declarations — all
   BEFORE any tag**: the owner verifies the merged candidate on the real Mac
   host and records the native evidence; then, still before tagging, a
   reviewed commit flips the `family.toml` declarations
   (`qualification = "verified"`, `qualified_targets`, `qualified_hosts`,
   `release.enabled = true`) citing that evidence. Tags are immutable, so no
   commit or flag change may follow the tag for that version; anything missed
   requires a new version.
4. **Tag**: the owner creates an annotated `vX.Y.Z` tag on the exact accepted
   merged commit (the one that already carries the enabled declarations and
   evidence) and pushes it.
5. **Build once**: the tag-driven `release.yml` build job runs the full gate,
   `cargo xtask package`, `cargo xtask package verify`, and uploads the
   payload artifact (binary + `release-manifest.json`, `state_schema = 0` =
   no local business state).
6. **Qualify the exact CI bytes**: the publish job waits in the owner-reviewed
   `release` GitHub environment (required reviewer, `v*` tag policy). While it
   waits, the owner downloads the artifact of that run and verifies those
   exact bytes on the real Mac host (for example `MCP_TEST_BINARY=<downloaded
   binary> cargo test --frozen -p agent-tasks-linear --test contract --test
   transport --test cli`, plus `doctor` and a disposable-home install), then
   approves the environment. A locally rebuilt binary is never accepted as
   evidence for the published payload; this gate additionally qualifies the
   exact CI artifact on top of the pre-tag candidate evidence.
7. **Publish**: after approval, `cargo xtask release publish` re-verifies
   hashes before executable permission, runs cargo-deny and the
   contract/transport/CLI suites against the exact payload through
   `MCP_TEST_BINARY`, refuses while `family.toml` declares
   `release.enabled = false`, `qualification != "verified"` or empty
   qualified targets/hosts, then creates a complete draft, downloads and
   verifies it, and publishes it.
8. **Observe**: `scripts/wait-release.sh --repo DKotsyuba/Agent-Tasks-Linear
   --tag vX.Y.Z --commit <full sha> [--result-file path]` binds repository,
   annotated tag, workflow run/attempt and downloaded asset hashes. Integrity
   is not provenance: `provenance_verification` stays `not_performed`.

## Host qualification evidence

The CI runner label (`macos-26`, arm64) is not host qualification. Native
evidence — the actual macOS build and CPU of the owner's machine, the
installed-launcher checks and the exact-payload test results — is recorded by
the owner from the real host, and only that record justifies flipping
`qualified_targets`/`qualified_hosts`, `qualification` and `release.enabled`
in a reviewed commit.

## Installation and legacy adoption (owner-operated)

```sh
./install.sh --version 0.5.0 [--home /absolute/home] [--bin-dir /absolute/bin]
./install.sh --version 0.5.0 --home ~/.config/agent-tasks-linear --adopt-existing
```

The installer verifies SHA256SUMS and the release manifest before executing
anything, then runs the downloaded binary's `self-install`, which writes only
immutable version directories under `<home>/standalone/`, a managed launcher
in `--bin-dir`, and nothing else — the owner's `config.toml`, credentials and
client registrations are untouched. Re-installing the same version with the
same bytes is a no-op; different bytes are refused. Rollback:
`agent-tasks-linear releases use <version> --home <home> --bin-dir <bin>`.
Rolling back code never undoes Linear-side changes.

**Adopting the pre-template 0.4.0 plain executable** at
`~/.local/bin/agent-tasks-linear` is explicit: `--adopt-existing` asks the
delivery helper to run the old file's `--version`, refuses anything that does
not identify as this product, preserves the old executable byte-exactly as
`<bin>/agent-tasks-linear-legacy-0.4.0` (sha256 recorded), and only then
replaces it with the managed launcher. Foreign or unrecognized launchers are
always refused, with or without the flag.

The managed launcher pins the installation default through `ATL_CONFIG` —
only when the caller has not set it and `<home>/config.toml` exists — and
forwards argv unchanged. Configuration precedence is therefore: an explicit
`--config` argument (as the existing serve/connect wrappers pass) > an
explicit `ATL_CONFIG` > the pinned installation default > the binary's own
`HOME`-based default. A child process that changes `HOME` cannot silently
redirect the installed product to an empty configuration, and wrappers
passing their own `--config` never hit a duplicate-argument error. No global
environment or user file is modified. Using the existing
`~/.config/agent-tasks-linear` directory as the installation home keeps the
live config and credentials in place with no migration. Stop the idle writer
before switching versions and restart it explicitly afterwards; the installer
never restarts, drains or prunes anything.

## Host registration

`registration/agent-tasks-linear.json` is the host-neutral descriptor
(product identity, stdio command through the stable managed launcher,
optional env, timeouts). It is a descriptor, not an automatic edit of any
host configuration; registering with a specific client is the owner's
explicit action using that client's supported mechanism.
