//! Document operations, moved here unchanged from the parent Gateway so the documents
//! provider can extend this child module directly. Reuses the parent's private helpers
//! (`resolve`, `project`, `store`) through ordinary Rust module-tree visibility.
use super::Gateway;
use crate::{
    model::{Fault, Result, now, require, text},
    records::markdown_equivalent,
    sections::replace_section,
};
use serde_json::{Value, json};

impl Gateway {
    /// Save one native document, using the caller's UUID to recover uncertain creation without
    /// duplicates. Reference arguments resolve to native identities first; the replay check
    /// compares the resolved parent, so permalink retries behave exactly like UUID retries.
    /// Editing an existing Document is guarded, see `edit_document`.
    pub(super) async fn document(&self, a: &Value) -> Result<Value> {
        let editing = a["id"].is_string();
        let id = if editing {
            self.resolve("document", text(a, "id")?).await?
        } else {
            require(
                a.get("section").is_none() && a.get("expected_updated_at").is_none(),
                "INVALID_INPUT",
                "A new Document has no section or prior state to guard",
            )?;
            text(a, "request_id")?.to_owned()
        };
        if editing {
            return self.edit_document(&id, a).await;
        }
        require(
            a["project_id"].is_string() != a["issue_id"].is_string(),
            "INVALID_INPUT",
            "A new Document needs exactly one Project or Issue parent",
        )?;
        text(a, "title")?;
        require(
            a.get("content").is_some(),
            "INVALID_INPUT",
            "New Document requires content",
        )?;
        let parent = match a["project_id"].as_str() {
            Some(reference) => ("projectId", self.resolve("project", reference).await?),
            None => (
                "issueId",
                self.resolve("issue", text(a, "issue_id")?).await?,
            ),
        };
        if parent.0 == "projectId" {
            self.project(&parent.1).await?;
        } else {
            self.store.work(&parent.1).await?;
        }
        if let Some(d) = self.store.optional("QDocument", "document", &id).await? {
            let (native_key, other_key) = if parent.0 == "projectId" {
                ("project", "issue")
            } else {
                ("issue", "project")
            };
            require(
                d["title"] == a["title"]
                    && markdown_equivalent(
                        a["content"].as_str().unwrap(),
                        d["content"].as_str().unwrap_or(""),
                    )
                    && d[native_key]["id"] == json!(parent.1)
                    && d[other_key]["id"].is_null(),
                "REQUEST_CONFLICT",
                "Document request_id already names different content",
            )?;
            return Ok(d);
        }
        let mut input = json!({"id":id,"title":a["title"],"content":a["content"]});
        input[parent.0] = json!(parent.1);
        Ok(self
            .store
            .linear
            .call("MCreateDocument", json!({"input":input}))
            .await?["documentCreate"]["document"]
            .clone())
    }

    /// Guard an existing Document's edit by comparing the requested desired state against a
    /// fresh native read before writing. A section replaces only that section's body; whole
    /// `content` without `section` replaces the entire body; omitting both leaves content
    /// unchanged. `hidden` maps to native `hiddenAt` (now/null); `project_id`/`issue_id` rebind
    /// to exactly one new parent, explicitly clearing the other.
    ///
    /// If the native state already matches every requested change, this replays with
    /// `replayed:true` and no write, even on the exact same request after a lost reply or a
    /// cold restart. Otherwise `expected_updated_at` is required (`PRECONDITION_REQUIRED` names
    /// the get_context route when it is missing) and must equal the current native `updatedAt`
    /// (`PENDING_CONFLICT` otherwise); the write happens once and its result is confirmed before
    /// being returned, so a failed confirmation is marked `uncertain` rather than silently
    /// accepted. There is no native compare-and-swap: this precondition only catches drift this
    /// same reader already observed, and one responsible writer remains the safe pattern for a
    /// document also reachable from the native UI or another process.
    async fn edit_document(&self, id: &str, a: &Value) -> Result<Value> {
        require(
            a.get("section").is_none() || a.get("content").is_some(),
            "INVALID_INPUT",
            "section requires content: the desired new body for that section",
        )?;
        require(
            !(a["project_id"].is_string() && a["issue_id"].is_string()),
            "INVALID_INPUT",
            "Choose at most one new Project or Issue parent",
        )?;
        let hidden_requested = a.get("hidden").is_some_and(Value::is_boolean);
        let parent_requested = a["project_id"].is_string() || a["issue_id"].is_string();
        require(
            a.get("title").is_some()
                || a.get("content").is_some()
                || hidden_requested
                || parent_requested,
            "INVALID_INPUT",
            "No document fields to edit",
        )?;

        let current = self
            .store
            .linear
            .object("QDocument", "document", id)
            .await?;
        let current_content = current["content"].as_str().unwrap_or("");

        let desired_title = a["title"]
            .as_str()
            .unwrap_or_else(|| current["title"].as_str().unwrap_or(""));
        let desired_content = if let Some(heading) = a["section"].as_str() {
            let body = a["content"].as_str().unwrap_or("");
            replace_section(current_content, heading, body)?
        } else if let Some(whole) = a["content"].as_str() {
            whole.to_owned()
        } else {
            current_content.to_owned()
        };
        let desired_hidden = a.get("hidden").and_then(Value::as_bool);

        // (own native key, other native key, own input key, other input key, resolved target id)
        let new_parent = if let Some(reference) = a["project_id"].as_str() {
            Some((
                "project",
                "issue",
                "projectId",
                "issueId",
                self.resolve("project", reference).await?,
            ))
        } else if let Some(reference) = a["issue_id"].as_str() {
            Some((
                "issue",
                "project",
                "issueId",
                "projectId",
                self.resolve("issue", reference).await?,
            ))
        } else {
            None
        };
        if let Some((kind, _, _, _, target)) = &new_parent {
            if *kind == "project" {
                self.project(target).await?;
            } else {
                self.store.work(target).await?;
            }
        }

        let title_matches = current["title"].as_str() == Some(desired_title);
        let content_matches = markdown_equivalent(&desired_content, current_content);
        let hidden_matches =
            desired_hidden.is_none_or(|hide| !current["hiddenAt"].is_null() == hide);
        let parent_matches = new_parent
            .as_ref()
            .is_none_or(|(own, other, _, _, target)| {
                current[*own]["id"].as_str() == Some(target.as_str())
                    && current[*other]["id"].is_null()
            });

        if title_matches && content_matches && hidden_matches && parent_matches {
            let mut replayed = current;
            replayed["replayed"] = json!(true);
            return Ok(replayed);
        }

        let expected = a["expected_updated_at"].as_str().ok_or_else(|| {
            Fault::new(
                "PRECONDITION_REQUIRED",
                "expected_updated_at is required for this change; call get_context(type=document) \
                 to read the current Document and retry with its updatedAt",
            )
        })?;
        require(
            current["updatedAt"] == json!(expected),
            "PENDING_CONFLICT",
            "Document changed since it was read; preserve the concurrent edit",
        )?;

        let mut input = json!({});
        if !title_matches {
            input["title"] = json!(desired_title);
        }
        if !content_matches {
            input["content"] = json!(desired_content);
        }
        if let Some(hide) = desired_hidden
            && !hidden_matches
        {
            input["hiddenAt"] = if hide { json!(now()) } else { Value::Null };
        }
        if let Some((_, _, set_key, clear_key, target)) = &new_parent
            && !parent_matches
        {
            input[*set_key] = json!(target);
            input[*clear_key] = Value::Null;
        }

        let native = self
            .store
            .linear
            .call("MUpdateDocument", json!({"id":id,"input":input}))
            .await?["documentUpdate"]["document"]
            .clone();

        require(
            native["id"] == json!(id)
                && native["title"].as_str() == Some(desired_title)
                && markdown_equivalent(&desired_content, native["content"].as_str().unwrap_or(""))
                && desired_hidden.is_none_or(|hide| !native["hiddenAt"].is_null() == hide)
                && new_parent
                    .as_ref()
                    .is_none_or(|(own, other, _, _, target)| {
                        native[*own]["id"].as_str() == Some(target.as_str())
                            && native[*other]["id"].is_null()
                    }),
            "NATIVE_STATE_MISMATCH",
            "Linear did not confirm the requested Document change",
        )
        .map_err(Fault::uncertain)?;

        let mut result = native;
        result["replayed"] = json!(false);
        Ok(result)
    }
}
