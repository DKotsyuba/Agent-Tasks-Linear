//! Real-binary delivery checks against disposable homes: package, verify,
//! self-install, no-op reinstall, same-version byte conflict refusal, managed
//! launcher execution and rollback. The product binary under test honours
//! MCP_TEST_BINARY so CI can target the exact packaged payload.

#[allow(dead_code)]
mod support;

use family_delivery::{self, Manifest};
use std::path::Path;
use std::process::Command;
use support::{id, product_binary};

/// Build one real bundle from the product binary with a synthetic identity.
fn bundle(dir: &Path, version: &str, binary: &str) -> std::path::PathBuf {
    let output = dir.join(format!("bundle-{version}"));
    family_delivery::package(
        Path::new(binary),
        &output,
        Manifest {
            schema_version: 1,
            profile: "single-binary-v1".into(),
            product: "agent-tasks-linear".into(),
            version: version.into(),
            source_commit: "7f094e0463c0a6d5cf52d68d91ca32f6bc0f465f".into(),
            target: "aarch64-apple-darwin".into(),
            binary: "agent-tasks-linear-aarch64-apple-darwin".into(),
            size: 1,
            sha256: "0".repeat(64),
            state_schema: 0,
            run_id: None,
            run_attempt: None,
        },
    )
    .expect("package real binary");
    output
}

/// Run the product binary's self-install with explicit absolute paths.
fn self_install(
    binary: &str,
    bundle_path: &Path,
    home: &Path,
    bin_dir: &Path,
) -> std::process::Output {
    Command::new(binary)
        .env_remove("ATL_CONFIG")
        .env_remove("LINEAR_OAUTH_TOKEN")
        .env_remove("LINEAR_API_KEY")
        .args([
            "self-install",
            "--bundle",
            bundle_path.to_str().unwrap(),
            "--home",
            home.to_str().unwrap(),
            "--bin-dir",
            bin_dir.to_str().unwrap(),
        ])
        .output()
        .expect("run self-install")
}

/// One full disposable-home cycle through the real CLI and launcher.
#[test]
fn real_binary_installs_noops_conflicts_and_rolls_back() {
    let root = std::env::temp_dir().join(format!("atl-install-{}", id()));
    std::fs::create_dir(&root).unwrap();
    let binary = product_binary();
    let first = bundle(&root, "0.9.0", &binary);
    let verified = family_delivery::verify(&first).expect("verify real bundle");
    assert_eq!(verified.state_schema, 0);
    assert_eq!(verified.version, "0.9.0");

    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir(&bin_dir).unwrap();
    let installed = self_install(&binary, &first, &home, &bin_dir);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let launcher = bin_dir.join("agent-tasks-linear");
    assert!(launcher.is_file());
    // The managed launcher executes the installed release with a stable identity.
    let run = Command::new(&launcher)
        .arg("--version")
        .env_remove("ATL_CONFIG")
        .output()
        .unwrap();
    assert!(run.status.success());
    let reported = String::from_utf8_lossy(&run.stdout).trim().to_owned();
    assert_eq!(
        reported,
        format!("agent-tasks-linear {}", env!("CARGO_PKG_VERSION"))
    );

    // Re-installing the same version with the same bytes is a verified no-op.
    let again = self_install(&binary, &first, &home, &bin_dir);
    assert!(again.status.success());

    // A different version installs alongside; rollback reactivates the old one.
    let second = bundle(&root, "0.9.1", &binary);
    let upgraded = self_install(&binary, &second, &home, &bin_dir);
    assert!(upgraded.status.success());
    let rollback = Command::new(&binary)
        .env_remove("ATL_CONFIG")
        .args([
            "releases",
            "use",
            "0.9.0",
            "--home",
            home.to_str().unwrap(),
            "--bin-dir",
            bin_dir.to_str().unwrap(),
        ])
        .output()
        .expect("run releases use");
    assert!(rollback.status.success());
    assert_eq!(
        std::fs::read_link(home.join("standalone/current")).unwrap(),
        std::path::PathBuf::from("releases/0.9.0")
    );

    // The same version with different bytes is refused; the active install stays.
    let foreign = root.join("foreign-binary");
    std::fs::write(&foreign, b"not the product binary").unwrap();
    bundle(&root, "0.9.1-conflict", foreign.to_str().unwrap());
    // Re-badge the conflict bundle as the already-installed version.
    let rebadged = root.join("bundle-0.9.1-conflict");
    let manifest_path = rebadged.join("release-manifest.json");
    let mut manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.version = "0.9.1".into();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let refused = self_install(&binary, &rebadged, &home, &bin_dir);
    assert!(!refused.status.success());
    assert_eq!(
        std::fs::read_link(home.join("standalone/current")).unwrap(),
        std::path::PathBuf::from("releases/0.9.0")
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// An unmanaged foreign launcher is never overwritten; the explicit adoption
/// path refuses anything that does not identify as this product.
#[test]
fn foreign_launcher_is_refused_without_explicit_adoption() {
    let root = std::env::temp_dir().join(format!("atl-adopt-{}", id()));
    std::fs::create_dir(&root).unwrap();
    let binary = product_binary();
    let first = bundle(&root, "0.9.2", &binary);
    let home = root.join("home");
    let bin_dir = root.join("bin");
    std::fs::create_dir(&bin_dir).unwrap();
    std::fs::write(
        bin_dir.join("agent-tasks-linear"),
        b"#!/bin/sh\necho other-product 1.0\n",
    )
    .unwrap();
    let refused = self_install(&binary, &first, &home, &bin_dir);
    assert!(!refused.status.success());
    assert_eq!(
        std::fs::read(bin_dir.join("agent-tasks-linear")).unwrap(),
        b"#!/bin/sh\necho other-product 1.0\n"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// The managed launcher pins the product home's config: a child that changes
/// HOME still resolves the intended configuration, and a home without a
/// config keeps the binary's own ATL_CONFIG/default resolution.
#[test]
fn launcher_pins_config_against_child_home_changes() {
    let root = std::env::temp_dir().join(format!("atl-pin-{}", id()));
    std::fs::create_dir(&root).unwrap();
    let binary = product_binary();
    let first = bundle(&root, "0.9.3", &binary);
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    // A real protected config inside the product home, as the documented
    // profile supports (the owner may adopt the existing
    // ~/.config/agent-tasks-linear directory as the installation home).
    agent_tasks_linear::config::Config::initialize(&home.join("config.toml")).unwrap();
    let bin_dir = root.join("bin");
    std::fs::create_dir(&bin_dir).unwrap();
    let installed = self_install(&binary, &first, &home, &bin_dir);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let fake_home = root.join("fake-child-home");
    std::fs::create_dir(&fake_home).unwrap();
    for run in 1..=2 {
        let doctor = Command::new(bin_dir.join("agent-tasks-linear"))
            .args(["doctor", "--json"])
            .env("HOME", &fake_home)
            .env_remove("ATL_CONFIG")
            .env_remove("LINEAR_OAUTH_TOKEN")
            .env_remove("LINEAR_API_KEY")
            .output()
            .unwrap();
        assert!(doctor.status.success(), "run {run}: {doctor:?}");
        let report: serde_json::Value =
            serde_json::from_slice(&doctor.stdout).expect("doctor JSON");
        let config = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "config")
            .unwrap();
        assert_eq!(config["status"], "ok", "run {run}");
        let detail = config["detail"].as_str().unwrap();
        assert!(
            detail.contains(home.join("config.toml").to_str().unwrap()),
            "run {run}: {detail}"
        );
    }
    std::fs::remove_dir_all(&root).unwrap();
}
