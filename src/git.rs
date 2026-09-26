//! Read-only validation of local Git checkouts, including linked worktrees.

use crate::model::{Fault, Result, require};
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Require an absolute existing directory inside a non-bare Git working tree.
/// Runs only `git rev-parse --show-toplevel` with literal arguments, discards output,
/// ignores inherited repository overrides and kills/reaps Git after two seconds.
/// Does not change files, refs or the index or contact a remote. Missing Git,
/// invalid directories/repositories and process failures return explicit faults.
pub fn validate_repository(path: &str) -> Result<()> {
    require(
        Path::new(path).is_absolute() && Path::new(path).is_dir(),
        "INVALID_REPOSITORY",
        format!("Local Git path must be an absolute existing directory: {path}"),
    )?;
    let mut child = Command::new("git")
        .args(["-C", path, "rev-parse", "--show-toplevel"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Fault::new("GIT_UNAVAILABLE", "Could not start local Git"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return require(
                    status.success(),
                    "INVALID_REPOSITORY",
                    format!("Not a readable Git working tree: {path}"),
                );
            }
            Ok(None) if started.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if result.is_err() {
                    Fault::new("GIT_FAILED", "Could not wait for local Git")
                } else {
                    Fault::new("GIT_TIMEOUT", "Local Git validation exceeded two seconds")
                });
            }
        }
    }
}
