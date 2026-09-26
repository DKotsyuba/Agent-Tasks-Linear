//! Read-only agent views assembled from native work, persisted reports and activity.

use crate::{
    activity::ActivityRecord,
    model::{Fault, Kind, Result, Work, require},
    reports::ModuleReport,
    rules,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Resolve a native Linear Issue URL to its human identifier without fetching the URL.
/// Only HTTPS links on linear.app with an issue path are accepted; a malformed or unrelated
/// reference returns INVALID_LINK. The caller still resolves the identifier through Linear.
pub fn issue_identifier(reference: &str) -> Result<String> {
    let url = reqwest::Url::parse(reference)
        .map_err(|_| Fault::new("INVALID_LINK", "Expected a native Linear Issue URL"))?;
    let parts: Vec<_> = url
        .path_segments()
        .map(|segments| segments.collect())
        .unwrap_or_default();
    let identifier = parts
        .windows(2)
        .find_map(|pair| (pair[0] == "issue").then_some(pair[1]))
        .unwrap_or("");
    require(
        url.scheme() == "https"
            && url.host_str() == Some("linear.app")
            && url.query().is_none()
            && url.fragment().is_none()
            && identifier.contains('-')
            && identifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "INVALID_LINK",
        "Expected a native Linear Issue URL",
    )?;
    Ok(identifier.to_owned())
}

/// Render the assignment and evidence for one managed Issue without loading external data.
/// The graph is the complete native Project hierarchy; `activity` holds bounded native comment
/// reads keyed by Issue UUID, and `documents` contains metadata links only. An incomplete native
/// membership is exposed in `discrepancies`, with no derived Module count presented as exact.
pub fn agent_context(
    work: &Work,
    graph: &[Work],
    view: &str,
    activity: &BTreeMap<String, Vec<ActivityRecord>>,
    documents: &[Value],
    report: Option<&ModuleReport>,
) -> Result<Value> {
    require(
        matches!(view, "lead" | "reviewer"),
        "INVALID_INPUT",
        "view must be lead or reviewer",
    )?;
    let meta = work.managed()?;
    let parent = rules::parent(work).and_then(|id| rules::find(graph, id));
    let module = if meta.kind == Kind::Module {
        Some(work)
    } else if parent.is_some_and(|item| item.meta.as_ref().is_some_and(|m| m.kind == Kind::Module))
    {
        parent
    } else {
        None
    };
    let epic = if meta.kind == Kind::Epic {
        Some(work)
    } else {
        module
            .and_then(|item| rules::parent(item))
            .or_else(|| {
                parent
                    .filter(|item| item.meta.as_ref().is_some_and(|m| m.kind == Kind::Epic))
                    .map(Work::id)
            })
            .and_then(|id| rules::find(graph, id))
    };
    let mut children = rules::children(graph, work.id());
    children.sort_by(|a, b| crate::gateway::priority_cmp(&a.native, &b.native));
    let tasks: Vec<_> = children
        .iter()
        .map(|child| {
            json!({"id":child.id(),"identifier":child.native["identifier"],"title":child.native["title"],
                "url":child.native["url"],"status":child.native["state"]["name"],"priority":child.native["priority"],
                "result":child.fields["result"],"reported_checks":child.fields["check_result"],
                "git_reports":child.meta.as_ref().map(|m| m.current_git_reports().collect::<Vec<_>>())})
        })
        .collect();
    let ids = std::iter::once(work.id()).chain(children.iter().map(|child| child.id()));
    let open_questions: Vec<_> = ids
        .filter_map(|id| activity.get(id))
        .flat_map(|records| records.iter())
        .filter(|record| record.kind == "question" && record.resolved_at.is_none())
        .map(|record| json!({"id":record.id,"url":record.url,"target":record.target,"recipient":record.recipient,"body":record.body}))
        .collect();
    let latest_review = activity.get(work.id()).and_then(|records| {
        records
            .iter()
            .filter(|record| record.formal_review)
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
    });
    let discrepancies = rules::discrepancies(work, graph);
    let checkout = module.map(|item| json!({"repository_path":item.fields["repository_path"],
        "branch":item.fields["branch"],"worktree":item.fields["worktree"],"lead":item.fields["lead"]}));
    let links: Vec<_> = documents
        .iter()
        .map(|doc| json!({"id":doc["id"],"title":doc["title"],"url":doc["url"]}))
        .collect();
    Ok(json!({
        "view":view,"id":work.id(),"url":work.native["url"],"identifier":work.native["identifier"],
        "title":work.native["title"],"kind":meta.kind,"status":work.native["state"]["name"],
        "goal":work.fields["expected_result"],"description":work.fields["description"],
        "acceptance_criteria":work.fields["acceptance_criteria"],
        "required_contract":module.map(|item| &item.fields["required_contract"]),
        "provided_contract":module.map(|item| &item.fields["provided_contract"]),
        "epic":epic.map(|item| json!({"id":item.id(),"url":item.native["url"],"business_requirements":item.fields["business_requirements"],"acceptance_criteria":item.fields["acceptance_criteria"]})),
        "checkout":checkout,"tasks":tasks,"module_report":report,"current_git_reports":meta.current_git_reports().collect::<Vec<_>>(),
        "latest_review":latest_review,"open_questions":open_questions,"documents":links,"discrepancies":discrepancies,
        "next_work":if view == "lead" {json!(tasks.iter().filter(|task| !matches!(task["status"].as_str(),Some("Done"|"Canceled"|"Duplicate"))).collect::<Vec<_>>())} else {Value::Null},
        "review_evidence":if view == "reviewer" {json!({"module_report":report,"latest_review":latest_review})} else {Value::Null}
    }))
}
