//! Style lints: syntax-level checks over the parse tree plus the
//! naming-driven unused-parameter and shadow lints.
//!
//! The legacy stack also carried a `missing-comma` lint compensating for
//! tree-sitter silently accepting `f(1 2)`; the hand parser rejects that
//! input with a syntax error (as R itself does), so the lint no longer
//! exists. The configuration key is still accepted for compatibility, and it
//! has no effect.

use crate::diagnostics::{Diagnostic, Severity};
use crate::{Db, SourceFile, item_naming, item_spans, parse};
use syntax::ast::AstNode;
use syntax::{SyntaxKind, SyntaxNode, TextRange};

/// A lint's configured level: keep its built-in severity, force one, or
/// disable it. The `[lint]` table keys each level by the lint's stable code
/// (`assignment-operator = "off"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LintLevel {
    #[default]
    Default,
    Off,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub enum NameStyle {
    #[serde(alias = "camelCase")]
    Camel,
    #[serde(alias = "snake_case")]
    Snake,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct LintConfig {
    /// `naming-style` is configured by its style value (`None` disables it).
    pub naming_style: Option<NameStyle>,
    pub assignment_operator: LintLevel,
    pub boolean_shorthand: LintLevel,
    /// Obsolete: a missing argument comma is a parse error on this stack.
    /// Accepted so existing configs keep loading; never read.
    pub missing_comma: LintLevel,
    pub trailing_comma: LintLevel,
    /// Default-off: R signatures legitimately carry ignored formals (an S3
    /// method must match its generic), so a project opts in explicitly.
    pub unused_parameter: LintLevel,
    /// Default-off: a package may `importFrom` a name for re-export or a side
    /// effect, and usage detection is a conservative token scan.
    pub unused_import: LintLevel,
    /// Default-off: rebinding a `base` name at top level is often deliberate
    /// (an S3 method like `print <- ...`, masking in a script), so a project
    /// opts in explicitly.
    pub shadows_builtin: LintLevel,
    /// Default-off: the same masking rationale for names from the non-`base`
    /// stub namespaces (`stats::filter`, `utils::head`), which resolve bare
    /// exactly like builtins.
    pub shadows_namespace: LintLevel,
}

/// Every lint finding of one file, in position order. Pure syntax walks plus
/// per-item naming output; cheap enough to run unmemoized per publication.
pub fn lint_file(db: &dyn Db, file: SourceFile, config: &LintConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let root = parse(db, file).syntax_node();
    syntax_lints(&root, config, &mut diagnostics);
    if let Some(severity) = opt_in_severity(config.unused_parameter) {
        let generics = crate::s3_generics(db, file);
        for span in item_spans(db, file) {
            let Some(naming) = item_naming(db, span.item) else {
                continue;
            };
            // An S3 method's formals are R's choice, not the author's: the
            // method must match its generic, so `format.myclass(x, ...)` that
            // ignores `x` is correct code and reporting it is noise. The
            // generic itself is the same case one step earlier. It declares
            // the argument it dispatches on and hands the call straight to
            // `UseMethod`, so *every* formal it names is unused by
            // construction.
            let exempt = span.item.name(db).as_deref().is_some_and(|name| {
                crate::is_s3_method_name(db, name, &generics) || generics.contains(name)
            });
            if exempt {
                continue;
            }
            for unused in &naming.unused_parameters {
                diagnostics.push(Diagnostic {
                    range: TextRange::new(
                        unused.range.start() + span.range.start(),
                        unused.range.end() + span.range.start(),
                    ),
                    severity: severity.clone(),
                    code: "unused-parameter",
                    message: format!("parameter `{}` is never used.", unused.name),
                    related: Vec::new(),
                });
            }
        }
    }
    shadow_lints(db, file, config, &mut diagnostics);
    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    diagnostics
}

/// A top-level binding whose name the stub corpus already resolves bare
/// wins over the stub for every read in the project: `base` names report
/// `shadows-builtin`, names declared by another stub namespace report
/// `shadows-namespace`. Both are off by default, because rebinding is often
/// deliberate, such as an S3 method or a script masking a name.
fn shadow_lints(
    db: &dyn Db,
    file: SourceFile,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let builtin_severity = opt_in_severity(config.shadows_builtin);
    let namespace_severity = opt_in_severity(config.shadows_namespace);
    if builtin_severity.is_none() && namespace_severity.is_none() {
        return;
    }
    let Some(library) = crate::stubs::stubs(db) else {
        return;
    };
    let is_builtin = |name: &str| {
        library
            .exports_by_namespace
            .get("base")
            .is_some_and(|exports| exports.contains(name))
    };
    for span in item_spans(db, file) {
        let Some(naming) = item_naming(db, span.item) else {
            continue;
        };
        for binding in naming.bindings.values() {
            if binding.kind != crate::naming::BindingKind::TopLevel {
                continue;
            }
            let range = TextRange::new(
                binding.range.start() + span.range.start(),
                binding.range.end() + span.range.start(),
            );
            if is_builtin(&binding.name) {
                if let Some(severity) = &builtin_severity {
                    diagnostics.push(Diagnostic {
                        range,
                        severity: severity.clone(),
                        code: "shadows-builtin",
                        message: format!("Top-level binding `{}` shadows a builtin.", binding.name),
                        related: Vec::new(),
                    });
                }
            } else if let Some(severity) = &namespace_severity
                && let Some(namespace) = crate::stubs::declaring_namespace(db, &binding.name)
            {
                diagnostics.push(Diagnostic {
                    range,
                    severity: severity.clone(),
                    code: "shadows-namespace",
                    message: format!(
                        "Top-level binding `{}` shadows `{namespace}::{}`.",
                        binding.name, binding.name
                    ),
                    related: Vec::new(),
                });
            }
        }
    }
}

fn syntax_lints(root: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    // Inherited state, maintained by an enter/leave depth counter: `#:`
    // annotation regions are type syntax, not R identifiers.
    let mut annotation_depth = 0usize;
    for event in root.preorder_with_tokens() {
        match event {
            rowan::WalkEvent::Enter(element) => match element {
                rowan::NodeOrToken::Node(node) => match node.kind() {
                    SyntaxKind::ANNOTATION => annotation_depth += 1,
                    SyntaxKind::BINARY_EXPR => {
                        binary_lints(&node, config, diagnostics);
                    }
                    SyntaxKind::ARGUMENT_LIST => {
                        let call = node
                            .parent()
                            .is_some_and(|parent| parent.kind() == SyntaxKind::CALL_EXPR);
                        if call {
                            trailing_comma_lint(&node, config, diagnostics);
                        }
                    }
                    SyntaxKind::PARAMETER => {
                        parameter_name_lint(&node, config, diagnostics);
                    }
                    _ => {}
                },
                rowan::NodeOrToken::Token(token) => {
                    if token.kind() == SyntaxKind::IDENT && annotation_depth == 0 {
                        let message = match token.text() {
                            "T" => Some("Use TRUE, not T, for Boolean values"),
                            "F" => Some("Use FALSE, not F, for Boolean values"),
                            _ => None,
                        };
                        if let Some(message) = message {
                            push_lint(
                                diagnostics,
                                config.boolean_shorthand,
                                Severity::Warning,
                                "boolean-shorthand",
                                token.text_range(),
                                message,
                            );
                        }
                    }
                }
            },
            rowan::WalkEvent::Leave(element) => {
                if let rowan::NodeOrToken::Node(node) = element
                    && node.kind() == SyntaxKind::ANNOTATION
                {
                    annotation_depth -= 1;
                }
            }
        }
    }
}

fn binary_lints(node: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    let Some(binary) = syntax::ast::BinaryExpr::cast(node.clone()) else {
        return;
    };
    let Some(operator) = binary.operator() else {
        return;
    };
    if operator.kind() == SyntaxKind::EQ {
        // The `=` token alone: the fix is that one character, and several
        // lints commonly fire on the same statement, so a whole-statement
        // range would stack three identical squiggles.
        push_lint(
            diagnostics,
            config.assignment_operator,
            Severity::Warning,
            "assignment-operator",
            operator.text_range(),
            "Use <-, not =, for assignment",
        );
    }
    if let Some(style) = config.naming_style
        && matches!(operator.kind(), SyntaxKind::LESS_MINUS | SyntaxKind::EQ)
        && let Some(target) = binary
            .lhs()
            .filter(|target| target.kind() == SyntaxKind::NAME)
    {
        let name = target.text().to_string();
        if let Some(message) = naming_style_message(&name, style, "Variable") {
            diagnostics.push(Diagnostic {
                range: expression_range(&target),
                severity: Severity::Warning,
                code: "naming-style",
                message,
                related: Vec::new(),
            });
        }
    }
}

fn trailing_comma_lint(
    argument_list: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut last_comma = None;
    for element in argument_list.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMA => {
                last_comma = Some(token.text_range());
            }
            rowan::NodeOrToken::Node(node) if node.kind() == SyntaxKind::ARGUMENT => {
                last_comma = None;
            }
            _ => {}
        }
    }
    if let Some(range) = last_comma {
        push_lint(
            diagnostics,
            config.trailing_comma,
            Severity::Error,
            "trailing-comma",
            range,
            "Unexpected comma after last argument",
        );
    }
}

fn parameter_name_lint(
    parameter: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(style) = config.naming_style else {
        return;
    };
    let Some(name_token) = parameter
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::IDENT)
    else {
        return;
    };
    if let Some(message) = naming_style_message(name_token.text(), style, "Parameter") {
        diagnostics.push(Diagnostic {
            range: name_token.text_range(),
            severity: Severity::Warning,
            code: "naming-style",
            message,
            related: Vec::new(),
        });
    }
}

/// The naming-style finding for one identifier, shared by the variable and
/// parameter sites: `None` when the name already matches the configured style.
fn naming_style_message(actual: &str, style: NameStyle, subject: &str) -> Option<String> {
    // `SCREAMING_SNAKE_CASE` is R's universal spelling for a constant, at any
    // scope, so it conforms under every configured style rather than being
    // rewritten to the style's default casing.
    if actual.chars().all(|character| {
        character.is_ascii_uppercase() || character == '_' || character.is_numeric()
    }) && actual
        .chars()
        .any(|character| character.is_ascii_uppercase())
    {
        return None;
    }
    let expected = match style {
        NameStyle::Camel => to_camel_case(actual),
        NameStyle::Snake => to_snake_case(actual),
    };
    if actual == expected {
        return None;
    }
    let style = match style {
        NameStyle::Camel => "camelCase",
        NameStyle::Snake => "snake_case",
    };
    Some(format!(
        "{subject} `{actual}` should have {style} name, e.g. {expected}"
    ))
}

fn push_lint(
    diagnostics: &mut Vec<Diagnostic>,
    level: LintLevel,
    default_severity: Severity,
    code: &'static str,
    range: TextRange,
    message: impl Into<String>,
) {
    let severity = match level {
        LintLevel::Off => return,
        LintLevel::Default => default_severity,
        LintLevel::Warn => Severity::Warning,
        LintLevel::Error => Severity::Error,
    };
    diagnostics.push(Diagnostic {
        range,
        severity,
        code,
        message: message.into(),
        related: Vec::new(),
    });
}

/// The opted-in severity of a default-off lint: `Default` behaves as `Off`.
fn opt_in_severity(level: LintLevel) -> Option<Severity> {
    match level {
        LintLevel::Default | LintLevel::Off => None,
        LintLevel::Warn => Some(Severity::Warning),
        LintLevel::Error => Some(Severity::Error),
    }
}

/// A node's range without the trailing trivia some expression nodes swallow
/// (a trailing comment or newline attaches inside).
fn expression_range(node: &SyntaxNode) -> TextRange {
    let mut end = node.text_range().end();
    let mut token = node.last_token();
    while let Some(current) = token {
        if current.kind().is_trivia() {
            end = current.text_range().start();
            token = current
                .prev_token()
                .filter(|previous| previous.text_range().start() >= node.text_range().start());
        } else {
            break;
        }
    }
    TextRange::new(node.text_range().start(), end)
}

fn to_camel_case(name: &str) -> String {
    let mut camel = String::new();
    let mut characters = name.chars();
    if let Some(first) = characters.next() {
        camel.push(first);
    }
    let mut uppercase_next = false;
    for character in characters {
        if character == '_' {
            uppercase_next = true;
            continue;
        }
        if uppercase_next {
            uppercase_next = false;
            camel.extend(character.to_uppercase());
        } else {
            camel.push(character);
        }
    }
    camel
}

fn to_snake_case(name: &str) -> String {
    let mut snake = String::new();
    let mut previous = '_';
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if index != 0 && previous != '_' {
                snake.push('_');
            }
            snake.extend(character.to_lowercase());
        } else {
            snake.push(character);
        }
        previous = character;
    }
    snake
}
