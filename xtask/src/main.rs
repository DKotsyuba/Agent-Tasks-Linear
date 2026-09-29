//! Rust-only product automation for agent-tasks-linear. The tool contract is
//! schema-first: `schemas/tools.json` is the single authority, and this gate
//! verifies it against the embedded catalogue, the gateway dispatch vocabulary
//! and real-binary MCP discovery. No interpreter or generator is involved.
#![allow(clippy::print_stdout, reason = "Developer CLI, not MCP")]

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Network preparation without changing Cargo.lock.
    Prepare,
    /// Canonical non-mutating gate: fmt, clippy, tests, rustdoc, contract, standard.
    Check,
    /// Check declarative family invariants for this product's profile.
    Standard {
        #[command(subcommand)]
        command: Option<StandardCommand>,
    },
    /// Verify the authoritative schema snapshot against dispatch and discovery.
    Contract {
        #[command(subcommand)]
        command: Option<ContractCommand>,
    },
    /// Focused test suites.
    Test { suite: String },
}

#[derive(Subcommand)]
enum StandardCommand {
    Check,
}

#[derive(Subcommand)]
enum ContractCommand {
    Check,
}

/// Parsed repository identity used by every gate.
struct Project {
    /// Repository root (parent of this crate).
    root: PathBuf,
    /// Product package name from Cargo.
    name: String,
    /// Root Cargo manifest as raw TOML.
    manifest: toml::Value,
    /// family.toml profile metadata.
    family: toml::Value,
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Run one helper with GitHub transport credentials removed unless it is gh.
fn helper(root: &Path, program: &str) -> Command {
    let mut command = Command::new(program);
    command.current_dir(root).stdin(Stdio::null());
    // Only gh needs these transport credentials; cargo/git must not inherit them.
    if program != "gh" {
        for name in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
        ] {
            command.env_remove(name);
        }
    }
    command
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    println!("xtask: {program} {}", args.join(" "));
    if !helper(root, program).args(args).status()?.success() {
        return Err(format!("{program} failed").into());
    }
    Ok(())
}

impl Project {
    fn load() -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("missing project root")?
            .to_path_buf();
        let manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
        let family: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join("family.toml"))?)?;
        let name = manifest["package"]["name"]
            .as_str()
            .ok_or("package name missing")?
            .to_owned();
        Ok(Self {
            root,
            name,
            manifest,
            family,
        })
    }

    /// Structural family checks: identity, honest profile, pins and required
    /// files. Proves structure only, never semantics or security.
    fn standard(&self) -> Result<()> {
        let w = &self.manifest["workspace"];
        if w["resolver"].as_str() != Some("3")
            || w["package"]["edition"].as_str() != Some("2024")
            || self.manifest["package"]["publish"].as_bool() != Some(false)
            || self.manifest["lints"]["workspace"].as_bool() != Some(true)
        {
            return Err("Rust workspace invariant failed".into());
        }
        if self.family["product"].as_str() != Some(self.name.as_str())
            || self.family["repository"].as_str() != Some("DKotsyuba/Agent-Tasks-Linear")
            || self.family["standard_version"].as_str() != Some("1.0.0-rc.2")
            || self.family["response_profile"].as_str() != Some("rust-minijinja-v1")
        {
            return Err("family identity/standard mismatch".into());
        }
        // This product is an explicit resident + external-state adaptation.
        // `external` must never be relabelled `none` to satisfy a scaffold.
        if self.family["profiles"]["process"].as_str() != Some("resident")
            || self.family["profiles"]["state"].as_str() != Some("external")
            || self.family["profiles"]["transports"]
                .as_array()
                .is_none_or(|t| t.iter().filter_map(|v| v.as_str()).all(|s| s != "stdio"))
        {
            return Err("family process/state profile mismatch".into());
        }
        let toolchain: toml::Value = toml::from_str(&std::fs::read_to_string(
            self.root.join("rust-toolchain.toml"),
        )?)?;
        if toolchain["toolchain"]["channel"].as_str() != w["package"]["rust-version"].as_str() {
            return Err("pinned toolchain must equal the declared rust-version".into());
        }
        for file in [
            "Cargo.lock",
            "AGENTS.md",
            "CLAUDE.md",
            "SECURITY.md",
            "CHANGELOG.md",
            "deny.toml",
            "family.toml",
            "schemas/tools.json",
            "schemas/linear.graphql",
            "docs/architecture.md",
            "docs/MCP_RESPONSE_STANDARD.md",
            "docs/FAMILY_CONTRACT.md",
            "docs/TEMPLATE_PROVENANCE.md",
        ] {
            if !self.root.join(file).is_file() {
                return Err(format!("required file absent: {file}").into());
            }
        }
        for directory in ["src", "xtask", "crates", "scripts"] {
            let base = self.root.join(directory);
            if !base.is_dir() {
                continue;
            }
            for entry in walk(&base)? {
                if matches!(
                    entry.extension().and_then(|s| s.to_str()),
                    Some("py" | "js" | "mjs" | "ts" | "rb")
                ) {
                    return Err(
                        format!("non-Rust tooling source remains: {}", entry.display()).into(),
                    );
                }
            }
        }
        println!("standard: structural checks passed (not a semantic or security certificate)");
        Ok(())
    }

    /// Verify the authoritative schema file structurally, then run the Rust
    /// contract tests that compare it with the embedded catalogue, the
    /// gateway dispatch vocabulary and real-binary MCP discovery.
    fn contract(&self) -> Result<()> {
        let path = self.root.join("schemas/tools.json");
        let tools: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let Some(tools) = tools.as_array() else {
            return Err("schemas/tools.json must be an array of tools".into());
        };
        let mut names: Vec<&str> = Vec::new();
        for tool in tools {
            let name = tool["name"].as_str().ok_or("tool entry without a name")?;
            if name.is_empty()
                || !name.as_bytes()[0].is_ascii_lowercase()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(format!("tool name violates the family pattern: {name}").into());
            }
            if !tool["description"].is_string()
                || tool["inputSchema"]["type"] != "object"
                || !tool["inputSchema"]["additionalProperties"].is_boolean()
            {
                return Err(format!("incomplete discovery contract: {name}").into());
            }
            for hint in [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint",
            ] {
                if !tool["annotations"][hint].is_boolean() {
                    return Err(format!("missing truthful annotation {hint}: {name}").into());
                }
            }
            names.push(name);
        }
        if names.len()
            != names
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        {
            return Err("duplicate tool names in the catalogue".into());
        }
        println!("contract: {} tools structurally valid", names.len());
        run(
            &self.root,
            "cargo",
            &[
                "test",
                "--frozen",
                "--package",
                &self.name,
                "--test",
                "contract",
            ],
        )
    }

    fn check(&self) -> Result<()> {
        self.standard()?;
        run(&self.root, "cargo", &["fmt", "--all", "--check"])?;
        for args in [
            vec![
                "clippy",
                "--frozen",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            vec![
                "clippy",
                "--frozen",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
            vec!["test", "--frozen", "--workspace"],
            vec!["test", "--frozen", "--workspace", "--all-features"],
        ] {
            run(&self.root, "cargo", &args)?;
        }
        if !helper(&self.root, "cargo")
            .args([
                "doc",
                "--frozen",
                "--workspace",
                "--all-features",
                "--no-deps",
            ])
            .env("RUSTDOCFLAGS", "-D warnings")
            .status()?
            .success()
        {
            return Err("rustdoc failed".into());
        }
        self.contract()
    }
}

/// Recursively list real files under one directory, refusing links.
fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err("managed trees must not contain symlinks".into());
        }
        if kind.is_dir() {
            out.extend(walk(&path)?);
        } else if kind.is_file() {
            out.push(path);
        } else {
            return Err("special managed file".into());
        }
    }
    Ok(out)
}

fn main_result() -> Result<()> {
    let project = Project::load()?;
    match Cli::parse().command {
        Task::Prepare => run(&project.root, "cargo", &["fetch", "--locked"]),
        Task::Check => project.check(),
        Task::Standard { command: _ } => project.standard(),
        Task::Contract { command: _ } => project.contract(),
        Task::Test { suite } => match suite.as_str() {
            "contract" => run(
                &project.root,
                "cargo",
                &[
                    "test",
                    "--frozen",
                    "--package",
                    &project.name,
                    "--test",
                    "contract",
                ],
            ),
            "protocol" => run(
                &project.root,
                "cargo",
                &[
                    "test",
                    "--frozen",
                    "--package",
                    &project.name,
                    "--test",
                    "transport",
                ],
            ),
            other => Err(format!("unknown test suite: {other}").into()),
        },
    }
}

fn main() -> ExitCode {
    match main_result() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "Test assertions")]
mod tests {
    #[test]
    fn scaffold_state_profile_is_external_not_none() {
        let family: toml::Value = toml::from_str(include_str!("../../family.toml")).unwrap();
        // The delivery manifest's state_schema=0 means no LOCAL business state.
        // The family profile itself must stay honestly `external`.
        assert_eq!(family["profiles"]["process"].as_str(), Some("resident"));
        assert_eq!(family["profiles"]["state"].as_str(), Some("external"));
    }
}
