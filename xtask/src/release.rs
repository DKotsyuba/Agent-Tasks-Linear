//! Explicit release operations. Local preparation never commits, tags or pushes.
//! Adapted from the family template at 7f094e0: the readiness gate is this
//! product's contract/transport/CLI suites executed against the exact payload
//! binary through MCP_TEST_BINARY, and publication refuses while family.toml
//! declares an unqualified release.
use crate::{Project, Result, capture, run, text};
use clap::Subcommand;
use family_delivery::Manifest;
use serde_json::{Value, json};
use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{fs, path::PathBuf};

#[derive(Subcommand)]
pub enum Release {
    /// Show version/CHANGELOG edits; --apply performs local edits only.
    Prepare {
        /// Target semver version, e.g. 0.5.0.
        version: String,
        /// Apply the local edits instead of only previewing them.
        #[arg(long)]
        apply: bool,
    },
    /// Publish an accepted CI bundle through a complete draft; never replace a release.
    Publish {
        /// Verified bundle directory built from the exact accepted commit.
        directory: PathBuf,
    },
    /// Observe one exact tag/commit and validate its run and bytes. No install or wake claim.
    Wait {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        tag: String,
        #[arg(long)]
        commit: String,
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
        #[arg(long)]
        result_file: Option<PathBuf>,
    },
}

fn repo_valid(repo: &str) -> bool {
    let parts: Vec<_> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 100
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

/// Accept both `## Unreleased` and the bracketed Keep-a-Changelog heading form.
fn unreleased_heading(changelog: &str) -> Result<&'static str> {
    ["## Unreleased", "## [Unreleased]"]
        .into_iter()
        .find(|h| changelog.matches(h).count() == 1)
        .ok_or("expected exactly one Unreleased section".into())
}

/// Replace the `version` key of `[workspace.package]` in place, preserving key
/// order, inline tables and comments; None when that exact line is absent.
fn bump_workspace_version(source: &str, from: &str, to: &str) -> Option<String> {
    let mut in_package = false;
    let mut bumped = false;
    let mut out = source
        .lines()
        .map(|line| {
            if line.trim_start().starts_with('[') {
                in_package = line.trim() == "[workspace.package]";
            }
            if in_package && !bumped && line.trim() == format!("version = \"{from}\"") {
                bumped = true;
                format!("version = \"{to}\"")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if source.ends_with('\n') {
        out.push('\n');
    }
    bumped.then_some(out)
}

fn prepare(project: &Project, version: &str, apply: bool) -> Result<()> {
    let old = semver::Version::parse(&project.version)?;
    let new = semver::Version::parse(version)?;
    if new <= old || !new.build.is_empty() {
        return Err("version must increase and cannot contain build metadata".into());
    }
    if !text(&project.root, "git", &["status", "--porcelain"])?.is_empty() {
        return Err("prepare requires a clean checkout".into());
    }
    let changelog = fs::read_to_string(project.root.join("CHANGELOG.md"))?;
    let unreleased = unreleased_heading(&changelog)?;
    println!(
        "{}",
        json!({"dry_run":!apply,"old":project.version,"new":version,
               "files":["Cargo.toml","Cargo.lock","CHANGELOG.md"],"remote_writes":0})
    );
    if !apply {
        return Ok(());
    }
    let cargo_toml = fs::read_to_string(project.root.join("Cargo.toml"))?;
    let bumped = bump_workspace_version(&cargo_toml, &project.version, version)
        .ok_or("expected the workspace version line in [workspace.package]")?;
    fs::write(project.root.join("Cargo.toml"), bumped)?;
    fs::write(
        project.root.join("CHANGELOG.md"),
        changelog.replacen(unreleased, &format!("{unreleased}\n\n## {version}"), 1),
    )?;
    capture(
        &project.root,
        "cargo",
        &[
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ],
        120,
    )?;
    run(
        &project.root,
        "cargo",
        &["update", "--workspace", "--offline"],
    )?;
    println!(
        "Review changes and Cargo.lock, run checks, commit, then create an annotated version tag explicitly."
    );
    Ok(())
}

/// Run the exact-payload acceptance suites against one binary: contract
/// (discovery/dispatch vs the committed schema), transport (real MCP calls
/// over HTTP and stdio) and CLI (doctor/config/aliases).
fn payload_tests(project: &Project, binary: &str) -> Result<()> {
    for suite in ["contract", "transport", "cli"] {
        let args = [
            "test",
            "--frozen",
            "--package",
            project.name.as_str(),
            "--test",
            suite,
        ];

        let ok = Command::new("cargo")
            .current_dir(&project.root)
            .args(args)
            .env("MCP_TEST_BINARY", binary)
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_ENTERPRISE_TOKEN")
            .env_remove("GITHUB_ENTERPRISE_TOKEN")
            .stdin(std::process::Stdio::null())
            .status()?
            .success();
        if !ok {
            return Err(format!("shipped-payload {suite} acceptance failed").into());
        }
    }
    Ok(())
}

fn publish(project: &Project, directory: PathBuf) -> Result<()> {
    let directory = fs::canonicalize(directory)?;
    let m = family_delivery::verify(&directory)?;
    let tag = format!("v{}", m.version);
    let repo = project.family["repository"]
        .as_str()
        .ok_or("repository missing")?;
    // Qualification flags are reviewed declarations backed by separate native
    // host evidence; they are never flipped to make a pipeline green.
    if !repo_valid(repo)
        || project.family["release"]["enabled"].as_bool() != Some(true)
        || project.family["qualification"].as_str() != Some("verified")
        || !project.family["compatibility"]["qualified_targets"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(m.target.as_str())))
        || !project.family["compatibility"]["qualified_hosts"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    {
        return Err("release is disabled or lacks reviewed native/host qualification".into());
    }
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
        || std::env::var("GITHUB_REPOSITORY").as_deref() != Ok(repo)
        || std::env::var("GITHUB_REF_NAME").as_deref() != Ok(tag.as_str())
        || std::env::var("GITHUB_SHA").as_deref() != Ok(m.source_commit.as_str())
        || std::env::var("GITHUB_RUN_ID")
            .ok()
            .and_then(|v| v.parse().ok())
            != m.run_id
        || std::env::var("GITHUB_RUN_ATTEMPT")
            .ok()
            .and_then(|v| v.parse().ok())
            != m.run_attempt
        || m.run_id.is_none()
        || m.product != project.name
        || m.version != project.version
    {
        return Err("CI/tag/source/run identity mismatch".into());
    }
    if text(&project.root, "git", &["cat-file", "-t", &tag])? != "tag"
        || text(&project.root, "git", &["rev-parse", &format!("{tag}^{{}}")])? != m.source_commit
        || !text(&project.root, "git", &["status", "--porcelain"])?.is_empty()
    {
        return Err("release requires clean exact source and an annotated immutable tag".into());
    }
    let binary = directory.join(&m.binary);
    let binary_str = binary.to_str().ok_or("non-UTF8 binary path")?;
    if text(&project.root, binary_str, &["--version"])? != format!("{} {}", m.product, m.version) {
        return Err("packaged binary identity mismatch".into());
    }
    if !Command::new("cargo")
        .current_dir(&project.root)
        .args(["deny", "--locked", "check"])
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .stdin(std::process::Stdio::null())
        .status()?
        .success()
    {
        return Err("supply-chain check failed for the release source".into());
    }
    payload_tests(project, binary_str)?;
    if family_delivery::verify(&directory)? != m {
        return Err("payload changed after acceptance".into());
    }
    let changelog = fs::read_to_string(project.root.join("CHANGELOG.md"))?;
    let headings = [format!("## {}", m.version), format!("## [{}]", m.version)];
    let section = changelog
        .lines()
        .skip_while(|l| headings.iter().all(|h| *l != h.as_str()))
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    if section.trim().is_empty() {
        return Err("release CHANGELOG section is empty".into());
    }
    let notes = project.root.join("target/release-notes.md");
    fs::write(&notes, section)?;
    let manifest_path = directory.join("release-manifest.json");
    let installer = project.root.join("install.sh");
    let sums_path = project.root.join("target/SHA256SUMS");
    let mut sums = String::new();
    for (name, file) in [
        (m.binary.as_str(), &binary),
        ("release-manifest.json", &manifest_path),
        ("install.sh", &installer),
    ] {
        sums.push_str(&format!("{}  {name}\n", family_delivery::digest(file)?.1));
    }
    fs::write(&sums_path, sums)?;
    // Existing releases (including drafts) cause create to fail. No --clobber path exists.
    run(
        &project.root,
        "gh",
        &[
            "release",
            "create",
            &tag,
            binary_str,
            manifest_path.to_str().ok_or("manifest path")?,
            installer.to_str().ok_or("installer path")?,
            sums_path.to_str().ok_or("checksum path")?,
            "--repo",
            repo,
            "--verify-tag",
            "--draft",
            "--title",
            &tag,
            "--notes-file",
            notes.to_str().ok_or("notes path")?,
        ],
    )?;
    let verify_dir = project
        .root
        .join(format!("target/upload-verify-{}", std::process::id()));
    fs::create_dir(&verify_dir)?;
    run(
        &project.root,
        "gh",
        &[
            "release",
            "download",
            &tag,
            "--repo",
            repo,
            "--dir",
            verify_dir.to_str().ok_or("verify path")?,
            "--pattern",
            &m.binary,
            "--pattern",
            "release-manifest.json",
        ],
    )?;
    if family_delivery::verify(&verify_dir)? != m {
        return Err("uploaded payload differs; draft left unpublished".into());
    }
    let bootstrap_dir = project
        .root
        .join(format!("target/bootstrap-verify-{}", std::process::id()));
    fs::create_dir(&bootstrap_dir)?;
    run(
        &project.root,
        "gh",
        &[
            "release",
            "download",
            &tag,
            "--repo",
            repo,
            "--dir",
            bootstrap_dir.to_str().ok_or("verify path")?,
            "--pattern",
            "install.sh",
            "--pattern",
            "SHA256SUMS",
        ],
    )?;
    if fs::read(bootstrap_dir.join("install.sh"))? != fs::read(&installer)?
        || fs::read(bootstrap_dir.join("SHA256SUMS"))? != fs::read(&sums_path)?
    {
        return Err("bootstrap assets differ; draft left unpublished".into());
    }
    let mut args = vec!["release", "edit", &tag, "--repo", repo, "--draft=false"];
    if !semver::Version::parse(&m.version)?.pre.is_empty() {
        args.push("--prerelease");
    }
    run(&project.root, "gh", &args)?;
    println!(
        "{}",
        json!({"status":"published","tag":tag,"commit":m.source_commit,
               "artifact_integrity":"verified","provenance_verification":"not_performed"})
    );
    Ok(())
}

fn wait(
    project: &Project,
    repo: &str,
    tag: &str,
    commit: &str,
    timeout: u64,
    result_file: Option<PathBuf>,
) -> Result<()> {
    if !repo_valid(repo)
        || commit.len() != 40
        || !commit.bytes().all(|b| b.is_ascii_hexdigit())
        || timeout == 0
        || timeout > 86400
    {
        return Err("invalid release wait scope or deadline".into());
    }
    let version = semver::Version::parse(tag.strip_prefix('v').ok_or("tag must start with v")?)?;
    if !version.build.is_empty() {
        return Err("release build metadata is unsupported".into());
    }
    fs::create_dir_all(&project.target_dir)?;
    let dir = project
        .target_dir
        .join(format!("release-wait-{}", std::process::id()));
    fs::create_dir(&dir)?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let remaining = || -> Result<u64> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            Err("release wait deadline exceeded".into())
        } else {
            Ok(left.as_secs().clamp(1, 60))
        }
    };
    loop {
        remaining()?;
        let response = capture(
            &project.root,
            "gh",
            &[
                "api",
                "--hostname",
                "github.com",
                &format!("repos/{repo}/releases/tags/{tag}"),
            ],
            remaining()?,
        );
        let data: Value = match response {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(
                    2.min(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .as_secs()
                            .max(1),
                    ),
                ));
                continue;
            }
        };
        if data["draft"] != false {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let assets = data["assets"]
            .as_array()
            .ok_or("release asset inventory absent")?;
        let meta = assets
            .iter()
            .find(|a| a["name"] == "release-manifest.json")
            .ok_or("published manifest absent")?;
        if meta["size"].as_u64().is_none_or(|s| s > 16384) {
            return Err("published manifest too large".into());
        }
        let meta_id = meta["id"].as_u64().ok_or("manifest asset ID missing")?;
        let bytes = capture(
            &project.root,
            "gh",
            &[
                "api",
                "--hostname",
                "github.com",
                &format!("repos/{repo}/releases/assets/{meta_id}"),
                "-H",
                "Accept: application/octet-stream",
            ],
            remaining()?,
        )?;
        let m: Manifest = serde_json::from_slice(&bytes)?;
        m.validate()?;
        if m.source_commit != commit || format!("v{}", m.version) != tag {
            return Err("release manifest identity mismatch".into());
        }
        let run_id = m.run_id.ok_or("release has no CI run identity")?;
        let run: Value = serde_json::from_slice(&capture(
            &project.root,
            "gh",
            &[
                "api",
                "--hostname",
                "github.com",
                &format!("repos/{repo}/actions/runs/{run_id}"),
            ],
            remaining()?,
        )?)?;
        if run["head_sha"] != commit
            || run["run_attempt"].as_u64() != m.run_attempt
            || run["event"] != "push"
            || run["path"] != ".github/workflows/release.yml"
        {
            return Err("release workflow identity mismatch".into());
        }
        if run["status"] != "completed" {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        if run["conclusion"] != "success" {
            return Err("release workflow failed".into());
        }
        let mut object = serde_json::from_slice::<Value>(&capture(
            &project.root,
            "gh",
            &[
                "api",
                "--hostname",
                "github.com",
                &format!("repos/{repo}/git/ref/tags/{tag}"),
            ],
            remaining()?,
        )?)?["object"]
            .clone();
        for _ in 0..4 {
            if object["type"] == "commit" {
                break;
            }
            if object["type"] != "tag" {
                return Err("unsupported tag target".into());
            }
            let sha = object["sha"].as_str().ok_or("tag SHA missing")?;
            object = serde_json::from_slice::<Value>(&capture(
                &project.root,
                "gh",
                &[
                    "api",
                    "--hostname",
                    "github.com",
                    &format!("repos/{repo}/git/tags/{sha}"),
                ],
                remaining()?,
            )?)?["object"]
                .clone();
        }
        if object["type"] != "commit" || object["sha"] != commit {
            return Err("tag no longer resolves to expected source".into());
        }
        let asset = assets
            .iter()
            .find(|a| a["name"] == m.binary)
            .ok_or("binary asset absent")?;
        if asset["size"].as_u64() != Some(m.size) {
            return Err("asset inventory size mismatch".into());
        }
        capture(
            &project.root,
            "gh",
            &[
                "release",
                "download",
                tag,
                "--repo",
                repo,
                "--dir",
                dir.to_str().ok_or("wait path")?,
                "--pattern",
                &m.binary,
                "--pattern",
                "release-manifest.json",
            ],
            remaining()?,
        )?;
        if family_delivery::verify(&dir)? != m {
            return Err("downloaded bytes differ from observed manifest".into());
        }
        let event = json!({"schema_version":1,"status":"published","repo":repo,"tag":tag,
            "commit":commit,"run_id":run_id,"run_attempt":m.run_attempt,
            "verified_assets":[m.binary,"release-manifest.json"],
            "artifact_integrity":"verified","provenance_verification":"not_performed",
            "installed":false,"agent_awakened":false});
        let text_json = serde_json::to_string(&event)?;
        if let Some(path) = result_file {
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(path)?;
            file.write_all(text_json.as_bytes())?;
            file.sync_all()?;
        }
        println!("{text_json}");
        return Ok(());
    }
}

/// Dispatch one release subcommand.
pub fn execute(project: &Project, command: Release) -> Result<()> {
    match command {
        Release::Prepare { version, apply } => prepare(project, &version, apply),
        Release::Publish { directory } => publish(project, directory),
        Release::Wait {
            repo,
            tag,
            commit,
            timeout,
            result_file,
        } => wait(project, &repo, &tag, &commit, timeout, result_file),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "Test assertions fail explicitly")]
mod tests {
    use super::*;
    use crate::state_schema_for;

    #[test]
    fn repository_scope() {
        assert!(repo_valid("DKotsyuba/Agent-Tasks-Linear"));
        for r in ["x", "x/y/z", "x/y?token=a", "/x", "https://github.com/x/y"] {
            assert!(!repo_valid(r));
        }
    }

    #[test]
    fn unreleased_heading_forms() {
        assert_eq!(
            unreleased_heading("# C\n\n## Unreleased\n\n### Added\n- x\n").unwrap(),
            "## Unreleased"
        );
        assert_eq!(
            unreleased_heading("# C\n\n## [Unreleased]\n\n### Added\n- x\n").unwrap(),
            "## [Unreleased]"
        );
        assert!(unreleased_heading("# C\nno headings\n").is_err());
    }

    #[test]
    fn version_bump_changes_exactly_one_line() {
        let source = "# top comment\n[workspace]\nmembers = [\"xtask\"]\n\n[workspace.package]\nversion = \"0.4.0\"\nedition = \"2024\"\n\n[dependencies]\nclap = { version = \"4.5\", features = [\"derive\"] }\n";
        let bumped = bump_workspace_version(source, "0.4.0", "0.5.0").unwrap();
        assert_eq!(
            bumped
                .lines()
                .zip(source.lines())
                .filter(|(a, b)| a != b)
                .count(),
            1
        );
        assert_eq!(
            bumped,
            source.replacen("version = \"0.4.0\"", "version = \"0.5.0\"", 1)
        );
        assert!(bump_workspace_version(source, "9.9.9", "1.0.0").is_none());
    }

    #[test]
    fn external_state_profile_maps_to_zero_local_state_schema() {
        let family: toml::Value = toml::from_str(include_str!("../../family.toml")).unwrap();
        // `external` packages with state_schema = 0: no LOCAL business state.
        assert_eq!(state_schema_for(&family).unwrap(), 0);
        assert_eq!(family["profiles"]["state"].as_str(), Some("external"));
        let none: toml::Value = toml::from_str("[profiles]\nstate = \"none\"\n").unwrap();
        assert_eq!(state_schema_for(&none).unwrap(), 0);
        let local: toml::Value = toml::from_str("[profiles]\nstate = \"local\"\n").unwrap();
        assert_eq!(state_schema_for(&local).unwrap(), 1);
        let invalid: toml::Value = toml::from_str("[profiles]\nstate = \"wat\"\n").unwrap();
        assert!(state_schema_for(&invalid).is_err());
    }
}
