//! Minimal native attachment state and lossless editing of readable Markdown sections.
use crate::{
    linear::Linear,
    model::{Fault, Meta, Result, Work, require},
};
use pulldown_cmark::{Event, Parser, Tag};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Issue fields exposed to agents and rendered as understandable sections.
pub const FIELDS: &[(&str, &str)] = &[
    ("description", "Описание"),
    ("business_requirements", "Бизнес-требования"),
    ("expected_result", "Ожидаемый результат"),
    ("scope", "Границы"),
    ("acceptance_criteria", "Критерии приёмки"),
    ("required_contract", "Требуемый контракт"),
    ("provided_contract", "Предоставляемый контракт"),
    ("lead", "Лид"),
    ("executor", "Исполнитель"),
    ("session_url", "Сессия"),
    ("repository_url", "Репозиторий"),
    ("repository_path", "Локальный репозиторий"),
    ("branch", "Ветка"),
    ("worktree", "Рабочая копия"),
    ("pr_url", "Pull request"),
    ("commit_url", "Коммит"),
    ("work_type", "Вид работы"),
    ("local_check", "План проверки"),
    ("result", "Результат"),
    ("check_result", "Результаты проверок"),
    ("artifact_url", "Артефакт"),
    ("merge_report", "Слияние PR"),
    ("after_epic", "После эпика"),
    ("integration_modules", "Проверяемые модули"),
    ("scenarios", "Сценарии взаимодействия"),
    ("environment", "Среда проверки"),
    ("reason", "Причина отмены"),
    ("duplicate_of", "Исходная работа"),
];
/// Derive a stable UUID in the API-required v4 format for subordinate objects.
/// This only prevents duplicate creation on retries; it is not a proof or content verification.
pub fn child_id(parent: &str, purpose: &str) -> String {
    let hash = Sha256::digest(format!("{parent}:{purpose}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}
/// Patch only named level-two sections; all other prose and sections remain byte-for-byte.
/// Null removes that field. Nested content should use level-three headings or deeper.
pub fn patch_description(original: &str, patch: &Value) -> String {
    let mut output = original.to_owned();
    for (key, label) in FIELDS {
        let Some(value) = patch.get(*key) else {
            continue;
        };
        let heading = format!("## {label}\n");
        let positions: Vec<usize> = output
            .match_indices(&heading)
            .filter(|(i, _)| *i == 0 || output.as_bytes()[i - 1] == b'\n')
            .map(|(i, _)| i)
            .collect();
        let rendered = if value.is_null() {
            String::new()
        } else {
            let body = value.as_str().map(str::to_owned).unwrap_or_else(|| {
                format!("```json\n{}\n```", serde_json::to_string(value).unwrap())
            });
            format!("{heading}{body}\n\n")
        };
        if let Some(&start) = positions.first() {
            let body = start + heading.len();
            let end = output[body..]
                .find("\n## ")
                .map(|i| body + i + 1)
                .unwrap_or(output.len());
            output.replace_range(start..end, &rendered);
        } else if !rendered.is_empty() {
            if !output.is_empty() && !output.ends_with("\n\n") {
                output.push_str("\n\n");
            }
            output.push_str(&rendered);
        }
    }
    output
}
/// Native persistence with fresh reads; no local workflow database or signed receipts.
#[derive(Clone)]
pub struct Store {
    /// Shared protected Linear API client.
    pub linear: Linear,
}

/// Validate the originating issue of a canonical state attachment before reading or updating it.
/// Native duplicate merges move attachments; `originalIssue` remains their owner when present.
/// Missing or foreign provenance fails closed without modifying the record.
fn validate_state_owner(attachment: &Value, id: &str) -> Result<()> {
    let owner = if attachment["originalIssue"].is_null() {
        &attachment["issue"]
    } else {
        &attachment["originalIssue"]
    };
    require(
        owner["id"] == id,
        "STATE_INVALID",
        "State attachment belongs to another issue",
    )
}

impl Store {
    /// Read a native object, distinguishing absence from authentication and partial errors.
    pub async fn optional(&self, query: &str, field: &str, id: &str) -> Result<Option<Value>> {
        match self.linear.object(query, field, id).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.code == "RECORD_MISSING" => Ok(None),
            Err(e) => Err(e),
        }
    }
    /// Read the deterministic metadata attachment for one originating issue, including native duplicate transfers.
    /// Foreign provenance is rejected; missing records remain unmanaged and reads never relocate attachments.
    pub async fn meta(&self, id: &str) -> Result<Option<Meta>> {
        let Some(a) = self
            .optional("QAttachmentById", "attachment", &child_id(id, "state"))
            .await?
        else {
            return Ok(None);
        };
        validate_state_owner(&a, id)?;
        let m: Meta = serde_json::from_value(a["metadata"]["workflow"].clone())
            .map_err(|_| Fault::new("STATE_INVALID", "Invalid workflow metadata"))?;
        require(
            m.schema == 2,
            "STATE_INVALID",
            "Unsupported workflow data version",
        )?;
        Ok(Some(m))
    }
    /// Fetch native issue data and its metadata; no writes occur during context reads.
    pub async fn work(&self, id: &str) -> Result<Work> {
        let native = self.linear.object("QIssue", "issue", id).await?;
        let meta = self.meta(native["id"].as_str().unwrap()).await?;
        let fields = meta.as_ref().map(|m| m.fields.clone()).unwrap_or(json!({}));
        Ok(Work {
            native,
            meta,
            fields,
        })
    }
    /// Create or update the canonical state attachment after validating its originating issue.
    /// A native transfer changes physical placement only; other issue records are never adopted or overwritten.
    /// Mutation uncertainty propagates to the caller.
    pub async fn save(&self, work: &Value, meta: &Meta) -> Result<()> {
        let id = work["id"].as_str().unwrap();
        let aid = child_id(id, "state");
        let metadata = json!({"workflow":meta});
        if let Some(attachment) = self.optional("QAttachmentById", "attachment", &aid).await? {
            validate_state_owner(&attachment, id)?;
            self.linear
                .call(
                    "MUpdateAttachment",
                    json!({"id":aid,"input":{"title":"Данные выполнения","metadata":metadata}}),
                )
                .await?;
        } else {
            self.linear.call("MUpsertRecord",json!({"input":{"id":aid,"issueId":id,"title":"Данные выполнения","url":format!("{}#execution",work["url"].as_str().unwrap_or("https://linear.app")),"metadata":metadata}})).await?;
        }
        Ok(())
    }
    /// Read every connection page selected by a root field name or JSON pointer, refusing missing or truncated graphs.
    /// `args` are copied into each request with bounded `first`/`after` pagination; no writes occur.
    pub async fn pages(&self, query: &str, field: &str, mut args: Value) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut after = Value::Null;
        for _ in 0..200 {
            args["first"] = json!(100);
            args["after"] = after.clone();
            let data = self.linear.call(query, args.clone()).await?;
            let c = if field.starts_with('/') {
                data.pointer(field).unwrap_or(&Value::Null)
            } else {
                &data[field]
            };
            let nodes = c["nodes"]
                .as_array()
                .ok_or_else(|| Fault::new("INCOMPLETE_DATA", "Missing connection page"))?;
            out.extend(nodes.iter().cloned());
            if c["pageInfo"]["hasNextPage"] == false {
                return Ok(out);
            }
            let next = c["pageInfo"]["endCursor"].clone();
            require(
                next.is_string() && next != after,
                "INCOMPLETE_DATA",
                "Pagination did not advance",
            )?;
            after = next;
        }
        Err(Fault::new(
            "INCOMPLETE_DATA",
            "Connection exceeds 200 pages; narrow the project",
        ))
    }
    /// Read the whole project hierarchy, including archived children needed for frozen membership.
    pub async fn graph(&self, project: &str) -> Result<Vec<Work>> {
        let nodes = self
            .pages(
                "QIssues",
                "issues",
                json!({"filter":{"project":{"id":{"eq":project}}},"includeArchived":true}),
            )
            .await?;
        let mut out = Vec::with_capacity(nodes.len());
        for native in nodes {
            let meta = self.meta(native["id"].as_str().unwrap()).await?;
            let fields = meta.as_ref().map(|m| m.fields.clone()).unwrap_or(json!({}));
            out.push(Work {
                native,
                meta,
                fields,
            });
        }
        // Follow recorded children too: a native project move must not hide unfinished scope.
        let mut index = 0;
        while index < out.len() {
            require(
                out.len() <= 20_000,
                "INCOMPLETE_DATA",
                "Hierarchy exceeds the read budget",
            )?;
            let ids = out[index]
                .meta
                .as_ref()
                .map(|m| m.children.clone())
                .unwrap_or_default();
            for id in ids {
                if !out.iter().any(|w| w.id() == id) {
                    match self.work(&id).await {
                        Ok(w) => out.push(w),
                        Err(e) if e.code == "RECORD_MISSING" => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            index += 1;
        }
        Ok(out)
    }
}

/// Return a comparison key across Linear's whitespace, punctuation escapes, links and unordered list markers.
/// Parser-recognized `-`/`*` list boundaries are encoded separately from normalized text,
/// so escaped literal markers and markers in code cannot collide with list syntax.
/// Unparsed backticks disable list folding conservatively. Text, destinations, headings and unknown
/// sections remain significant; the serialized key is comparison-only and performs no writes.
pub fn markdown_key(value: &str) -> String {
    // Native Linear links gain the target's title. In typed URL sections only,
    // the destination is the field value; preserve labels in all ordinary prose.
    let url_fields = read_fields(value).ok().map(|fields| {
        Value::Object(
            fields
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| key.ends_with("_url") || key.as_str() == "duplicate_of")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    });
    let source = url_fields
        .map(|fields| patch_description(value, &fields))
        .unwrap_or_else(|| value.to_owned());
    let events: Vec<_> = Parser::new(&source).into_offset_iter().collect();
    let ambiguous = events
        .iter()
        .any(|(event, _)| matches!(event, Event::Text(text) if text.contains('`')));
    let mut parts = Vec::new();
    let mut start = 0;
    if !ambiguous {
        for (event, range) in events {
            if matches!(event, Event::Start(Tag::Item))
                && matches!(source.as_bytes().get(range.start), Some(b'-' | b'*'))
            {
                parts.push(markdown_text_key(&source[start..range.start]));
                start = range.start + 1;
            }
        }
    }
    parts.push(markdown_text_key(&source[start..]));
    serde_json::to_string(&parts).unwrap()
}

/// Normalize an intact Markdown text segment using Linear's existing escape, link and whitespace rules.
/// List boundaries are excluded by the caller; this helper does not infer or rewrite list/code syntax.
fn markdown_text_key(source: &str) -> String {
    let mut text = String::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(char::is_ascii_punctuation) {
            text.push(chars.next().unwrap());
        } else {
            text.push(c);
        }
    }
    // Linear serializes bare URLs as Markdown links, including angle-bracket destinations.
    let mut offset = 0;
    while let Some(middle) = text[offset..].find("](").map(|i| offset + i) {
        let Some(open) = text[..middle].rfind('[') else {
            offset = middle + 2;
            continue;
        };
        let Some(close) = link_end(&text, middle + 2) else {
            break;
        };
        let label = &text[open + 1..middle];
        let destination = text[middle + 2..close]
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        if destination.starts_with("https://") || destination.starts_with("http://") {
            let replacement = if label == destination {
                destination.to_owned()
            } else {
                format!("[{label}]({destination})")
            };
            text.replace_range(open..=close, &replacement);
            offset = open + replacement.len();
        } else {
            offset = close + 1;
        }
    }
    text.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Find the closing Markdown link parenthesis after a destination start byte offset.
/// Angle-bracket destinations may contain parentheses; bare destinations must balance them.
fn link_end(text: &str, start: usize) -> Option<usize> {
    if text[start..].starts_with('<') {
        return text[start..].find(">)").map(|i| start + i + 1);
    }
    let mut depth = 1;
    for (offset, c) in text[start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse the recognized readable sections after manual edits; unknown sections stay unowned.
/// Invalid structured integration lists fail rather than retaining stale hidden values.
pub fn read_fields(description: &str) -> Result<Value> {
    let mut fields = json!({});
    for (key, label) in FIELDS {
        let heading = format!("## {label}\n");
        let starts: Vec<_> = description
            .match_indices(&heading)
            .filter(|(i, _)| *i == 0 || description.as_bytes()[i - 1] == b'\n')
            .collect();
        require(
            starts.len() <= 1,
            "INVALID_INPUT",
            format!("Duplicate section: {label}"),
        )?;
        if let Some((i, _)) = starts.first() {
            let start = i + heading.len();
            let end = description[start..]
                .find("\n## ")
                .map(|n| start + n)
                .unwrap_or(description.len());
            let body = description[start..end].trim();
            if !body.is_empty() {
                fields[*key] = if *key == "integration_modules" {
                    let body = body
                        .strip_prefix("```json")
                        .or_else(|| body.strip_prefix("```"))
                        .and_then(|s| s.trim().strip_suffix("```"))
                        .unwrap_or(body)
                        .trim();
                    // Linear escapes bare brackets on Markdown round trips.
                    let body = body.replace("\\[", "[").replace("\\]", "]");
                    serde_json::from_str(&body).map_err(|_| {
                        Fault::new(
                            "INVALID_INPUT",
                            "Integration Modules section must contain a JSON array of UUIDs",
                        )
                    })?
                } else if key.ends_with("_url") || *key == "duplicate_of" {
                    let destination = body
                        .strip_prefix('[')
                        .and_then(|s| s.split_once("]("))
                        .and_then(|(_, url)| url.strip_suffix(')'))
                        .unwrap_or(body)
                        .trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>');
                    json!(destination)
                } else {
                    json!(body)
                };
            }
        }
    }
    Ok(fields)
}
