//! Bounded read-only local Git access and structured commit-message reports.

use crate::model::{Fault, Result, require};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// Immutable source snapshot read from one concrete Git commit; no workflow state is inferred.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitCommit {
    /// Canonical common Git directory shared by a repository's linked worktrees.
    pub repository_identity: String,
    /// Canonical local checkout used to read this snapshot.
    pub repository_path: String,
    /// Full object ID, resolved from an unambiguous hexadecimal hash.
    pub sha: String,
    /// Original first line, validated as a Conventional Commit subject.
    pub subject: String,
    /// Exact UTF-8 message bytes from the commit object, including trailing newlines.
    pub original_message: String,
    /// Nonempty Result section with only surrounding whitespace removed.
    pub result: String,
    /// Nonempty Checks section; these are author claims, not independent verification.
    pub checks: String,
    /// Optional nonempty Notes section.
    pub notes: Option<String>,
    /// Git author name and email as reported by Git.
    pub author: String,
    /// Git author's ISO 8601 timestamp, including its recorded offset.
    pub authored_at: String,
}

/// Execute a prepared local command with bounded stdout and elapsed time, returning its bytes.
/// Stdin/stderr are discarded. Errors kill and reap the child; stdout is drained on a bounded
/// reader thread so large output cannot deadlock the process. No shell is constructed here.
fn run(command: &mut Command, limit: usize, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Fault::new("GIT_UNAVAILABLE", "Could not start local Git"))?;
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(result);
    });
    let started = Instant::now();
    let result = (|| {
        let bytes = receiver
            .recv_timeout(timeout)
            .map_err(|_| Fault::new("GIT_TIMEOUT", "Local Git exceeded its process deadline"))?
            .map_err(|_| Fault::new("GIT_FAILED", "Could not read local Git output"))?;
        require(
            bytes.len() <= limit,
            "GIT_OUTPUT_LIMIT",
            "Local Git output exceeds the byte limit",
        )?;
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|_| Fault::new("GIT_FAILED", "Could not wait for local Git"))?
            {
                require(
                    status.success(),
                    "GIT_FAILED",
                    "Local Git rejected the repository or object",
                )?;
                return Ok(bytes);
            }
            require(
                started.elapsed() < timeout,
                "GIT_TIMEOUT",
                "Local Git exceeded its process deadline",
            )?;
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

/// Read literal Git arguments in an absolute existing directory, ignoring inherited repository
/// overrides and replacement objects. Limits each process to two seconds and 64 KiB of UTF-8 output.
/// No command here writes Git state or contacts a remote; invalid paths/encoding remain explicit.
fn git(path: &str, args: &[&str]) -> Result<String> {
    require(
        Path::new(path).is_absolute() && Path::new(path).is_dir(),
        "INVALID_REPOSITORY",
        format!("Local Git path must be an absolute existing directory: {path}"),
    )?;
    let bytes = run(
        Command::new("git")
            .args(["--no-replace-objects", "-C", path])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_OPTIONAL_LOCKS", "0"),
        65_536,
        Duration::from_secs(2),
    )?;
    String::from_utf8(bytes).map_err(|_| Fault::new("GIT_ENCODING", "Git output is not UTF-8"))
}

/// Require an absolute existing directory inside a non-bare Git working tree.
/// Uses the bounded read-only Git runner; normal repositories and linked worktrees are accepted.
pub fn validate_repository(path: &str) -> Result<()> {
    git(path, &["rev-parse", "--show-toplevel"])
        .map(|_| ())
        .map_err(|e| {
            if e.code == "GIT_FAILED" {
                Fault::new(
                    "INVALID_REPOSITORY",
                    format!("Not a readable Git working tree: {path}"),
                )
            } else {
                e
            }
        })
}

/// Return a validated checkout's canonical common Git directory for worktree-independent identity.
/// Resolves local symlinks; inaccessible or non-UTF-8 paths fail without changing the repository.
pub fn repository_identity(path: &str) -> Result<String> {
    validate_repository(path)?;
    let common = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = common.strip_suffix('\n').unwrap_or(&common);
    std::fs::canonicalize(common)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .ok_or_else(|| {
            Fault::new(
                "INVALID_REPOSITORY",
                "Cannot resolve the common Git directory",
            )
        })
}

/// Parse a Conventional Commit message into subject, Result, Checks and optional Notes.
/// Result and Checks must each occur once and contain text; duplicate/empty sections, malformed
/// subjects and NUL bytes are rejected. Section text is preserved except surrounding whitespace.
pub fn parse_message(message: &str) -> Result<(String, String, String, Option<String>)> {
    let subject = message.lines().next().unwrap_or("");
    let valid_subject = subject.split_once(": ").is_some_and(|(prefix, body)| {
        let prefix = prefix.strip_suffix('!').unwrap_or(prefix);
        let (kind, scope_ok) = match prefix.split_once('(') {
            Some((kind, scope)) => (
                kind,
                scope
                    .strip_suffix(')')
                    .is_some_and(|s| !s.is_empty() && !s.contains(['(', ')'])),
            ),
            None => (prefix, true),
        };
        scope_ok
            && !kind.is_empty()
            && kind
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !body.trim().is_empty()
    });
    require(
        valid_subject && !message.contains('\0'),
        "INVALID_COMMIT_MESSAGE",
        "Expected a Conventional Commit subject",
    )?;
    let mut sections: [Option<String>; 3] = [None, None, None];
    let mut current = None;
    for line in message.lines().skip(1) {
        if let Some(index) = ["Result:", "Checks:", "Notes:"]
            .iter()
            .position(|header| line.trim() == *header)
        {
            require(
                sections[index].is_none(),
                "INVALID_COMMIT_MESSAGE",
                "Duplicate report section",
            )?;
            sections[index] = Some(String::new());
            current = Some(index);
        } else if let Some(index) = current {
            let section = sections[index].as_mut().unwrap();
            section.push_str(line);
            section.push('\n');
        }
    }
    let [result, checks, notes] = sections.map(|s| s.map(|v| v.trim().to_owned()));
    require(
        result.as_ref().is_some_and(|s| !s.is_empty())
            && checks.as_ref().is_some_and(|s| !s.is_empty())
            && notes.as_ref().is_none_or(|s| !s.is_empty()),
        "INVALID_COMMIT_MESSAGE",
        "Result and Checks must be nonempty; Notes may be omitted",
    )?;
    Ok((subject.to_owned(), result.unwrap(), checks.unwrap(), notes))
}

/// Read one unambiguous 4–64 digit hexadecimal object hash from a local checkout.
/// Branch/ref expressions are rejected. Captures exact UTF-8 message bytes and Git author data;
/// absent/ambiguous/non-commit objects, malformed reports and runner limits return explicit faults.
pub fn read_commit(path: &str, hash: &str) -> Result<GitCommit> {
    require(
        (4..=64).contains(&hash.len()) && hash.bytes().all(|c| c.is_ascii_hexdigit()),
        "INVALID_COMMIT",
        "Use a concrete hexadecimal commit hash, not a branch or revision expression",
    )?;
    let repository_identity = repository_identity(path)?;
    let objects = git(path, &["rev-parse", &format!("--disambiguate={hash}")])?;
    let objects: Vec<_> = objects.lines().collect();
    require(
        objects.len() == 1,
        "INVALID_COMMIT",
        "Commit hash is missing or ambiguous",
    )?;
    let sha = objects[0];
    require(
        git(path, &["cat-file", "-t", sha])?.trim() == "commit",
        "INVALID_COMMIT",
        "Hash must identify a commit object",
    )?;
    let raw = git(path, &["cat-file", "commit", sha])?;
    let (_, message) = raw
        .split_once("\n\n")
        .ok_or_else(|| Fault::new("INVALID_COMMIT", "Commit lacks a message boundary"))?;
    let (subject, result, checks, notes) = parse_message(message)?;
    let author = git(
        path,
        &[
            "show",
            "--no-patch",
            "--no-notes",
            "--no-show-signature",
            "--format=format:%an <%ae>%x00%aI",
            sha,
        ],
    )?;
    let (author, authored_at) = author
        .split_once('\0')
        .ok_or_else(|| Fault::new("INVALID_COMMIT", "Commit lacks author metadata"))?;
    let repository_path = std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .ok_or_else(|| Fault::new("INVALID_REPOSITORY", "Cannot resolve checkout path"))?;
    Ok(GitCommit {
        repository_identity,
        repository_path,
        sha: sha.into(),
        subject,
        original_message: message.into(),
        result,
        checks,
        notes,
        author: author.into(),
        authored_at: authored_at.into(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    //! Direct process-boundary checks using small native commands, with no shell or Git writes.
    use super::*;
    /// Output, time and missing-executable failures remain distinct and bounded.
    #[test]
    fn process_limits_are_enforced() {
        assert_eq!(
            run(
                Command::new("printf").args(["%s", "too much"]),
                3,
                Duration::from_secs(1)
            )
            .unwrap_err()
            .code,
            "GIT_OUTPUT_LIMIT"
        );
        assert_eq!(
            run(
                Command::new("sleep").arg("2"),
                10,
                Duration::from_millis(20)
            )
            .unwrap_err()
            .code,
            "GIT_TIMEOUT"
        );
        assert_eq!(
            run(
                &mut Command::new("/nonexistent/git-test-command"),
                10,
                Duration::from_secs(1)
            )
            .unwrap_err()
            .code,
            "GIT_UNAVAILABLE"
        );
    }
}
