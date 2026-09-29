//! Document operations, moved here unchanged from the parent Gateway so the documents
//! provider can extend this child module directly. Reuses the parent's private helpers
//! (`resolve`, `project`, `store`) through ordinary Rust module-tree visibility.
use super::Gateway;
use crate::{
    model::{Result, require, text},
    records::markdown_equivalent,
};
use serde_json::{Value, json};

impl Gateway {
    /// Save one native document, using the caller's UUID to recover uncertain creation without
    /// duplicates. Reference arguments resolve to native identities first; the replay check
    /// compares the resolved parent, so permalink retries behave exactly like UUID retries.
    pub(super) async fn document(&self, a: &Value) -> Result<Value> {
        let editing = a["id"].is_string();
        let id = if editing {
            self.resolve("document", text(a, "id")?).await?
        } else {
            text(a, "request_id")?.to_owned()
        };
        if editing {
            require(
                a.get("project_id").is_none() && a.get("issue_id").is_none(),
                "INVALID_INPUT",
                "Document ownership cannot be changed by edit",
            )?;
            self.store
                .linear
                .object("QDocument", "document", &id)
                .await?;
            let mut input = json!({});
            for k in ["title", "content"] {
                if let Some(v) = a.get(k) {
                    input[k] = v.clone();
                }
            }
            require(
                !input.as_object().unwrap().is_empty(),
                "INVALID_INPUT",
                "No document fields to edit",
            )?;
            return Ok(self
                .store
                .linear
                .call("MUpdateDocument", json!({"id":id,"input":input}))
                .await?["documentUpdate"]["document"]
                .clone());
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
}
