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
   binary> cargo test --frozen -p agent-tasks --test contract --test
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
8. **Observe**: `scripts/wait-release.sh --repo DKotsyuba/agent-tasks
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

### 0.5.0 qualification scope

The local candidate from `88e4348000540c0e9ec2b1e0cbb33474b205848a`
(SHA-256 `1f89a9e84197300df061a2c0925e73ecf0852df321b3b5351bbc94b49e15611a`)
was exercised on macOS 27.0 build 26A428, arm64, through Claude Code 2.1.280
on 2026-09-29. Native MCP document creation, guarded section replacement
with preserved sibling content, identical-request replay, and UTF-8/binary
attachment upload/download passed. Both downloaded fixtures were compared
byte-for-byte. A disposable installed launcher passed local doctor with 25
tools under a changed child HOME and an explicit configuration argument.
The source also passed [CI](https://github.com/DKotsyuba/Agent-Tasks-Linear/actions/runs/36598775453).

Qualification covers that target and host only. Native Linear search returned
title/content/semantic matches, but an opaque hyphenated body marker was not
found; it is not an exhaustive substring index. These local candidate checks
do not replace step 6's qualification of the exact CI-produced release bytes.
No publication or production installation is implied by these declarations.

## Installation and legacy adoption (owner-operated)

```sh
./install.sh --version 0.6.0 [--home /absolute/home] [--bin-dir /absolute/bin]
./install.sh --version 0.6.0 --home ~/.config/agent-tasks --adopt-existing
```

The installer verifies SHA256SUMS and the release manifest before executing
anything, then runs the downloaded binary's `self-install`, which writes only
immutable version directories under `<home>/standalone/`, a managed launcher
in `--bin-dir`, and nothing else — the owner's `config.toml`, credentials and
client registrations are untouched. Re-installing the same version with the
same bytes is a no-op; different bytes are refused. Rollback:
`agent-tasks releases use <version> --home <home> --bin-dir <bin>`.
Rolling back code never undoes Linear-side changes.

The rename to `agent-tasks` does not touch the existing `agent-tasks-linear`
0.5 installation: it is a separately named launcher and home, left running
until the owner has verified the new install and chooses to retire it.

**Adopting a pre-existing plain executable** at the new
`~/.local/bin/agent-tasks` path is explicit: `--adopt-existing` asks the
delivery helper to run that file's `--version`, refuses anything that does
not identify as this product (so an unrelated foreign executable already at
that path, for example an old Python CLI, is left untouched), preserves the
adopted executable byte-exactly as `<bin>/agent-tasks-legacy-<version>`
(sha256 recorded), and only then replaces it with the managed launcher.
Foreign or unrecognized launchers are always refused, with or without the
flag.

The managed launcher pins the installation default through `ATL_CONFIG` —
only when the caller has not set it and `<home>/config.toml` exists — and
forwards argv unchanged. Configuration precedence is therefore: an explicit
`--config` argument (as the existing serve/connect wrappers pass) > an
explicit `ATL_CONFIG` > the pinned installation default > the binary's own
default, which is `~/.config/agent-tasks/config.toml`, falling back to the
pre-rename `~/.config/agent-tasks-linear/config.toml` only when a config
already exists there and not at the new default. A child process that
changes `HOME` cannot silently redirect the installed product to an empty
configuration, and wrappers passing their own `--config` never hit a
duplicate-argument error. No global environment or user file is modified;
neither default path is ever created, moved or overwritten by this fallback,
it only changes which existing file is read. Using the existing
`~/.config/agent-tasks-linear` directory's `config.toml` as the new
installation home's config (copied, not moved) keeps the live credentials in
place with no forced migration. Stop the idle writer before switching
versions and restart it explicitly afterwards; the installer never restarts,
drains or prunes anything.

## Host registration

`registration/agent-tasks.json` is the host-neutral descriptor (product
identity, stdio command through the stable managed launcher, optional env,
timeouts). It is a descriptor, not an automatic edit of any host
configuration; registering with a specific client is the owner's explicit
action using that client's supported mechanism.
