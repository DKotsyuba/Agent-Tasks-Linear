//! Pure, network-free Markdown heading selection/replacement and native currentness
//! classification, shared by document read/search/write handlers.
use crate::model::{Result, require};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde_json::{Value, json};

/// A native Document (or search row) is current iff both visibility timestamps are absent.
/// Title, prose and missing hashes never change this classification.
pub fn is_current(archived_at: &Value, hidden_at: &Value) -> bool {
    archived_at.is_null() && hidden_at.is_null()
}

/// Convenience over a raw native Document payload carrying `archivedAt`/`hiddenAt`.
pub fn document_is_current(document: &Value) -> bool {
    is_current(&document["archivedAt"], &document["hiddenAt"])
}

/// One Document link with native ownership visibility: id/title/url, whatever `project`/`issue`
/// parent the caller already fetched (passed through verbatim, native `null` when the Document
/// has no such parent; never a fresh lookup), plus updated_at, archived, hidden and the derived
/// current flag, the same currentness signal every document-listing route (list, search and
/// current-context links) agrees on.
pub fn document_link(document: &Value) -> Value {
    json!({
        "id": document["id"],
        "title": document["title"],
        "url": document["url"],
        "project": document["project"],
        "issue": document["issue"],
        "updated_at": document["updatedAt"],
        "archived": !document["archivedAt"].is_null(),
        "hidden": !document["hiddenAt"].is_null(),
        "current": document_is_current(document),
    })
}

/// One Markdown heading with its exact byte span in the source document.
/// Headings inside fenced/indented code blocks are never produced here, because the
/// CommonMark parser reads that text as a code block, not as a heading.
struct Heading {
    /// ATX/setext heading level, 1 through 6.
    level: u8,
    /// Rendered heading text: inline code and soft/hard breaks are folded into plain characters
    /// and a single space respectively, then the whole line is trimmed.
    text: String,
    /// Byte offset where the heading itself starts (its line, for setext its text line).
    start: usize,
    /// Byte offset where the heading's own line(s) end and its body begins.
    body_start: usize,
}

/// One selected section: its matched heading text and position among all document headings,
/// plus the exact body bytes between that heading and the next same-or-higher heading.
#[derive(Debug)]
pub struct Section<'a> {
    /// The matched heading's own rendered text, trimmed.
    pub heading: String,
    /// 1-based position of the matched heading among every heading in the document.
    pub index: usize,
    /// Total number of headings in the document, for navigation alongside `index`.
    pub count: usize,
    /// Exact body bytes between the heading's own line and the next same-or-higher heading (or
    /// the end of the document), excluding the heading line itself.
    pub body: &'a str,
}

/// Collect every heading in `content`, in document order, using the same CommonMark parser as
/// the rest of this module. A heading inside a fenced/indented code block never appears here,
/// because the parser reads that text as code, not as a heading.
fn headings(content: &str) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut open: Option<(u8, usize, String)> = None;
    for (event, range) in Parser::new(content).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                open = Some((level as u8, range.start, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, text)) = open.take() {
                    out.push(Heading {
                        level,
                        text: text.trim().to_owned(),
                        start,
                        body_start: range.end,
                    });
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, text)) = &mut open {
                    text.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, text)) = &mut open {
                    text.push(' ');
                }
            }
            _ => {}
        }
    }
    out
}

/// Find the single heading matching `heading_text` (trimmed, exact) and the byte offset where
/// its section ends: the start of the next heading at the same or a higher (numerically lower)
/// level, or the end of the document. Fails before any write on a missing or ambiguous heading.
fn locate(content: &str, heading_text: &str) -> Result<(Vec<Heading>, usize, usize)> {
    let wanted = heading_text.trim();
    let items = headings(content);
    let matches: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, h)| h.text == wanted)
        .map(|(i, _)| i)
        .collect();
    require(
        !matches.is_empty(),
        "SECTION_NOT_FOUND",
        format!("No section heading matches {heading_text:?}"),
    )?;
    require(
        matches.len() == 1,
        "SECTION_AMBIGUOUS",
        format!(
            "{} sections match heading {heading_text:?}; use a more specific heading",
            matches.len()
        ),
    )?;
    let i = matches[0];
    let level = items[i].level;
    let end = items[i + 1..]
        .iter()
        .find(|h| h.level <= level)
        .map(|h| h.start)
        .unwrap_or(content.len());
    Ok((items, i, end))
}

/// Read one full section: heading text, its 1-based position and the total heading count
/// (for navigation), and the exact body bytes outside the heading line itself.
pub fn find_section<'a>(content: &'a str, heading_text: &str) -> Result<Section<'a>> {
    let (items, i, end) = locate(content, heading_text)?;
    Ok(Section {
        heading: items[i].text.clone(),
        index: i + 1,
        count: items.len(),
        body: &content[items[i].body_start..end],
    })
}

/// Replace only the body of the section matching `heading_text`, preserving the heading line
/// and every byte outside the section's range unchanged.
pub fn replace_section(content: &str, heading_text: &str, new_body: &str) -> Result<String> {
    let (items, i, end) = locate(content, heading_text)?;
    let mut out = String::with_capacity(content.len() + new_body.len() + 1);
    out.push_str(&content[..items[i].body_start]);
    out.push_str(new_body);
    // A replacement body without its own trailing newline would otherwise run directly into
    // whatever follows: a plain heading loses the line break an ATX heading needs to parse at
    // all, and a fenced code block loses the closing fence's own line, silently absorbing every
    // byte after it (including the next heading) as more code. One newline is the minimum that
    // keeps both the replaced body and everything after it independently parseable again.
    if end < content.len() && !new_body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&content[end..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currentness_reads_both_native_fields() {
        assert!(is_current(&Value::Null, &Value::Null));
        assert!(!is_current(
            &Value::String("2026-01-01".into()),
            &Value::Null
        ));
        assert!(!is_current(
            &Value::Null,
            &Value::String("2026-01-01".into())
        ));
        assert!(document_is_current(
            &serde_json::json!({"archivedAt": null, "hiddenAt": null})
        ));
        assert!(!document_is_current(
            &serde_json::json!({"archivedAt": null, "hiddenAt": "2026-01-01"})
        ));
    }

    #[test]
    fn reads_and_preserves_neighbours() {
        let doc =
            "# Title\n\nIntro.\n\n## Описание\n\nBody one.\nMore body.\n\n## Границы\n\nOther.\n";
        let section = find_section(doc, "Описание").unwrap();
        assert_eq!(section.heading, "Описание");
        assert_eq!(section.index, 2);
        assert_eq!(section.count, 3);
        assert_eq!(section.body, "\nBody one.\nMore body.\n\n");

        let replaced = replace_section(doc, "Описание", "New body.\n\n").unwrap();
        assert_eq!(
            replaced,
            "# Title\n\nIntro.\n\n## Описание\nNew body.\n\n## Границы\n\nOther.\n"
        );
    }

    #[test]
    fn replacement_without_a_trailing_newline_keeps_the_next_heading_independent() {
        let doc = "# Control\n\n## First\n\nuntouched\n\n## Work\n\nold\n\n## Last\n\nkeep\n";
        // A valid, complete fenced block with no trailing newline at all: the naive splice
        // would run the closing fence directly into "## Last", leaving the fence unclosed and
        // absorbing "Last" as code instead of a heading.
        let replaced = replace_section(doc, "Work", "new\n\n```text\ncode\n```").unwrap();
        // Reparse the edited document itself, not just compare against one fixture string: a
        // regression that only breaks the byte layout without changing the exact expected
        // string would otherwise slip through undetected.
        let control = find_section(&replaced, "Control").unwrap();
        assert_eq!(control.index, 1);
        assert_eq!(control.count, 4);
        let first = find_section(&replaced, "First").unwrap();
        assert_eq!(first.body, "\nuntouched\n\n");
        let work = find_section(&replaced, "Work").unwrap();
        assert_eq!(work.body, "new\n\n```text\ncode\n```\n");
        let last = find_section(&replaced, "Last").unwrap();
        assert_eq!(last.index, 4);
        assert_eq!(last.count, 4);
        assert_eq!(last.body, "\nkeep\n");
    }

    #[test]
    fn nested_levels_bound_the_section_at_same_or_higher_heading() {
        let doc = "# A\n\n## B\n\n### C\n\nleaf\n\n## D\n\ntail\n";
        // "B" contains nested "C"; its section must stop at the next H2 "D", not at "C".
        let section = find_section(doc, "B").unwrap();
        assert_eq!(section.body, "\n### C\n\nleaf\n\n");
        let leaf = find_section(doc, "C").unwrap();
        assert_eq!(leaf.body, "\nleaf\n\n");
    }

    #[test]
    fn ignores_headings_inside_fenced_code() {
        let doc = "## Real\n\n```\n## Not a heading\n```\n\nafter\n";
        let err = find_section(doc, "Not a heading").unwrap_err();
        assert_eq!(err.code, "SECTION_NOT_FOUND");
        let section = find_section(doc, "Real").unwrap();
        assert_eq!(section.body, "\n```\n## Not a heading\n```\n\nafter\n");
    }

    #[test]
    fn rejects_duplicate_headings_as_ambiguous() {
        let doc = "## Same\n\none\n\n## Same\n\ntwo\n";
        let err = find_section(doc, "Same").unwrap_err();
        assert_eq!(err.code, "SECTION_AMBIGUOUS");
    }

    #[test]
    fn rejects_missing_heading() {
        let err = find_section("## Only\n\nbody\n", "Absent").unwrap_err();
        assert_eq!(err.code, "SECTION_NOT_FOUND");
    }

    #[test]
    fn headings_collects_level_and_rendered_text_in_document_order() {
        // A setext heading's text may itself span multiple lines: the soft break between them
        // folds into a single space, exercising that path alongside an ATX heading with an
        // inline code span.
        let doc = "Soft\nbreak heading\n===\n\n## `Code` heading\n\nBody.\n";
        let items = headings(doc);
        let levels_and_text: Vec<(u8, &str)> =
            items.iter().map(|h| (h.level, h.text.as_str())).collect();
        assert_eq!(
            levels_and_text,
            vec![(1, "Soft break heading"), (2, "Code heading")]
        );
    }

    #[test]
    fn matches_unicode_heading_text_exactly() {
        let doc = "## Заголовок ключа 🔑\n\nтело\n";
        let section = find_section(doc, "Заголовок ключа 🔑").unwrap();
        assert_eq!(section.body, "\nтело\n");
    }
}
