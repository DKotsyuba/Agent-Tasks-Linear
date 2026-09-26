//! Pure, embedded MiniJinja presentation for agent-facing MCP outcomes.

use crate::model::Outcome;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Build the process-wide plain-text environment from trusted repository assets.
/// Invalid templates are programming defects; no filesystem or network access occurs.
fn environment() -> &'static Environment<'static> {
    static ENVIRONMENT: OnceLock<Environment> = OnceLock::new();
    ENVIRONMENT.get_or_init(|| {
        let mut env = Environment::new();
        env.set_trim_blocks(true);
        env.set_lstrip_blocks(true);
        env.set_keep_trailing_newline(true);
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_auto_escape_callback(|_| AutoEscape::None);
        env.add_template("ack", include_str!("../assets/mcp/ack.txt.j2"))
            .expect("valid ack template");
        env.add_template("error", include_str!("../assets/mcp/error.txt.j2"))
            .expect("valid error template");
        env
    })
}

/// Render a validated request and its existing outcome without changing either.
/// A presentation defect preserves confirmed success or failure and the request identity;
/// callers must inspect a confirmed mutation rather than submit a new one.
pub fn render_outcome(tool: &str, request: &Value, outcome: &Outcome) -> String {
    let success = matches!(outcome.status.as_str(), "ok" | "committed" | "noop");
    let context = if success {
        json!({
            "tool": tool,
            "status": outcome.status,
            "id": outcome.data["id"].as_str().or_else(|| request["request_id"].as_str()).unwrap_or("")
        })
    } else {
        json!({
            "status": outcome.status,
            "code": outcome.data["code"].as_str().unwrap_or("TOOL_FAILED"),
            "message": outcome.data["message"].as_str().unwrap_or("Request failed"),
            "retry": outcome.data["retry"].as_str().unwrap_or(""),
            "request_id": request["request_id"].as_str().unwrap_or("")
        })
    };
    let template = if success { "ack" } else { "error" };
    render_template(template, context)
        .unwrap_or_else(|_| presentation_fallback(tool, request, outcome))
}

/// Render one embedded template, returning an error if its name or projection is invalid.
fn render_template(name: &str, context: Value) -> Result<String, minijinja::Error> {
    environment().get_template(name)?.render(context)
}

/// Keep confirmed outcome and identity actionable when a presentation template fails.
fn presentation_fallback(tool: &str, request: &Value, outcome: &Outcome) -> String {
    let id = outcome.data["id"]
        .as_str()
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            render_outcome("create_task", &request, &success),
            "create_task: ok — item-1"
        );
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
    }
}
