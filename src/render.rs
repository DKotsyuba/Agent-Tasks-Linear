//! Pure, embedded MiniJinja presentation for every agent-facing MCP outcome.

use crate::model::Outcome;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Build the process-wide plain-text environment from trusted repository assets.
/// Invalid templates disable rendering but never erase a completed operation.
fn environment() -> Option<&'static Environment<'static>> {
    static ENVIRONMENT: OnceLock<Option<Environment>> = OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| {
            let mut env = Environment::new();
            env.set_trim_blocks(true);
            env.set_lstrip_blocks(true);
            env.set_keep_trailing_newline(true);
            env.set_undefined_behavior(UndefinedBehavior::Strict);
            env.set_auto_escape_callback(|_| AutoEscape::None);
            for (name, source) in [
                ("ack", include_str!("../assets/mcp/ack.txt.j2")),
                ("context", include_str!("../assets/mcp/context.txt.j2")),
                ("overview", include_str!("../assets/mcp/overview.txt.j2")),
                ("list", include_str!("../assets/mcp/list.txt.j2")),
                ("comment", include_str!("../assets/mcp/comment.txt.j2")),
                ("error", include_str!("../assets/mcp/error.txt.j2")),
            ] {
                env.add_template(name, source).ok()?;
            }
            Some(env)
        })
        .as_ref()
}

/// Map the complete public tool catalog to repo-owned success templates.
fn template_for(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "create_project"
        | "edit_project"
        | "create_epic"
        | "edit_epic"
        | "create_module"
        | "edit_module"
        | "create_task"
        | "edit_task"
        | "create_atomic"
        | "edit_atomic"
        | "save_document"
        | "move_status"
        | "record_review"
        | "record_commits"
        | "add_comment"
        | "resolve_comment"
        | "save_project_update" => "ack",
        "get_context" => "context",
        "get_overview" => "overview",
        "list_items" | "search" => "list",
        "get_comment" => "comment",
        _ => return None,
    })
}

/// Render a validated request and its existing outcome without changing either.
/// A presentation defect preserves confirmed success or failure and the request identity;
/// callers must inspect a confirmed mutation rather than submit a new one.
pub fn render_outcome(tool: &str, request: &Value, outcome: &Outcome) -> String {
    let success = matches!(outcome.status.as_str(), "ok" | "committed" | "noop");
    if success && !valid_success_shape(tool, &outcome.data) {
        return presentation_fallback(tool, request, outcome);
    }
    let (template, context) = if success {
        let page = match tool {
            "get_context" => context_page(&outcome.data),
            "get_overview" => overview_page(&outcome.data),
            "list_items" | "search" => list_page(tool, request, &outcome.data),
            "get_comment" => comment_page(&outcome.data),
            _ => ack_page(tool, request, &outcome.data),
        };
        (template_for(tool).unwrap_or("missing"), page)
    } else {
        (
            "error",
            json!({
                "status": outcome.status,
                "code": outcome.data["code"].as_str().unwrap_or("TOOL_FAILED"),
                "message": if outcome.data["code"] == "INVALID_COMMIT_MESSAGE" {
                    format!("{}\nExpected commit message format:\nfeat(scope): summary\n\nResult:\nWhat changed.\n\nChecks:\nWhat passed.", outcome.data["message"].as_str().unwrap_or("Invalid commit message"))
                } else {
                    outcome.data["message"].as_str().unwrap_or("Request failed").to_owned()
                },
                "retry": outcome.data["retry"].as_str().unwrap_or(""),
                "request_id": request["request_id"].as_str().unwrap_or("")
            }),
        )
    };
    render_template(template, context)
        .unwrap_or_else(|_| presentation_fallback(tool, request, outcome))
}

/// Reject missing essential read fields so a malformed success cannot look like an empty page.
fn valid_success_shape(tool: &str, data: &Value) -> bool {
    match tool {
        "get_context" => {
            data["issue"]["id"].is_string()
                || data["project"]["id"].is_string()
                || data["project_update"]["id"].is_string()
                || data["id"].is_string()
        }
        "get_overview" => data["project_id"].is_string() && data["cursor"].is_string(),
        "list_items" | "search" => data["nodes"].is_array() && data["pageInfo"].is_object(),
        "get_comment" => {
            data["comment"]["id"].is_string()
                && data["activity"].is_object()
                && data["replies"]["nodes"].is_array()
        }
        _ => data.is_object(),
    }
}

/// Render one embedded template, reporting registration and projection errors to the fallback.
fn render_template(name: &str, context: Value) -> Result<String, String> {
    environment()
        .ok_or("template registration failed")?
        .get_template(name)
        .map_err(|error| error.to_string())?
        .render(context)
        .map_err(|error| error.to_string())
}

/// Keep confirmed outcome and identity actionable when a presentation template fails.
fn presentation_fallback(tool: &str, request: &Value, outcome: &Outcome) -> String {
    let id = outcome.data["id"]
        .as_str()
        .or_else(|| outcome.data["issue"]["id"].as_str())
        .or_else(|| outcome.data["project"]["id"].as_str())
        .or_else(|| outcome.data["project_update"]["id"].as_str())
        .or_else(|| outcome.data["comment"]["id"].as_str())
        .or_else(|| outcome.data["review"]["id"].as_str())
        .or_else(|| request["request_id"].as_str())
        .unwrap_or("");
    if matches!(outcome.status.as_str(), "ok" | "committed" | "noop") {
        format!(
            "{tool}: {}. id/request_id: {id}. Presentation failed; inspect context before retrying a mutation.\n",
            outcome.status
        )
    } else {
        format!(
            "{}: {}. request_id: {id}. Presentation failed; inspect context before retrying.\n",
            outcome.status,
            outcome.data["code"].as_str().unwrap_or("TOOL_FAILED")
        )
    }
}

/// Read a nonempty scalar while retaining meaningful false and zero values.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Add one labelled scalar without shortening user-provided content.
fn field(lines: &mut Vec<String>, label: &str, value: &Value) {
    if let Some(value) = scalar(value) {
        lines.push(format!("{label}: {value}"));
    }
}

/// Add a titled Markdown section only when its body exists.
fn section(lines: &mut Vec<String>, label: &str, value: &Value) {
    if let Some(value) = scalar(value) {
        lines.push(format!("\n## {label}\n{value}"));
    }
}

/// Identify one native entity using a readable title plus actionable UUID and URL.
fn identity(lines: &mut Vec<String>, item: &Value) {
    if item["title"].is_string() {
        field(lines, "Title", &item["title"]);
    } else {
        field(lines, "Title", &item["name"]);
    }
    field(lines, "Identifier", &item["identifier"]);
    field(lines, "ID", &item["id"]);
    field(lines, "URL", &item["url"]);
}

/// Build a short confirmation without echoing submitted descriptions or report bodies.
fn ack_page(tool: &str, request: &Value, data: &Value) -> Value {
    let mut lines = vec![];
    if tool == "move_status" && data.get("allowed").is_some() {
        field(&mut lines, "Allowed", &data["allowed"]);
        field(&mut lines, "Target", &data["status"]);
        for condition in data["conditions"].as_array().into_iter().flatten() {
            field(&mut lines, "Condition", condition);
        }
    } else {
        let native = ["issue", "project", "project_update", "comment", "review"]
            .iter()
            .find_map(|key| data.get(*key))
            .unwrap_or(data);
        identity(&mut lines, native);
        field(&mut lines, "Status", &native["state"]["name"]);
        field(&mut lines, "Health", &native["health"]);
        if tool == "save_project_update" {
            field(&mut lines, "Updated at", &native["updatedAt"]);
        }
        if tool == "resolve_comment" {
            lines.push(format!("Resolved: {}", native["resolvedAt"].is_string()));
        }
        if tool == "record_review" {
            field(&mut lines, "Verdict", &request["verdict"]);
            if native["url"].is_null() {
                field(&mut lines, "URL", &data["url"]);
            }
        }
        if tool == "record_commits" {
            for report in data["git_reports"].as_array().into_iter().flatten() {
                let commit = if report["commit"].is_object() {
                    &report["commit"]
                } else {
                    report
                };
                lines.push(format!(
                    "Commit: {} {}",
                    scalar(&commit["sha"]).unwrap_or_default(),
                    scalar(&commit["subject"]).unwrap_or_default()
                ));
            }
            for comment in data["journal"].as_array().into_iter().flatten() {
                field(&mut lines, "Report URL", &comment["url"]);
            }
        }
        if tool == "create_project" {
            for document in data["documents"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "Document: {} — {} (ID {})",
                    scalar(&document["title"]).unwrap_or_default(),
                    scalar(&document["url"]).unwrap_or_default(),
                    scalar(&document["id"]).unwrap_or_default()
                ));
            }
        }
        field(&mut lines, "Replayed", &data["replayed"]);
        field(&mut lines, "Unchanged", &data["unchanged"]);
        if lines.is_empty() {
            field(&mut lines, "ID", &request["id"]);
        }
    }
    json!({"heading":format!("{tool}: confirmed"),"lines":lines})
}

/// Present native content once and add only distinct context needed for follow-up actions.
fn context_page(data: &Value) -> Value {
    let mut lines = vec![];
    let heading = if let Some(issue) = data.get("issue") {
        identity(&mut lines, issue);
        field(&mut lines, "Project ID", &issue["project"]["id"]);
        field(&mut lines, "Team ID", &issue["team"]["id"]);
        field(&mut lines, "Parent ID", &issue["parent"]["id"]);
        field(&mut lines, "Status", &issue["state"]["name"]);
        field(&mut lines, "Priority", &issue["priority"]);
        section(&mut lines, "Description", &issue["description"]);
        let agent = &data["agent_context"];
        let checkout = if agent["checkout"].is_object() {
            &agent["checkout"]
        } else {
            &data["parent_checkout"]
        };
        if checkout.is_object() {
            lines.push("\n## Checkout".into());
            for (label, key) in [
                ("Repository", "repository_path"),
                ("Repository URL", "repository_url"),
                ("Branch", "branch"),
                ("Worktree", "worktree"),
                ("Lead", "lead"),
            ] {
                field(&mut lines, label, &checkout[key]);
            }
        }
        if agent["epic"].is_object() {
            lines.push("\n## Parent Epic".into());
            field(&mut lines, "URL", &agent["epic"]["url"]);
            section(
                &mut lines,
                "Business requirements",
                &agent["epic"]["business_requirements"],
            );
            section(
                &mut lines,
                "Acceptance criteria",
                &agent["epic"]["acceptance_criteria"],
            );
        }
        if !data["fields"]["required_contract"].is_string() {
            section(&mut lines, "Required contract", &agent["required_contract"]);
        }
        if !data["fields"]["provided_contract"].is_string() {
            section(&mut lines, "Provided contract", &agent["provided_contract"]);
        }
        let report = &data["module_report"];
        if report.is_object() {
            lines.push(format!(
                "\n## Module progress\n{}/{} Tasks Done",
                report["tasks_done"], report["tasks_total"]
            ));
            if data["fields"]["result"].is_null() {
                section(&mut lines, "PR draft", &report["pr_draft"]);
            }
            for item in report["unfinished"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "Unfinished: {} — {}",
                    scalar(&item["url"]).unwrap_or_default(),
                    scalar(&item["status"]).unwrap_or_default()
                ));
            }
        }
        let tasks = if agent["tasks"].is_array() {
            &agent["tasks"]
        } else {
            &data["children"]
        };
        for task in tasks.as_array().into_iter().flatten() {
            let status = scalar(&task["status"])
                .or_else(|| scalar(&task["state"]["name"]))
                .unwrap_or_default();
            lines.push(format!(
                "\nChild: {} — {} ({status})",
                scalar(&task["identifier"]).unwrap_or_default(),
                scalar(&task["title"]).unwrap_or_default()
            ));
            field(&mut lines, "ID", &task["id"]);
            field(&mut lines, "URL", &task["url"]);
            if report.is_null() {
                section(&mut lines, "Result", &task["result"]);
                section(&mut lines, "Checks", &task["reported_checks"]);
            }
        }
        if report.is_null() {
            for git in data["git_reports"].as_array().into_iter().flatten() {
                let commit = if git["commit"].is_object() {
                    &git["commit"]
                } else {
                    git
                };
                lines.push(format!(
                    "\nCommit: {} {}",
                    scalar(&commit["sha"]).unwrap_or_default(),
                    scalar(&commit["subject"]).unwrap_or_default()
                ));
            }
        }
        if agent["latest_review"].is_object() {
            let review = &agent["latest_review"];
            lines.push("\n## Latest review".into());
            field(&mut lines, "URL", &review["url"]);
            field(&mut lines, "Verdict", &review["verdict"]);
            section(&mut lines, "Summary", &review["summary"]);
            section(&mut lines, "Findings", &review["findings"]);
        }
        for question in agent["open_questions"].as_array().into_iter().flatten() {
            lines.push("\n## Open question".into());
            field(&mut lines, "URL", &question["url"]);
            field(&mut lines, "Recipient", &question["recipient"]);
            section(&mut lines, "Body", &question["body"]);
        }
        for document in agent["documents"].as_array().into_iter().flatten() {
            lines.push(format!(
                "Document: {} — {} (ID {})",
                scalar(&document["title"]).unwrap_or_default(),
                scalar(&document["url"]).unwrap_or_default(),
                scalar(&document["id"]).unwrap_or_default()
            ));
        }
        for peer in data["priority_group"]["peers"]
            .as_array()
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "Priority peer: {} — {} ({})",
                scalar(&peer["identifier"]).unwrap_or_default(),
                scalar(&peer["url"]).unwrap_or_default(),
                scalar(&peer["status"]).unwrap_or_default()
            ));
        }
        if data["workflow"]["pending"].is_object() {
            let pending = &data["workflow"]["pending"]["request"];
            let call = json!({"tool":pending["tool"],"arguments":pending["arguments"]});
            lines.push(format!("\n## Pending mutation recovery\nRetry the same tool with these exact argument values after inspecting the native state:\n```json\n{}\n```", serde_json::to_string_pretty(&call).unwrap_or_default()));
        }
        for problem in data["discrepancies"].as_array().into_iter().flatten() {
            field(&mut lines, "Discrepancy", problem);
        }
        for transition in data["transitions"].as_array().into_iter().flatten() {
            let conditions = transition["conditions"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(scalar)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            lines.push(format!(
                "Transition {}: {}{}",
                scalar(&transition["status"]).unwrap_or_default(),
                if transition["allowed"] == true {
                    "allowed"
                } else {
                    "blocked"
                },
                if conditions.is_empty() {
                    String::new()
                } else {
                    format!(" — {conditions}")
                }
            ));
        }
        "Issue"
    } else if let Some(project) = data.get("project") {
        identity(&mut lines, project);
        for team in project["teams"]["nodes"].as_array().into_iter().flatten() {
            field(&mut lines, "Team ID", &team["id"]);
        }
        section(&mut lines, "Project content", &project["content"]);
        for document in data["documents"].as_array().into_iter().flatten() {
            lines.push(format!(
                "Document: {} — {} (ID {})",
                scalar(&document["title"]).unwrap_or_default(),
                scalar(&document["url"]).unwrap_or_default(),
                scalar(&document["id"]).unwrap_or_default()
            ));
        }
        "Project"
    } else if let Some(update) = data.get("project_update") {
        identity(&mut lines, update);
        field(&mut lines, "Health", &update["health"]);
        field(&mut lines, "Updated at", &update["updatedAt"]);
        field(&mut lines, "Author", &data["activity"]["actor"]);
        field(&mut lines, "Reason", &data["activity"]["reason"]);
        if data["activity"]["body"].is_string() {
            section(&mut lines, "Body", &data["activity"]["body"]);
        } else {
            section(&mut lines, "Body", &update["body"]);
        }
        "Project update"
    } else {
        identity(&mut lines, data);
        section(&mut lines, "Content", &data["content"]);
        "Document"
    };
    json!({"heading":heading,"lines":lines})
}

/// Present one full project overview or changed fields from a valid cursor.
fn overview_page(data: &Value) -> Value {
    let mut lines = vec![];
    field(&mut lines, "Project", &data["project_title"]);
    field(&mut lines, "Project ID", &data["project_id"]);
    field(&mut lines, "Project URL", &data["project_url"]);
    field(&mut lines, "Cursor", &data["cursor"]);
    if data["baseline_expired"] == true {
        lines.push("Previous cursor expired; this is a full overview.".into());
    }
    if let Some(changes) = data["changes"].as_array() {
        if changes.is_empty() {
            lines.push("No changes since the supplied cursor.".into());
        }
        for change in changes {
            let before = &change["before"];
            let after = &change["after"];
            let current = if after.is_null() { before } else { after };
            let label = scalar(&current["identifier"])
                .or_else(|| scalar(&current["url"]))
                .or_else(|| scalar(&change["key"]))
                .unwrap_or_default();
            lines.push(format!(
                "\nChange: {label}{}",
                if after.is_null() {
                    " (removed)"
                } else if before.is_null() {
                    " (added)"
                } else {
                    ""
                }
            ));
            field(&mut lines, "URL", &current["url"]);
            if after.is_null() {
                continue;
            }
            for (name, key) in [
                ("Status", "status"),
                ("Title", "title"),
                ("Name", "name"),
                ("Kind", "kind"),
                ("Parent ID", "parent_id"),
                ("Lead", "lead"),
                ("Priority", "priority"),
                ("Result", "result_preview"),
                ("Comment", "body_preview"),
                ("Verdict", "verdict"),
                ("Health", "health"),
                ("Reason", "reason"),
                ("Resolved", "resolved_at"),
            ] {
                if before[key] != after[key] {
                    if after[key].is_null() {
                        lines.push(format!("{name}: cleared"));
                    } else {
                        field(&mut lines, name, &after[key]);
                    }
                }
            }
            if before["result_hash"] != after["result_hash"]
                && before["result_preview"] == after["result_preview"]
            {
                lines.push(
                    "Result changed beyond its preview; read the Issue for full text.".into(),
                );
            }
            if before["body_hash"] != after["body_hash"]
                && before["body_preview"] == after["body_preview"]
            {
                lines.push(
                    "Comment changed beyond its preview; read the thread for full text.".into(),
                );
            }
            if before["fields_hash"] != after["fields_hash"] {
                lines.push("Issue fields changed; read the Issue for full text.".into());
            }
            if before["checks_hash"] != after["checks_hash"] {
                lines.push("Reported checks changed; read the Issue for full text.".into());
            }
            if before["source_commits"] != after["source_commits"]
                && (!before["source_commits"].is_null() || !after["source_commits"].is_null())
            {
                lines.push("Commit sources changed; read the Issue for full text.".into());
            }
            if before["review"] != after["review"]
                && (!before["review"].is_null() || !after["review"].is_null())
            {
                lines.push("Review changed; read the Issue for full text.".into());
            }
        }
    } else {
        for epic in data["active_epics"].as_array().into_iter().flatten() {
            lines.push(format!(
                "\nEpic: {} — {} ({})",
                scalar(&epic["identifier"]).unwrap_or_default(),
                scalar(&epic["title"]).unwrap_or_default(),
                scalar(&epic["status"]).unwrap_or_default()
            ));
            field(&mut lines, "URL", &epic["url"]);
            field(&mut lines, "ID", &epic["id"]);
            section(
                &mut lines,
                "Business requirements",
                &epic["business_requirements"],
            );
            section(&mut lines, "Expected result", &epic["expected_result"]);
            section(&mut lines, "Result", &epic["result"]);
            for module in epic["modules"].as_array().into_iter().flatten() {
                overview_module(&mut lines, module);
            }
        }
        for module in data["standalone_modules"].as_array().into_iter().flatten() {
            overview_module(&mut lines, module);
        }
        for atomic in data["atomics"].as_array().into_iter().flatten() {
            lines.push(format!(
                "\nAtomic: {} — {} ({})",
                scalar(&atomic["identifier"]).unwrap_or_default(),
                scalar(&atomic["title"]).unwrap_or_default(),
                scalar(&atomic["status"]).unwrap_or_default()
            ));
            field(&mut lines, "URL", &atomic["url"]);
            section(&mut lines, "Result", &atomic["result"]);
            section(&mut lines, "Reported checks", &atomic["reported_checks"]);
        }
        for question in data["open_questions"].as_array().into_iter().flatten() {
            lines.push("\nOpen question".into());
            field(&mut lines, "URL", &question["url"]);
            field(&mut lines, "Recipient", &question["recipient"]);
            section(&mut lines, "Body", &question["body"]);
        }
        for item in data["awaiting_review"].as_array().into_iter().flatten() {
            lines.push(format!(
                "Awaiting review: {} — {}",
                scalar(&item["identifier"]).unwrap_or_default(),
                scalar(&item["url"]).unwrap_or_default()
            ));
        }
        for item in data["excluded"].as_array().into_iter().flatten() {
            lines.push(format!(
                "Excluded: {} — {}",
                scalar(&item["identifier"]).unwrap_or_default(),
                scalar(&item["url"]).unwrap_or_default()
            ));
            field(&mut lines, "Reason", &item["reason"]);
        }
    }
    json!({"heading":"Project overview","lines":lines})
}

/// Summarize one Module's progress without repeating its derived report body.
fn overview_module(lines: &mut Vec<String>, module: &Value) {
    lines.push(format!(
        "\nModule: {} — {} ({})",
        scalar(&module["identifier"]).unwrap_or_default(),
        scalar(&module["title"]).unwrap_or_default(),
        scalar(&module["status"]).unwrap_or_default()
    ));
    field(lines, "URL", &module["url"]);
    field(lines, "ID", &module["id"]);
    field(lines, "Lead", &module["lead"]);
    section(lines, "Expected result", &module["expected_result"]);
    lines.push(format!(
        "Tasks: {}/{} Done",
        module["report"]["tasks_done"], module["report"]["tasks_total"]
    ));
    section(lines, "Result", &module["report"]["summary"]);
    section(
        lines,
        "Reported checks",
        &module["report"]["reported_checks"],
    );
    for item in module["report"]["unfinished"]
        .as_array()
        .into_iter()
        .flatten()
    {
        lines.push(format!(
            "Unfinished: {} — {}",
            scalar(&item["url"]).unwrap_or_default(),
            scalar(&item["status"]).unwrap_or_default()
        ));
    }
}

/// Present all native page items once and preserve the opaque cursor unchanged.
fn list_page(tool: &str, request: &Value, data: &Value) -> Value {
    let mut lines = vec![];
    let records = data["activity_records"].as_array();
    for (index, item) in data["nodes"].as_array().into_iter().flatten().enumerate() {
        let record = records.and_then(|records| records.get(index));
        lines.push("\nItem".into());
        identity(&mut lines, item);
        field(&mut lines, "Status", &item["state"]["name"]);
        field(&mut lines, "Priority", &item["priority"]);
        field(&mut lines, "Health", &item["health"]);
        if request["type"] == "project_update" {
            field(&mut lines, "Updated at", &item["updatedAt"]);
        }
        if let Some(record) = record {
            field(&mut lines, "Kind", &record["kind"]);
            field(&mut lines, "Actor", &record["actor"]);
            field(&mut lines, "Recipient", &record["recipient"]);
            field(&mut lines, "Reason", &record["reason"]);
            section(&mut lines, "Body", &record["body"]);
        }
    }
    if data["nodes"].as_array().is_some_and(Vec::is_empty) {
        lines.push("No items on this page.".into());
    }
    field(
        &mut lines,
        "Has next page",
        &data["pageInfo"]["hasNextPage"],
    );
    field(&mut lines, "Next cursor", &data["pageInfo"]["endCursor"]);
    json!({"heading":format!("{tool}: {}", scalar(&request["type"]).unwrap_or_else(|| "items".into())),"lines":lines})
}

/// Present the explicitly requested comment body and every returned reply in full.
fn comment_page(data: &Value) -> Value {
    let mut lines = vec![];
    let record = &data["activity"];
    identity(&mut lines, &data["comment"]);
    field(&mut lines, "Kind", &record["kind"]);
    field(&mut lines, "Actor", &record["actor"]);
    field(&mut lines, "Recipient", &record["recipient"]);
    field(&mut lines, "Verdict", &record["verdict"]);
    section(&mut lines, "Body", &record["body"]);
    section(&mut lines, "Findings", &record["findings"]);
    if data["root"]["id"] != data["comment"]["id"] && data["root"].is_object() {
        lines.push("\nRoot comment".into());
        identity(&mut lines, &data["root"]);
        section(&mut lines, "Body", &data["root"]["body"]);
    }
    for reply in data["replies"]["nodes"].as_array().into_iter().flatten() {
        lines.push("\nReply".into());
        identity(&mut lines, reply);
        section(&mut lines, "Body", &reply["body"]);
    }
    field(
        &mut lines,
        "Has next page",
        &data["replies"]["pageInfo"]["hasNextPage"],
    );
    field(
        &mut lines,
        "Next cursor",
        &data["replies"]["pageInfo"]["endCursor"],
    );
    json!({"heading":"Comment thread","lines":lines})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard the mapping against a catalog addition that would silently lose its details.
    #[test]
    fn every_catalog_tool_has_a_template_and_realistic_shape() {
        let catalog = crate::catalog::Catalog::new().unwrap();
        assert_eq!(catalog.tools.len(), 22);
        for tool in catalog.tools {
            let name = tool["name"].as_str().unwrap();
            assert!(template_for(name).is_some(), "{name}");
            let request = json!({"request_id":"req-1","type":"issue"});
            let native = json!({"id":"issue-1","identifier":"MYT-1","title":"A task","url":"https://linear.app/example/issue/MYT-1/a-task","state":{"name":"In Progress"}});
            let data = match name {
                "get_context" => {
                    json!({"issue":native,"fields":{},"transitions":[{"status":"Done","allowed":false,"conditions":["Checks required"]}]})
                }
                "get_overview" => {
                    json!({"project_id":"project-1","cursor":"opaque-1","standalone_modules":[{"id":"issue-1","identifier":"MYT-1","title":"A task","url":"https://linear.app/example/issue/MYT-1/a-task","status":"In Progress","report":{"tasks_done":0,"tasks_total":1}}]})
                }
                "list_items" | "search" => {
                    json!({"nodes":[native],"pageInfo":{"hasNextPage":true,"endCursor":"opaque-1"}})
                }
                "get_comment" => {
                    json!({"comment":{"id":"comment-1","url":"https://linear.app/comment-1"},"activity":{"kind":"note","body":"Exact comment body"},"replies":{"nodes":[],"pageInfo":{"hasNextPage":false}}})
                }
                "create_project" => {
                    json!({"project":{"id":"project-1","name":"A project","url":"https://linear.app/project-1"},"documents":[{"title":"Runbook","url":"https://linear.app/document-1"}]})
                }
                "edit_project" => {
                    json!({"id":"project-1","name":"A project","url":"https://linear.app/project-1"})
                }
                "save_document" => {
                    json!({"id":"document-1","title":"Plan","url":"https://linear.app/document-1","content":"Do not echo this submitted body"})
                }
                "record_review" => {
                    json!({"review":{"id":"review-1"},"url":"https://linear.app/review-1"})
                }
                "add_comment" | "resolve_comment" => {
                    json!({"comment":{"id":"comment-1","url":"https://linear.app/comment-1"},"replayed":false})
                }
                "save_project_update" => {
                    json!({"project_update":{"id":"update-1","url":"https://linear.app/update-1","health":"onTrack"},"replayed":false})
                }
                "record_commits" => {
                    json!({"issue":native,"git_reports":[{"sha":"abcdef123","subject":"feat: work"}]})
                }
                _ => json!({"issue":native,"replayed":false}),
            };
            let text = render_outcome(name, &request, &Outcome::ok(data));
            assert!(
                !text.is_empty() && !text.contains("Presentation failed"),
                "{name}: {text}"
            );
            assert!(!text.contains("\"status\":\"ok\""), "{name}");
            assert!(
                text.contains("ID:") || text.contains("Cursor:"),
                "{name}: {text}"
            );
        }
    }

    /// Read paths preserve human prose, full requested documents and exact recovery arguments.
    #[test]
    fn context_document_and_pending_request_keep_content() {
        let description =
            "## Known field\nRequired result\n\n## User note\nKeep this unknown section intact.";
        let args = json!({"request_id":"req-1","actor":"codex:lead","fields":{"result":"Line 1\n  line 2","flag":false,"count":0,"unset":null}});
        let issue = json!({"issue":{"id":"issue-1","identifier":"MYT-1","title":"A task","url":"https://linear.app/issue-1","project":{"id":"project-1"},"team":{"id":"team-1"},"parent":{"id":"module-1"},"state":{"name":"In Progress"},"description":description},"fields":{},"workflow":{"pending":{"request":{"tool":"edit_task","arguments":args}}},"transitions":[{"status":"Done","allowed":false,"conditions":["Checks required"]}]});
        let text = render_outcome(
            "get_context",
            &json!({"type":"issue","id":"issue-1"}),
            &Outcome::ok(issue),
        );
        assert!(text.contains(description));
        assert!(text.contains("Project ID: project-1"));
        assert!(text.contains("Team ID: team-1"));
        assert!(text.contains("Parent ID: module-1"));
        assert!(text.contains("Checks required"));
        assert!(text.contains("\"tool\": \"edit_task\""));
        assert!(text.contains("\"flag\": false"));
        assert!(text.contains("\"count\": 0"));
        assert!(text.contains("\"unset\": null"));
        assert!(text.contains("Line 1\\n  line 2"));
        assert!(!text.contains("\"before\""));
        let document = json!({"id":"doc-1","title":"Plan","url":"https://linear.app/doc-1","content":"First line\n".to_owned()+&"Long body. ".repeat(2000)});
        let body = document["content"].as_str().unwrap();
        let rendered = render_outcome(
            "get_context",
            &json!({"type":"document","id":"doc-1"}),
            &Outcome::ok(document.clone()),
        );
        assert!(rendered.contains(body));
        assert!(rendered.len() < serde_json::to_string(&Outcome::ok(document)).unwrap().len());
        let update = json!({"project_update":{"id":"update-1","url":"https://linear.app/update-1","health":"atRisk","updatedAt":"2026-09-26T20:00:00Z"},"activity":{"actor":"codex:lead","reason":"Blocked upstream","body":"The full update body."}});
        let text = render_outcome(
            "get_context",
            &json!({"type":"project_update","id":"update-1"}),
            &Outcome::ok(update),
        );
        assert!(text.contains("Updated at: 2026-09-26T20:00:00Z"));
        assert!(text.contains("Reason: Blocked upstream"));
        assert!(text.contains("The full update body."));
    }

    /// List and overview cursors stay exact while deltas show changed values instead of hashes.
    #[test]
    fn pages_and_deltas_are_actionable() {
        let cursor = "{\"version\":1,\"last\":\"a\\\"b\"}";
        let page = json!({"nodes":[{"id":"issue-1","identifier":"MYT-1","title":"First","url":"https://linear.app/issue-1","priority":0}],"pageInfo":{"hasNextPage":true,"endCursor":cursor}});
        let text = render_outcome("list_items", &json!({"type":"issue"}), &Outcome::ok(page));
        assert!(text.contains(cursor));
        assert!(text.contains("Has next page: true"));
        assert!(text.contains("Priority: 0"));
        let delta = json!({"project_id":"project-1","cursor":"next-1","changes":[{"key":"work:issue-1","before":{"status":"Todo","result_hash":"abc","result_preview":"Old"},"after":{"id":"issue-1","identifier":"MYT-1","url":"https://linear.app/issue-1","status":"Done","result_hash":"def","result_preview":"New"}}]});
        let text = render_outcome(
            "get_overview",
            &json!({"project_id":"project-1"}),
            &Outcome::ok(delta),
        );
        assert!(text.contains("Status: Done"));
        assert!(text.contains("Result: New"));
        assert!(text.contains("Cursor: next-1"));
        assert!(!text.contains("result_hash"));
    }

    /// Explicit activity reads keep every returned body while exposing the next reply page.
    #[test]
    fn comments_keep_full_thread_text() {
        let comment = json!({"comment":{"id":"reply-1","url":"https://linear.app/reply-1"},"activity":{"kind":"question","actor":"codex:lead","recipient":"owner","body":"Full question\nsecond line"},"root":{"id":"root-1","url":"https://linear.app/root-1","body":"Original question"},"replies":{"nodes":[{"id":"reply-2","url":"https://linear.app/reply-2","body":"Answer\nsecond line"}],"pageInfo":{"hasNextPage":true,"endCursor":"reply-cursor"}}});
        let text = render_outcome(
            "get_comment",
            &json!({"id":"reply-1"}),
            &Outcome::ok(comment),
        );
        for expected in [
            "Full question\nsecond line",
            "Original question",
            "Answer\nsecond line",
            "Next cursor: reply-cursor",
            "Recipient: owner",
        ] {
            assert!(text.contains(expected));
        }
        let page = json!({"nodes":[{"id":"root-1","url":"https://linear.app/root-1"}],"activity_records":[{"kind":"question","actor":"codex:lead","body":"Unclipped list body"}],"pageInfo":{"hasNextPage":false}});
        let text = render_outcome("list_items", &json!({"type":"comment"}), &Outcome::ok(page));
        assert!(text.contains("Unclipped list body"));
        assert!(text.contains("Has next page: false"));
    }

    /// Invalid commit reports get a concrete format example without altering failure status.
    #[test]
    fn commit_format_error_shows_recovery_example() {
        let failed = Outcome {
            status: "blocked".into(),
            data: json!({"code":"INVALID_COMMIT_MESSAGE","message":"Result and Checks must be nonempty","retry":"Correct the reported condition."}),
        };
        let text = render_outcome("record_commits", &json!({"request_id":"req-1"}), &failed);
        assert!(text.contains("Result:\nWhat changed."));
        assert!(text.contains("Checks:\nWhat passed."));
        assert!(text.contains("request_id: req-1"));
    }

    /// Check that failure states retain retry guidance and zero/false fields do not become success.
    #[test]
    fn failure_and_success_are_distinct() {
        let request = json!({"request_id":"request-1"});
        let failed = Outcome {
            status: "outcome_unknown".into(),
            data: json!({"code":"LINEAR_UNAVAILABLE","message":"Maybe committed","retry":"Inspect, then retry same ID"}),
        };
        let text = render_outcome("edit_task", &request, &failed);
        assert!(text.contains("outcome_unknown: LINEAR_UNAVAILABLE"));
        assert!(text.contains("request-1"));
        assert!(text.contains("Inspect, then retry same ID"));
        let success = Outcome::ok(json!({"id":"item-1","count":0,"replayed":false}));
        let text = render_outcome("create_task", &request, &success);
        assert!(text.contains("ID: item-1"));
        assert!(text.contains("Replayed: false"));
        assert!(!text.contains("count"));
    }

    /// Simulate a missing template after a confirmed mutation and retain its ID and outcome.
    #[test]
    fn presentation_failure_does_not_relabel_success() {
        assert!(render_template("missing", json!({})).is_err());
        let text = presentation_fallback(
            "create_task",
            &json!({"request_id":"request-1"}),
            &Outcome::ok(json!({"id":"item-1"})),
        );
        assert!(text.contains("ok"));
        assert!(text.contains("item-1"));
        assert!(text.contains("inspect context before retrying"));
        let malformed = render_outcome(
            "get_context",
            &json!({"id":"issue-1"}),
            &Outcome::ok(json!({})),
        );
        assert!(malformed.contains("get_context: ok"));
        assert!(malformed.contains("Presentation failed"));
    }
}
