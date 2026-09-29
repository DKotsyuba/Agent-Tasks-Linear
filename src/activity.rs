//! Visible native Linear activity and its typed, read-only projection.
use crate::{
    model::{Fault, Meta, Result, Review, require, text},
    records::Store,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Native entity that owns one comment.
#[derive(Debug, Clone, Serialize)]
pub struct ActivityTarget {
    /// issue, project or project_update.
    pub kind: String,
    /// Full native entity UUID.
    pub id: String,
}

/// Native comment projected for agent context without granting ordinary prose workflow authority.
#[derive(Debug, Clone, Serialize)]
pub struct ActivityRecord {
    /// Full native comment UUID.
    pub id: String,
    /// Direct native Linear permalink.
    pub url: String,
    /// Native comment owner.
    pub target: ActivityTarget,
    /// Parent comment UUID for a reply.
    pub parent_id: Option<String>,
    /// note, progress, question, decision, review, handoff or project_update; manual comments become notes.
    pub kind: String,
    /// Reported role, or user/reviewer for native manual/current review comments.
    pub role: Option<String>,
    /// Reported agent identity or native author name.
    pub actor: Option<String>,
    /// Optional reported session.
    pub session: Option<String>,
    /// Optional question addressee.
    pub recipient: Option<String>,
    /// Human content without the structured visible header.
    pub body: String,
    /// First review/progress paragraph, when applicable.
    pub summary: Option<String>,
    /// Explicit HTTP(S) source links from the visible header.
    pub source_links: Vec<String>,
    /// Native health for ProjectUpdates; absent for comments.
    pub health: Option<String>,
    /// Explicit health explanation for a formatted ProjectUpdate.
    pub reason: Option<String>,
    /// Native Linear creation time.
    pub created_at: String,
    /// Native Linear update time.
    pub updated_at: String,
    /// Native thread resolution time, absent while open.
    pub resolved_at: Option<String>,
    /// Native child comment resolving the thread, when supplied.
    pub resolving_comment_id: Option<String>,
    /// Review verdict for a marked review or current workflow review.
    pub verdict: Option<String>,
    /// Review findings, absent for other activity.
    pub findings: Option<String>,
    /// Review work round.
    pub round: Option<u64>,
    /// Review content revision.
    pub revision: Option<u64>,
    /// Only the current matching workflow review is formal.
    pub formal_review: bool,
}

/// Return the exactly one supported native target of a comment.
pub fn target(comment: &Value) -> Result<(&'static str, &str)> {
    let targets: Vec<_> = [
        ("issue", "issue"),
        ("project", "project"),
        ("project_update", "projectUpdate"),
    ]
    .into_iter()
    .filter_map(|(kind, key)| comment[key]["id"].as_str().map(|id| (kind, id)))
    .collect();
    require(
        targets.len() == 1,
        "NATIVE_STATE_MISMATCH",
        "Comment must have one supported target",
    )?;
    Ok(targets[0])
}

/// Render a visible one-line-per-field activity header. Extra fields in `meta` are optional
/// session, recipient, reviewer, verdict, round, revision and source_links; the caller stamps
/// handoff round/revision from the work record. Unsafe line breaks fail.
pub fn render(kind: &str, role: &str, actor: &str, body: &str, meta: &Value) -> Result<String> {
    require(
        matches!(
            kind,
            "note" | "progress" | "question" | "decision" | "review" | "handoff"
        ),
        "INVALID_INPUT",
        "Unknown activity kind",
    )?;
    let mut lines = vec!["Activity: v1".to_owned(), format!("Kind: {kind}")];
    for (name, value) in [("Role", role), ("Actor", actor)] {
        require(
            !value.trim().is_empty() && !value.contains('\n') && !value.contains('\r'),
            "INVALID_INPUT",
            format!("{name} must be one nonblank line"),
        )?;
        lines.push(format!("{name}: {value}"));
    }
    for (key, name) in [
        ("session", "Session"),
        ("recipient", "Recipient"),
        ("reviewer", "Reviewer"),
        ("verdict", "Verdict"),
    ] {
        if let Some(value) = meta[key].as_str() {
            require(
                !value.trim().is_empty() && !value.contains('\n') && !value.contains('\r'),
                "INVALID_INPUT",
                format!("{name} must be one nonblank line"),
            )?;
            lines.push(format!("{name}: {value}"));
        }
    }
    for (key, name) in [("round", "Round"), ("revision", "Revision")] {
        if let Some(value) = meta[key].as_u64() {
            lines.push(format!("{name}: {value}"));
        }
    }
    if let Some(sources) = meta["source_links"].as_array() {
        for source in sources {
            let source = source
                .as_str()
                .ok_or_else(|| Fault::new("INVALID_INPUT", "Source must be a URL"))?;
            require(
                (source.starts_with("https://") || source.starts_with("http://"))
                    && !source.contains('\n')
                    && !source.contains('\r'),
                "INVALID_INPUT",
                "Source must be one HTTP(S) URL",
            )?;
            lines.push(format!("Source: {source}"));
        }
    }
    require(
        !body.trim().is_empty(),
        "INVALID_INPUT",
        "Activity body is empty",
    )?;
    Ok(format!("{}\n\n{}", lines.join("\n"), body.trim()))
}

/// Project one native comment; only the attachment's matching current review may approve work.
pub fn record(comment: &Value, current_review: Option<&Review>) -> Result<ActivityRecord> {
    let (target_kind, target_id) = target(comment)?;
    let id = text(comment, "id")?;
    let native_body = comment["body"].as_str().unwrap_or("");
    let (header, content) = native_body.split_once("\n\n").unwrap_or(("", native_body));
    let mut header_lines = header.lines();
    let marked = header_lines.next() == Some("Activity: v1");
    let mut values = BTreeMap::new();
    let mut sources = vec![];
    if marked {
        for line in header_lines {
            if let Some((key, value)) = line.split_once(": ") {
                if key == "Source" {
                    sources.push(value.to_owned());
                } else {
                    values.insert(key, value);
                }
            }
        }
    }
    let value = |key| values.get(key).copied();
    let current = current_review.filter(|review| review.id == id);
    let kind = if current.is_some() {
        "review"
    } else if marked
        && matches!(
            value("Kind"),
            Some("note" | "progress" | "question" | "decision" | "review" | "handoff")
        )
    {
        value("Kind").unwrap()
    } else {
        "note"
    };
    let body = if marked { content } else { native_body };
    let findings = if kind == "review" {
        body.split_once("\n\n### Findings\n")
            .or_else(|| body.split_once("\n### Замечания\n"))
            .map(|(_, tail)| {
                tail.split("\n\n### ")
                    .next()
                    .unwrap_or(tail)
                    .trim()
                    .to_owned()
            })
    } else {
        None
    };
    let summary = if matches!(kind, "review" | "progress") {
        Some(
            body.split("\n\n### ")
                .next()
                .unwrap_or(body)
                .trim()
                .to_owned(),
        )
    } else {
        None
    };
    Ok(ActivityRecord {
        id: id.into(),
        url: text(comment, "url")?.into(),
        target: ActivityTarget {
            kind: target_kind.into(),
            id: target_id.into(),
        },
        parent_id: comment["parent"]["id"].as_str().map(str::to_owned),
        kind: kind.into(),
        role: value("Role").map(str::to_owned).or_else(|| {
            Some(
                if current.is_some() {
                    "reviewer"
                } else {
                    "user"
                }
                .into(),
            )
        }),
        actor: value("Actor")
            .map(str::to_owned)
            .or_else(|| comment["user"]["name"].as_str().map(str::to_owned)),
        session: value("Session").map(str::to_owned),
        recipient: value("Recipient").map(str::to_owned),
        body: body.into(),
        summary,
        source_links: sources,
        health: None,
        reason: None,
        created_at: text(comment, "createdAt")?.into(),
        updated_at: text(comment, "updatedAt")?.into(),
        resolved_at: comment["resolvedAt"].as_str().map(str::to_owned),
        resolving_comment_id: comment["resolvingCommentId"].as_str().map(str::to_owned),
        verdict: current
            .map(|r| {
                if r.accepted {
                    "accepted"
                } else {
                    "changes_requested"
                }
                .into()
            })
            .or_else(|| {
                (kind == "review")
                    .then(|| value("Verdict").map(str::to_owned))
                    .flatten()
            }),
        findings,
        round: current
            .map(|r| r.round)
            .or_else(|| value("Round").and_then(|s| s.parse().ok())),
        revision: current
            .map(|r| r.revision)
            .or_else(|| value("Revision").and_then(|s| s.parse().ok())),
        formal_review: current.is_some(),
    })
}

/// Render one ProjectUpdate body with visible author and single-line health explanation.
/// The caller supplies the selected native health separately; this function performs no write.
pub fn render_project_update(actor: &str, reason: &str, body: &str) -> Result<String> {
    for (name, value) in [("actor", actor), ("reason", reason)] {
        require(
            !value.trim().is_empty() && !value.contains(['\n', '\r']),
            "INVALID_INPUT",
            format!("{name} must be one nonblank line"),
        )?;
    }
    require(
        !body.trim().is_empty(),
        "INVALID_INPUT",
        "Project update body is empty",
    )?;
    Ok(format!(
        "Author: {actor}\nReason: {reason}\n\n{}",
        body.trim()
    ))
}

/// Map a native ProjectUpdate to the same activity contract while preserving native health.
/// Unformatted updates retain their full body and native user; no comment or workflow state is read.
pub fn project_update_record(update: &Value) -> Result<ActivityRecord> {
    let native_body = update["body"].as_str().unwrap_or("");
    let (header, content) = native_body.split_once("\n\n").unwrap_or(("", native_body));
    let mut lines = header.lines();
    let actor = lines.next().and_then(|line| line.strip_prefix("Author: "));
    let reason = lines.next().and_then(|line| line.strip_prefix("Reason: "));
    let formatted = actor.is_some() && reason.is_some();
    let body = if formatted { content } else { native_body };
    Ok(ActivityRecord {
        id: text(update, "id")?.into(),
        url: text(update, "url")?.into(),
        target: ActivityTarget {
            kind: "project".into(),
            id: text(&update["project"], "id")?.into(),
        },
        parent_id: None,
        kind: "project_update".into(),
        role: Some("project updater".into()),
        actor: actor
            .map(str::to_owned)
            .or_else(|| update["user"]["name"].as_str().map(str::to_owned)),
        session: None,
        recipient: None,
        body: body.into(),
        summary: Some(body.split("\n\n").next().unwrap_or(body).trim().to_owned()),
        source_links: vec![],
        health: Some(text(update, "health")?.into()),
        reason: reason.map(str::to_owned),
        created_at: text(update, "createdAt")?.into(),
        updated_at: text(update, "updatedAt")?.into(),
        resolved_at: None,
        resolving_comment_id: None,
        verdict: None,
        findings: None,
        round: None,
        revision: None,
        formal_review: false,
    })
}

/// Read all native activity for one target within the Store page budget; no workflow state is written.
/// `known_meta` reuses an already-read `Meta` (from the same request's `Store::graph`/`work`
/// call) instead of a redundant `Store::meta` point read; pass `None` for a standalone read
/// with no such Meta on hand, which falls back to that same point read for current correctness.
pub async fn read_activity(
    store: &Store,
    target_type: &str,
    target_id: &str,
    known_meta: Option<&Meta>,
) -> Result<Vec<ActivityRecord>> {
    let key = match target_type {
        "issue" => "issue",
        "project" => "project",
        "project_update" => "projectUpdate",
        _ => return Err(Fault::new("INVALID_INPUT", "Unknown activity target")),
    };
    let current = if target_type != "issue" {
        None
    } else if let Some(m) = known_meta {
        m.review.clone()
    } else {
        store.meta(target_id).await?.and_then(|m| m.review)
    };
    let comments = store
        .pages(
            "QComments",
            "comments",
            json!({"filter":{key:{"id":{"eq":target_id}}},"includeArchived":false}),
        )
        .await?;
    comments
        .iter()
        .map(|comment| record(comment, current.as_ref()))
        .collect()
}
