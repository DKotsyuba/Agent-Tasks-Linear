//! Workflow types, readable issue fields and safe tool outcomes.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A safe error; `uncertain` means a Linear write might already have succeeded.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Fault {
    /// Stable error identifier.
    pub code: String,
    /// Explanation without credentials or raw response bodies.
    pub message: String,
    /// Whether callers must inspect and retry the same request identifier.
    pub uncertain: bool,
}
impl Fault {
    /// Build a known rejection from a code and safe explanation.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            uncertain: false,
        }
    }
    /// Mark an ambiguous remote write outcome.
    pub fn uncertain(mut self) -> Self {
        self.uncertain = true;
        self
    }
}
/// Fallible application result preserving the external-write uncertainty flag.
pub type Result<T> = std::result::Result<T, Fault>;
/// Enforce a condition without making a remote write.
pub fn require(condition: bool, code: &str, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Fault::new(code, message))
    }
}
/// Read a required nonblank string from a validated request.
pub fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Fault::new("INVALID_INPUT", format!("{k} must be nonempty")))
}
/// Current UTC observation time; native Linear history remains authoritative for status dates.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Work kind represented by a native issue and a matching uppercase label.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Business outcome containing a frozen collection of modules.
    Epic,
    /// Code delivery reviewed as one PR.
    Module,
    /// Local deliverable without a separate review round.
    Task,
    /// Standalone work or a cross-module integration check.
    Atomic,
}
impl Kind {
    /// Return the native type label to reuse or create.
    pub fn label(self) -> &'static str {
        match self {
            Self::Epic => "EPIC",
            Self::Module => "MODULE",
            Self::Task => "TASK",
            Self::Atomic => "ATOMIC",
        }
    }
    /// Prefix a nonblank title with this kind's marker, case-insensitively stripping repeated leading recognized markers while preserving unrelated title text; returns `INVALID_INPUT` if no title remains.
    pub fn title(self, value: &str) -> Result<String> {
        let mut title = value.trim();
        while let Some(rest) = title.strip_prefix('[') {
            let Some((prefix, tail)) = rest.split_once(']') else {
                break;
            };
            if !matches!(
                prefix.trim().to_ascii_uppercase().as_str(),
                "EPIC" | "MODULE" | "TASK" | "ATOMIC"
            ) {
                break;
            }
            title = tail.trim_start();
        }
        require(
            !title.trim().is_empty(),
            "INVALID_INPUT",
            "title must contain text after its kind prefix",
        )?;
        Ok(format!("[{}] {}", self.label(), title.trim()))
    }
}
/// Standard workflow names. Configuration must resolve existing Linear states to these names.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    /// Draft work.
    Backlog,
    /// Planned work, possibly incomplete.
    Todo,
    /// Active implementation.
    #[serde(rename = "In Progress")]
    InProgress,
    /// Whole-work review (never used by tasks).
    #[serde(rename = "In Review")]
    InReview,
    /// Accepted result, or locally completed task.
    Done,
    /// Retired with a reason.
    Canceled,
    /// Retired in favor of an identified original.
    Duplicate,
}
impl Status {
    /// Native display name used for lookup and diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
            Self::Canceled => "Canceled",
            Self::Duplicate => "Duplicate",
        }
    }
    /// Whether this work no longer contributes unfinished scope.
    pub fn terminal(self) -> bool {
        matches!(self, Self::Done | Self::Canceled | Self::Duplicate)
    }
}
/// Latest review decision; full reports are preserved in native comments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    /// Native comment UUID, also the caller's stable review request ID.
    pub id: String,
    /// Work round reviewed.
    pub round: u64,
    /// Content revision submitted to review.
    pub revision: u64,
    /// True for acceptance, false for changes requested.
    pub accepted: bool,
}
/// Minimal machine data stored on one native attachment, never in issue titles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    /// Record format discriminator.
    pub schema: u32,
    /// Native issue's semantic kind.
    pub kind: Kind,
    /// Structured user fields rendered in readable sections; technical references are separate from titles.
    pub fields: Value,
    /// Expected native project; mismatches are reported, not reverted.
    pub project_id: String,
    /// Expected native parent; root issues have no issue parent.
    pub parent_id: Option<String>,
    /// Known direct child UUIDs; retained to detect native detachments or project moves.
    #[serde(default)]
    pub children: Vec<String>,
    /// Last explicitly accepted native status.
    pub status: Status,
    /// Implementation round; incremented on return to work.
    pub round: u64,
    /// Human-content revision, incremented on edits affecting review.
    pub revision: u64,
    /// Module IDs fixed at the first epic start; Some(empty) is still frozen.
    pub frozen_modules: Option<Vec<String>>,
    /// Snapshot of merged modules used by an integration run.
    #[serde(default)]
    pub integration: BTreeMap<String, String>,
    /// Most recent review; previous reports remain in Linear comments.
    pub review: Option<Review>,
    /// Native completion timestamp expected after closure.
    pub completed_at: Option<String>,
    /// Exact description last submitted or explicitly adopted through an edit.
    pub description: String,
    /// Source request used to create the issue, for safe create replay.
    pub creation: Value,
    /// Last mutation request and its payload, for replay/conflict detection.
    pub last_request: Option<Value>,
    /// A prepared write survives crashes and can only be resumed by its original request.
    pub pending: Option<Box<Pending>>,
}
/// Two-phase issue update stored before changing native fields; no background recovery runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    /// Complete validated tool request including its operation name.
    pub request: Value,
    /// Intended metadata once the native update is confirmed.
    pub next: Meta,
    /// Native issue update input, replayed only with the same request ID.
    pub input: Value,
    /// Native fields observed before preparing the write, used to reject conflicting manual edits.
    #[serde(default)]
    pub before: Value,
}
/// Fresh native issue plus its attachment data and readable structured sections.
#[derive(Debug, Clone)]
pub struct Work {
    /// Unmodified native API result exposed as useful context.
    pub native: Value,
    /// Machine data; absent for ordinary unmanaged issues.
    pub meta: Option<Meta>,
    /// Named human fields parsed from the issue description.
    pub fields: Value,
}
impl Work {
    /// Native UUID used for links and parent comparisons.
    pub fn id(&self) -> &str {
        self.native["id"].as_str().unwrap_or("")
    }
    /// Require MCP metadata rather than adopting another issue implicitly.
    pub fn managed(&self) -> Result<&Meta> {
        self.meta
            .as_ref()
            .ok_or_else(|| Fault::new("UNMANAGED_ITEM", "This issue was not created by this MCP"))
    }
    /// Read a current native status, rejecting unmapped custom statuses.
    pub fn status(&self) -> Result<Status> {
        serde_json::from_value(self.native["state"]["name"].clone())
            .map_err(|_| Fault::new("UNKNOWN_STATUS", "Use a configured standard workflow state"))
    }
}
/// The only public tool envelope; failures are explicit and contain no success data.
#[derive(Debug, Serialize)]
pub struct Outcome {
    /// `ok`, `blocked`, `unavailable`, or `outcome_unknown`.
    pub status: String,
    /// Result object or actionable error explanation.
    pub data: Value,
}
impl Outcome {
    /// Return confirmed data, including check-only validation results.
    pub fn ok(data: Value) -> Self {
        Self {
            status: "ok".into(),
            data,
        }
    }
    /// Preserve the distinction between rejection and uncertain external outcome.
    pub fn failure(f: Fault) -> Self {
        Self {
            status: if f.uncertain {
                "outcome_unknown"
            } else if matches!(
                f.code.as_str(),
                "LINEAR_TOKEN_MISSING" | "LINEAR_UNAVAILABLE" | "RATE_LIMITED"
            ) {
                "unavailable"
            } else {
                "blocked"
            }
            .into(),
            data: json!({"code":f.code,"message":f.message,"retry":if f.uncertain {"Inspect context, then retry with the same request_id and arguments."} else {"Correct the reported condition."}}),
        }
    }
}
