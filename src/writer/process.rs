//! Writer process runs (spec §6.1): the `<writer-process>` request
//! markup the agent edits from. Like the S2 request markup, hostile
//! text is sanitized or refused so pasted content cannot forge
//! fields; the forbidden-tag list is shared with `request.rs`.

use super::markers::ParseOutput;
use super::request::{check_hostile_tags, sanitize_header, sanitize_text};
use super::{line_of, WriterError, TARGET_EXCERPT_CHARS, TRUNCATION_MARKER};

/// Closing-tag prefixes that would break out of the process markup
/// (any case): the envelope plus every inner field. Kept separate
/// from the request list so neither family false-refuses the
/// other's legitimate text.
const FORBIDDEN_TAGS: [&str; 7] = [
    "</writer-process",
    "</markers",
    "</marker",
    "</prompt",
    "</target",
    "</rules",
    "</file",
];

/// Refuse prompt/target text carrying a process-markup tag.
fn check_hostile(field: &'static str, text: &str) -> Result<(), WriterError> {
    check_hostile_tags(field, text, &FORBIDDEN_TAGS)
}

/// Wrap or standalone, as the agent reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessShape {
    Wrap,
    Standalone,
}

impl ProcessShape {
    /// Lowercase shape word used in the markup.
    pub fn name(&self) -> &'static str {
        match self {
            ProcessShape::Wrap => "wrap",
            ProcessShape::Standalone => "standalone",
        }
    }
}

/// One parsed marker as the agent sees it: position, shape, and
/// capped excerpts (the file holds the rest). `whole` is the
/// pre-run char span, for the live `⟳` gutter rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessMarker {
    pub index: usize,
    pub line: u32,
    pub whole: std::ops::Range<usize>,
    pub shape: ProcessShape,
    pub verb: Option<String>,
    pub prompt: String,
    pub target: Option<String>,
}

/// A run request over parsed markers: self-contained rules plus the
/// marker list. Built by `AppState` when the human presses Process;
/// empty lists render (the press path refuses those first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterProcess {
    pub id: u64,
    pub doc_name: String,
    pub rev: u64,
    pub markers: Vec<ProcessMarker>,
}

impl WriterProcess {
    /// Build from M1 parse output over `text`. Prompts and targets
    /// are sanitized; either carrying a markup-closing tag refuses
    /// the whole run with [`WriterError::HostileMarkup`].
    pub fn over_markers(
        id: u64,
        doc_name: &str,
        text: &str,
        rev: u64,
        out: &ParseOutput,
    ) -> Result<Self, WriterError> {
        let mut markers = Vec::with_capacity(out.markers.len());
        for (index, m) in out.markers.iter().enumerate() {
            let chars: Vec<char> = text.chars().collect();
            let slice =
                |range: &std::ops::Range<usize>| chars[range.clone()].iter().collect::<String>();
            let verb = m.verb.as_ref().map(|v| slice(v));
            let prompt = sanitize_text(&slice(&m.prompt));
            check_hostile("prompt", &prompt)?;
            let target = match m.target.as_ref() {
                Some(t) => {
                    let full = sanitize_text(&slice(t));
                    check_hostile("target", &full)?;
                    Some(excerpt(&full))
                }
                None => None,
            };
            let line = line_of(text, m.whole.start).ok_or(WriterError::RangeOutOfBounds)?;
            markers.push(ProcessMarker {
                index,
                line,
                whole: m.whole.clone(),
                shape: if m.target.is_some() {
                    ProcessShape::Wrap
                } else {
                    ProcessShape::Standalone
                },
                verb,
                prompt,
                target,
            });
        }
        Ok(WriterProcess {
            id,
            doc_name: sanitize_header(doc_name),
            rev,
            markers,
        })
    }

    /// Render the `[forge writer …]` header plus `<writer-process>`
    /// body: rules first, then one `<marker>` per entry.
    pub fn markup(&self) -> String {
        let mut out = format!(
            "[forge writer \"{}\" process {} file rev {}, {} marker{}]:\n\
             <writer-process id=\"{}\" file=\"{}\" rev=\"{}\">\n\
             <rules>\n\
             {}\n\
             </rules>\n\
             <markers>\n",
            self.doc_name,
            self.id,
            self.rev,
            self.markers.len(),
            if self.markers.len() == 1 { "" } else { "s" },
            self.id,
            self.doc_name,
            self.rev,
            PROCESS_RULES,
        );
        for m in &self.markers {
            out.push_str(&format!(
                "<marker index=\"{}\" line=\"{}\" shape=\"{}\" verb=\"{}\">\n<prompt>\n{}\n</prompt>\n<target>\n{}\n</target>\n</marker>\n",
                m.index,
                m.line,
                m.shape.name(),
                m.verb.as_deref().unwrap_or("(none)"),
                m.prompt,
                m.target.as_deref().unwrap_or("(none)"),
            ));
        }
        out.push_str("</markers>\n</writer-process>");
        out
    }
}

/// Self-contained run rules: everything the agent needs, no PRD.
/// Processing a marker REPLACES it: the marker text leaves the file
/// and its result takes its place.
const PROCESS_RULES: &str = "Process the markers below in index order, one at a time, editing the file named in file= above directly with your file tools. WRAP (@@verb prompt@@target@@): replace the WHOLE marker (from the opening @@ through the closing @@ or @@end) with the target rewritten per the prompt. STANDALONE (@@verb prompt @@end): replace the WHOLE marker with the content the prompt asks for, written at that position. QUESTION (verb ask or ending in ?): do not rewrite; replace the marker with its target text unchanged (standalone: remove it), and send the answer with writer_answer(run, index, answer). After EACH marker, write the file before starting the next (the user watches it change live). Change nothing outside marker spans; all other text must stay byte-identical. Line numbers are pre-run positions; earlier edits shift them, so re-locate each marker by its text. Report started before editing (it lights the progress gutter), then each marker with writer_run_report(run, index, status, note) where status is done | skipped | failed (failed/skipped: leave that marker untouched, give the reason in note). When all markers are handled, call writer_run_done(run, summary) even if some failed.";

/// First [`TARGET_EXCERPT_CHARS`] chars plus the truncation marker
/// when cut (it names `writer_read` for the rest).
fn excerpt(full: &str) -> String {
    if full.chars().count() <= TARGET_EXCERPT_CHARS {
        return full.to_string();
    }
    let mut cut: String = full.chars().take(TARGET_EXCERPT_CHARS).collect();
    cut.push_str(TRUNCATION_MARKER);
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::markers::parse_markers;

    const DOC: &str = "alpha @@fix typo@@this is teh@@\n\n@@ask capital@@Paris@@\n";

    fn built() -> WriterProcess {
        let out = parse_markers(DOC);
        assert_eq!(out.markers.len(), 2, "fixture parses");
        WriterProcess::over_markers(1, "notes.md", DOC, 3, &out).expect("clean fixture")
    }

    #[test]
    fn process_markup_carries_all_fields() {
        let body = built().markup();
        assert!(
            body.contains("[forge writer \"notes.md\" process 1 file rev 3, 2 markers]:"),
            "header: {body}"
        );
        assert!(
            body.contains("<writer-process id=\"1\" file=\"notes.md\" rev=\"3\">"),
            "body: {body}"
        );
        assert!(body.contains("<rules>"), "rules: {body}");
        assert!(body.contains("</writer-process>"), "body: {body}");
        assert!(
            body.contains("<marker index=\"0\" line=\"1\" shape=\"wrap\" verb=\"fix\">"),
            "first marker: {body}"
        );
        assert!(body.contains("<prompt>\ntypo\n</prompt>"), "body: {body}");
        assert!(
            body.contains("<target>\nthis is teh\n</target>"),
            "body: {body}"
        );
        assert!(
            body.contains("<marker index=\"1\" line=\"3\" shape=\"wrap\" verb=\"ask\">"),
            "second marker: {body}"
        );
    }

    #[test]
    fn standalone_markup_has_no_target() {
        let text = "@@note hi @@end\n";
        let out = parse_markers(text);
        let body = WriterProcess::over_markers(2, "a.md", text, 0, &out)
            .expect("clean")
            .markup();
        assert!(
            body.contains("<marker index=\"0\" line=\"1\" shape=\"standalone\" verb=\"note\">"),
            "body: {body}"
        );
        assert!(body.contains("<target>\n(none)\n</target>"), "body: {body}");
    }

    #[test]
    fn missing_verb_renders_as_none() {
        let text = "@@just do it@@target here@@";
        let out = parse_markers(text);
        let body = WriterProcess::over_markers(3, "a.md", text, 0, &out)
            .expect("clean")
            .markup();
        assert!(body.contains("verb=\"(none)\""), "body: {body}");
        assert!(body.contains("<prompt>\njust do it\n</prompt>"), "body: {body}");
    }

    #[test]
    fn target_excerpt_caps_at_500_chars() {
        let big = "x".repeat(600);
        let text = format!("@@fix@@{big}@@");
        let out = parse_markers(&text);
        let proc = WriterProcess::over_markers(4, "a.md", &text, 0, &out).expect("clean");
        assert_eq!(proc.markers[0].target.as_ref().expect("target").chars().count(), 500 + crate::writer::TRUNCATION_MARKER.chars().count());
        assert!(proc.markup().contains(crate::writer::TRUNCATION_MARKER), "marked");
        // Prompts are short by nature and pass whole.
        let prompt = "y".repeat(600);
        let text = format!("@@{prompt}@@t@@");
        let out = parse_markers(&text);
        let proc = WriterProcess::over_markers(5, "a.md", &text, 0, &out).expect("clean");
        assert_eq!(proc.markers[0].prompt.chars().count(), 600);
    }

    #[test]
    fn hostile_prompt_or_target_is_refused() {
        let text = "@@ok</marker> @@t@@";
        let out = parse_markers(text);
        assert_eq!(
            WriterProcess::over_markers(1, "a.md", text, 0, &out).unwrap_err(),
            WriterError::HostileMarkup("prompt")
        );
        let text = "@@fix@@a </writer-process> b@@";
        let out = parse_markers(text);
        assert_eq!(
            WriterProcess::over_markers(1, "a.md", text, 0, &out).unwrap_err(),
            WriterError::HostileMarkup("target")
        );
    }

    #[test]
    fn doc_name_cannot_break_the_header_line() {
        let proc = built();
        let body = WriterProcess::over_markers(1, "a\nb\"c.md", DOC, 0, &parse_markers(DOC))
            .expect("clean")
            .markup();
        let header = body.lines().next().unwrap();
        assert_eq!(
            header,
            "[forge writer \"a�b'c.md\" process 1 file rev 0, 2 markers]:",
            "header: {header}"
        );
        assert_eq!(header.chars().filter(|&c| c == '"').count(), 2);
        let _ = proc;
    }

    /// The agent obeys the rules text, so it must state every
    /// effect: what each shape writes, write-after-each, the
    /// byte-identical surround, stale line numbers, failure
    /// handling, and the status close-out.
    #[test]
    fn rules_state_each_shape_effect_and_the_run_discipline() {
        let body = built().markup();
        for needle in [
            "replace the WHOLE marker",
            "rewritten per the prompt",
            "written at that position",
            "QUESTION",
            "writer_answer(run, index, answer)",
            "write the file before starting the next",
            "byte-identical",
            "re-locate each marker by its text",
            "done | skipped | failed",
            "writer_run_done(run, summary)",
        ] {
            assert!(body.contains(needle), "rules miss {needle}: {body}");
        }
        assert!(
            !body.contains("Never add or remove"),
            "inverted marker rule survives: {body}"
        );
    }

    /// The tool and the rules text share one status vocabulary: every
    /// wire status the tool accepts is named in the rules, so the two
    /// cannot drift apart.
    #[test]
    fn tool_status_set_matches_the_rules_text() {
        use crate::app::writer::runs::RunMarkerStatus;
        let body = built().markup();
        for status in ["started", "done", "skipped", "failed"] {
            assert!(
                RunMarkerStatus::parse(status).is_some(),
                "tool rejects '{status}'"
            );
            assert!(
                body.contains(status),
                "rules never name '{status}': {body}"
            );
        }
        assert!(
            RunMarkerStatus::parse("blocked").is_none(),
            "the old 'blocked' status must go"
        );
    }

    #[test]
    fn empty_marker_list_renders_without_refusal() {
        let out = parse_markers("plain text\n");
        let body = WriterProcess::over_markers(9, "a.md", "plain text\n", 0, &out)
            .expect("empty ok")
            .markup();
        assert!(body.contains(", 0 markers]:"), "body: {body}");
        assert!(body.contains("</markers>"), "body: {body}");
    }
}
