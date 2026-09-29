//! Real-binary CLI checks: family-standard vocabulary (mcp/init/config/doctor)
//! and legacy aliases stay compatible, and the local doctor runs without any
//! Linear credential or network access.

#[allow(dead_code)]
mod support;

use agent_tasks::config::Config;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use support::{id, run_cli};

/// `--version` reports one product identity sourced from Cargo.
#[test]
fn version_reports_product_identity() {
    let output = run_cli(&["--version"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        text.trim(),
        format!("agent-tasks {}", env!("CARGO_PKG_VERSION"))
    );
}

/// Local doctor works without credentials or network: exit 0, machine JSON,
/// and no secret material in the report.
#[test]
fn local_doctor_needs_no_credentials_or_network() {
    let config_path = std::env::temp_dir().join(format!("atl-cli-doctor-{}.toml", id()));
    let output = run_cli(&[
        "--config",
        config_path.to_str().unwrap(),
        "doctor",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{:?}", output);
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["product"], "agent-tasks");
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["online"], false);
    let names: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for expected in [
        "catalog",
        "presentation",
        "config",
        "linear_credentials",
        "git",
    ] {
        assert!(names.contains(&expected), "{names:?}");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("lin_api_"), "credential-looking text leaked");
    let config = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "config")
        .unwrap();
    assert_eq!(config["status"], "warning");
}

/// An invalid protected config is a failed local check (exit 2), never a
/// silent pass; `config check` refuses it locally without network too.
#[test]
fn invalid_config_fails_local_doctor_and_config_check() {
    let bad = std::env::temp_dir().join(format!("atl-cli-bad-{}.toml", id()));
    std::fs::write(&bad, b"listen = \"not-an-address\"\n").unwrap();
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
    let doctor = run_cli(&["--config", bad.to_str().unwrap(), "doctor", "--json"]);
    assert_eq!(doctor.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&doctor.stdout).expect("doctor JSON");
    let config = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "config")
        .unwrap();
    assert_eq!(config["status"], "failed");
    let check = run_cli(&["--config", bad.to_str().unwrap(), "config", "check"]);
    assert_eq!(check.status.code(), Some(2));
    std::fs::remove_file(bad).unwrap();
}

/// A valid protected config passes `config check` and the local doctor.
#[test]
fn valid_config_passes_local_checks() {
    let path = std::env::temp_dir().join(format!("atl-cli-ok-{}.toml", id()));
    Config::initialize(&path).unwrap();
    let check = run_cli(&["--config", path.to_str().unwrap(), "config", "check"]);
    assert!(check.status.success(), "{:?}", check);
    let doctor = run_cli(&["--config", path.to_str().unwrap(), "doctor", "--json"]);
    assert_eq!(doctor.status.code(), Some(0));
    std::fs::remove_file(path).unwrap();
}

/// `doctor --online` without a credential fails fast with a clear local
/// refusal instead of attempting network access.
#[test]
fn online_doctor_without_credentials_refuses_locally() {
    let config_path = std::env::temp_dir().join(format!("atl-cli-online-{}.toml", id()));
    let output = run_cli(&[
        "--config",
        config_path.to_str().unwrap(),
        "doctor",
        "--json",
        "--online",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    let viewer = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "linear_viewer")
        .unwrap();
    assert_eq!(viewer["status"], "failed");
    assert_eq!(report["online"], true);
}

/// `init` is the standard alias of `init-config` and never overwrites an
/// existing credential file.
#[test]
fn init_alias_creates_but_never_overwrites() {
    let path = std::env::temp_dir().join(format!("atl-cli-init-{}.toml", id()));
    let first = run_cli(&["--config", path.to_str().unwrap(), "init"]);
    assert!(first.status.success(), "{:?}", first);
    assert!(path.is_file());
    let before = std::fs::read(&path).unwrap();
    let second = run_cli(&["--config", path.to_str().unwrap(), "init"]);
    assert!(!second.status.success());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_file(path).unwrap();
}
