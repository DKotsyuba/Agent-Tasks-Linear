//! Real local Git reader and parser checks; all Git writes are disposable test setup.
use agent_tasks::git::{parse_message, read_commit};
use std::{fs, path::Path, process::Command};

/// Run literal Git arguments in a fixture, requiring success and returning UTF-8 stdout.
fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Normal repositories and linked worktrees produce the same source identity and exact message.
#[test]
fn reads_exact_reports_without_changing_checkout() {
    let root = std::env::temp_dir().join(format!("git reports {}", uuid::Uuid::new_v4()));
    let repo = root.join("repo");
    let linked = root.join("linked");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let message = "feat(reader): read local reports\n\nResult:\n- Preserve snapshots.\n\nChecks:\n- Reader assertion passed.\n\nNotes:\n- Local only.\n";
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "--allow-empty",
            "--cleanup=verbatim",
            "-m",
            message,
        ],
    );
    let sha = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    fs::write(repo.join("dirty.txt"), "keep").unwrap();
    let status = git(&repo, &["status", "--porcelain=v1"]);
    let report = read_commit(repo.to_str().unwrap(), &sha[..12]).unwrap();
    let linked_report = read_commit(linked.to_str().unwrap(), &sha).unwrap();
    assert_eq!(report.sha, sha);
    assert_eq!(report.original_message, message);
    assert_eq!(
        report.repository_identity,
        linked_report.repository_identity
    );
    assert_eq!(report.result, "- Preserve snapshots.");
    assert_eq!(report.checks, "- Reader assertion passed.");
    assert_eq!(report.author, "Fixture <fixture@example.test>");
    assert!(chrono::DateTime::parse_from_rfc3339(&report.authored_at).is_ok());
    for invalid in ["HEAD", "HEAD~1", "--help", "deadbeefdeadbeef"] {
        assert_eq!(
            read_commit(repo.to_str().unwrap(), invalid)
                .unwrap_err()
                .code,
            "INVALID_COMMIT"
        );
    }
    assert_eq!(git(&repo, &["status", "--porcelain=v1"]), status);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), sha);
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "fix: missing checks\n\nResult:\nChanged",
        ],
    );
    let invalid = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(
        read_commit(repo.to_str().unwrap(), invalid.trim())
            .unwrap_err()
            .code,
        "INVALID_COMMIT_MESSAGE"
    );
    fs::remove_dir_all(root).unwrap();
}

/// Malformed and duplicate sections fail; Notes may be absent without losing Checks text.
#[test]
fn parser_requires_meaningful_sections() {
    assert!(
        parse_message("fix: repair\n\nResult:\nDone\n\nChecks:\nPassed")
            .unwrap()
            .3
            .is_none()
    );
    for invalid in [
        "No type\nResult:\nDone\nChecks:\nPassed",
        "fix: repair\nResult:\n\nChecks:\nPassed",
        "fix: repair\nResult:\nDone\nChecks:\nPassed\nChecks:\nAgain",
        "fix: repair\nResult:\nDone\nChecks:\nPassed\nNotes:",
    ] {
        assert_eq!(
            parse_message(invalid).unwrap_err().code,
            "INVALID_COMMIT_MESSAGE"
        );
    }
}
