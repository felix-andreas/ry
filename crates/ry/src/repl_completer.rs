//! The REPL's analysis-backed Tab completion: the session's accepted lines
//! form a script document, so completions see the standard-library corpus
//! (typed stubs and export manifests) plus every binding the session itself
//! defined. It runs `ide::completion` over the session as a script.

use repl::reedline::{Span, Suggestion};
use salsa::Setter;
use semantics::{DocumentKind, ProjectFiles, RootDatabase, SourceFile};

pub struct AnalysisCompleter {
    db: RootDatabase,
    files: ProjectFiles,
    file: SourceFile,
    /// Accepted input so far, newline-terminated: the prefix of the
    /// session-as-script document; the line under edit is appended per
    /// completion request.
    session: String,
}

impl AnalysisCompleter {
    pub fn new() -> AnalysisCompleter {
        let db = RootDatabase::default();
        semantics::stubs::install_shipped_stubs(&db);
        let file = SourceFile::new(&db, String::new(), DocumentKind::Script);
        let files = ProjectFiles::new(&db, vec![file]);
        AnalysisCompleter {
            db,
            files,
            file,
            session: String::new(),
        }
    }
}

impl Default for AnalysisCompleter {
    fn default() -> AnalysisCompleter {
        AnalysisCompleter::new()
    }
}

impl repl::SessionCompleter for AnalysisCompleter {
    fn accept(&mut self, line: &str) {
        self.session.push_str(line);
        self.session.push('\n');
    }

    fn complete(&mut self, line: &str, position: usize) -> Vec<Suggestion> {
        let position = position.min(line.len());
        let source = format!("{}{line}\n", self.session);
        let offset = self.session.len() + position;
        self.file.set_text(&mut self.db).to(source);
        let Some(result) = ide::completion(
            &self.db,
            self.files,
            self.file,
            syntax::TextSize::new(offset as u32),
        ) else {
            return Vec::new();
        };
        // The replacement span is the identifier being typed: scan back over
        // R name characters (`$`, `@`, and `::` boundaries end it, matching
        // how the completion query itself is scoped).
        let start = line[..position]
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '.' || *c == '_')
            .last()
            .map(|(index, _)| index)
            .unwrap_or(position);
        result
            .items
            .into_iter()
            .map(|item| Suggestion {
                value: item.label,
                description: item.detail.or(item.documentation),
                style: None,
                extra: None,
                span: Span {
                    start,
                    end: position,
                },
                append_whitespace: false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use repl::SessionCompleter;

    #[test]
    fn stdlib_names_complete_with_types() {
        let mut completer = AnalysisCompleter::new();
        let line = "seq_l";
        let suggestions = completer.complete(line, line.len());
        let seq_len = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "seq_len")
            .expect("seq_len completes");
        assert_eq!(
            seq_len.description.as_deref(),
            Some("fn(length.out: Any) -> integer[]")
        );
        assert_eq!(seq_len.span, Span { start: 0, end: 5 });
    }

    #[test]
    fn session_bindings_complete_after_accept() {
        let mut completer = AnalysisCompleter::new();
        completer.accept("first_total <- 1L");
        completer.accept("second_total <- first_total * 2");
        let line = "print(first_t";
        let suggestions = completer.complete(line, line.len());
        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.value == "first_total"),
            "session bindings must complete: {:?}",
            suggestions
                .iter()
                .map(|suggestion| &suggestion.value)
                .collect::<Vec<_>>()
        );
        let span = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "first_total")
            .map(|suggestion| suggestion.span);
        assert_eq!(span, Some(Span { start: 6, end: 13 }));
    }

    #[test]
    fn manifest_only_exports_complete_undecorated() {
        let mut completer = AnalysisCompleter::new();
        let line = "bitwAn";
        let suggestions = completer.complete(line, line.len());
        let bitw_and = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "bitwAnd")
            .expect("manifest names complete");
        assert_eq!(
            bitw_and.description.as_deref(),
            Some("From the `base` package.")
        );
    }

    #[test]
    fn namespace_completion_lists_exports() {
        let mut completer = AnalysisCompleter::new();
        let line = "utils::aspel";
        let suggestions = completer.complete(line, line.len());
        assert!(
            suggestions
                .iter()
                .any(|suggestion| suggestion.value == "aspell"),
            "qualified completion must see manifest exports"
        );
        // The span replaces only the name after `::`.
        let aspell = suggestions
            .iter()
            .find(|suggestion| suggestion.value == "aspell")
            .expect("aspell suggestion");
        assert_eq!(aspell.span, Span { start: 7, end: 12 });
    }
}
