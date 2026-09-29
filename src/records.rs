//! Minimal native attachment state and lossless editing of readable Markdown sections.
use crate::{
    linear::Linear,
    model::{Fault, Meta, Result, Work, require},
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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
/// Validate and parse one already-fetched state attachment for its known originating issue.
/// Shared by the single `meta` lookup and the bulk `graph` read so both apply identical
/// provenance/schema checks regardless of which query fetched the record.
fn parse_state_attachment(attachment: &Value, id: &str) -> Result<Meta> {
    validate_state_owner(attachment, id)?;
    let m: Meta = serde_json::from_value(attachment["metadata"]["workflow"].clone())
        .map_err(|_| Fault::new("STATE_INVALID", "Invalid workflow metadata"))?;
    require(
        m.schema == 2,
        "STATE_INVALID",
        "Unsupported workflow data version",
    )?;
    Ok(m)
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
        parse_state_attachment(&a, id).map(Some)
    }
    /// Read many deterministic state attachments in bounded chunks by native attachment ID,
    /// instead of one request per issue. A requested ID absent from the native page is simply
    /// missing from the result, never an error; callers decide what that means for their issue.
    /// ponytail: 100-ID `in` chunks, matching this client's existing page size; narrow further
    /// only if Linear's real IDComparator array limit proves smaller.
    async fn state_attachments(&self, ids: &[String]) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(100) {
            out.extend(
                self.pages(
                    "QStateAttachments",
                    "attachments",
                    json!({"filter":{"id":{"in":chunk}},"includeArchived":true}),
                )
                .await?,
            );
        }
        Ok(out)
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
    /// Reads one bulk page of state attachments (bounded chunks by deterministic ID) instead of
    /// one request per issue; a project's request count no longer scales with its issue count.
    pub async fn graph(&self, project: &str) -> Result<Vec<Work>> {
        let nodes = self
            .pages(
                "QIssues",
                "issues",
                json!({"filter":{"project":{"id":{"eq":project}}},"includeArchived":true}),
            )
            .await?;
        let state_ids: Vec<String> = nodes
            .iter()
            .map(|n| child_id(n["id"].as_str().unwrap(), "state"))
            .collect();
        let attachments = self.state_attachments(&state_ids).await?;
        let mut by_id: BTreeMap<String, Value> = BTreeMap::new();
        for a in attachments {
            if let Some(aid) = a["id"].as_str() {
                by_id.insert(aid.to_owned(), a);
            }
        }
        let mut out = Vec::with_capacity(nodes.len());
        for (native, state_id) in nodes.into_iter().zip(state_ids.iter()) {
            let id = native["id"].as_str().unwrap().to_owned();
            let meta = match by_id.get(state_id) {
                Some(a) => Some(parse_state_attachment(a, &id)?),
                None => None,
            };
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

/// Byte ranges of inline code spans and fenced/indented code blocks in `text`, using the same
/// CommonMark parser as the rest of this module. Literal code content inside these ranges is
/// never pattern-matched as a link or bare autolink target; a URL or `[label](url)` shape found
/// there is opaque text, not markup, regardless of what surrounds the code elsewhere.
fn code_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut fence_start = None;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Code(_) => ranges.push(range),
            Event::Start(Tag::CodeBlock(_)) => fence_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = fence_start.take() {
                    ranges.push(start..range.end);
                }
            }
            _ => {}
        }
    }
    ranges
}

/// Report whether byte offset `pos` in the text `ranges` were computed from lies inside code.
fn in_code(ranges: &[std::ops::Range<usize>], pos: usize) -> bool {
    ranges.iter().any(|r| r.contains(&pos))
}

/// Compare requested Markdown with Linear's native rendering without losing intentional labels.
/// A bare HTTP(S) URL may gain a native title even when closing prose punctuation follows it;
/// a prose domain may become the same-label `http://` link at its original word boundary;
/// an email address, angle-bracketed `<address>` or bare in prose, may become the
/// same-label `mailto:` link.
/// Different destinations or labels, code, extra prose and list boundaries still differ: the
/// per-part walk only ever applies this leniency outside a code span or block, on either side,
/// so unrelated code elsewhere in the same document never blocks a real match and a literal
/// code region is never silently treated as a link or vice versa.
/// The comparison is directional and never rewrites either source.
pub fn markdown_equivalent(expected: &str, actual: &str) -> bool {
    let expected_key = markdown_key(expected);
    let actual_key = markdown_key(actual);
    if expected_key == actual_key {
        return true;
    }
    let expected_parts: Vec<String> = serde_json::from_str(&expected_key).unwrap();
    let actual_parts: Vec<String> = serde_json::from_str(&actual_key).unwrap();
    expected_parts.len() == actual_parts.len()
        && expected_parts
            .iter()
            .zip(&actual_parts)
            .all(|(expected, actual)| same_text_with_native_link_title(expected, actual))
}

/// Report whether `domain` is a dotted sequence of labels a native autolinker can link:
/// at least one dot, then nonempty alphanumeric/hyphen labels that start and end
/// alphanumerically, with an alphabetic top-level label of at least two characters.
fn linkable_domain(domain: &str) -> bool {
    domain.contains('.')
        && domain.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && part
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
        && domain
            .rsplit('.')
            .next()
            .is_some_and(|part| part.len() >= 2 && part.bytes().all(|b| b.is_ascii_alphabetic()))
}

/// Report whether `label` is a plausible email address: a nonempty local part without
/// Markdown link syntax, then a linkable domain after the last `@`.
fn email_address(label: &str) -> bool {
    label.rsplit_once('@').is_some_and(|(local, domain)| {
        !local.is_empty()
            && !local.contains(['<', '>', '[', ']', '(', ')', ' '])
            && linkable_domain(domain)
    })
}

/// Report whether a requested bare URL or domain token ends at `rest`, the remainder of the
/// requested text starting exactly after that token. A token ends at end of input, whitespace,
/// or one of the closing prose punctuation characters `,;:!?)]}`; a period also ends it only
/// when no alphanumeric follows, so a URL that genuinely continues (for example `…/a.foo`)
/// is never split at an interior-looking dot. Linear's autolinker closes generated links
/// before exactly this punctuation, so requiring the boundary keeps destinations exact while
/// tolerating where native serialization places the link end.
fn prose_boundary(rest: &str) -> bool {
    rest.chars().next().is_none_or(|c| {
        c.is_whitespace()
            || matches!(c, ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
            || (c == '.'
                && rest[1..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_ascii_alphanumeric()))
    })
}

/// Compare normalized text while consuming only a native link aligned to a requested bare URL,
/// a same-label prose domain or a same-label email address (angle-bracketed autolink or bare
/// prose). URL destinations, email addresses and domain word boundaries must match exactly;
/// explicit labels, changed destinations and surrounding prose remain visible. Bare forms may
/// end at any closing prose punctuation, exactly where Linear's autolinker closes a link.
fn same_text_with_native_link_title(mut expected: &str, mut actual: &str) -> bool {
    let expected_len = expected.len();
    let actual_len = actual.len();
    let expected_code = code_ranges(expected);
    let actual_code = code_ranges(actual);
    let mut previous = None;
    while !expected.is_empty() && !actual.is_empty() {
        let in_code = in_code(&expected_code, expected_len - expected.len())
            || in_code(&actual_code, actual_len - actual.len());
        if !in_code
            && actual.starts_with('[')
            && let Some(middle) = actual.find("](")
            && !actual[1..middle].bytes().any(|b| b == b'[' || b == b']')
            && let Some(close) = link_end(actual, middle + 2)
        {
            let label = &actual[1..middle];
            let destination = actual[middle + 2..close]
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            let bare_url = expected.starts_with(destination)
                && (destination.starts_with("https://") || destination.starts_with("http://"))
                && prose_boundary(&expected[destination.len()..]);
            let bare_domain = previous.is_none_or(|c: char| {
                c.is_whitespace() || matches!(c, '(' | '[' | '{' | '"' | '\'')
            }) && linkable_domain(label)
                && destination == format!("http://{label}")
                && expected.starts_with(label)
                && prose_boundary(&expected[label.len()..]);
            let angle_email = expected.starts_with(&format!("<{label}>"))
                && prose_boundary(&expected[label.len() + 2..]);
            let bare_email = email_address(label)
                && destination == format!("mailto:{label}")
                && (angle_email
                    || (expected.starts_with(label) && prose_boundary(&expected[label.len()..])));
            if bare_url || bare_domain || bare_email {
                let consumed = if bare_url {
                    destination.len()
                } else if bare_email && angle_email {
                    label.len() + 2
                } else {
                    label.len()
                };
                previous = expected[..consumed].chars().last();
                expected = &expected[consumed..];
                actual = &actual[close + 1..];
                continue;
            }
        }
        let left = expected.chars().next().unwrap();
        let right = actual.chars().next().unwrap();
        if left != right {
            return false;
        }
        expected = &expected[left.len_utf8()..];
        actual = &actual[right.len_utf8()..];
        previous = Some(left);
    }
    expected.is_empty() && actual.is_empty()
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
    // Ranges are recomputed each pass: an applied replacement shifts later byte offsets, and
    // code content is never rewritten, so its positions never need to survive a shift anyway.
    let mut offset = 0;
    while let Some(middle) = text[offset..].find("](").map(|i| offset + i) {
        let Some(open) = text[..middle].rfind('[') else {
            offset = middle + 2;
            continue;
        };
        let Some(close) = link_end(&text, middle + 2) else {
            break;
        };
        let code = code_ranges(&text);
        if in_code(&code, open) || in_code(&code, close) {
            offset = close + 1;
            continue;
        }
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
