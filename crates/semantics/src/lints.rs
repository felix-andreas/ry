use crate::diagnostics::{Diagnostic, Severity};
use crate::naming::BindingKind;
use crate::stubs::{declaring_namespace, stubs};
use crate::{Db, SourceFile, is_s3_method_name, item_naming, item_spans, parse, s3_generics};
use rowan::{NodeOrToken, WalkEvent};
use syntax::ast::BinaryExpr;
use syntax::{SyntaxKind, SyntaxNode, TextRange, is_inline_annotation};

/// `Default` keeps the lint's built-in severity, which is off for opt-in lints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LintLevel {
    #[default]
    Default,
    Off,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
pub enum NameStyle {
    #[serde(alias = "camelCase")]
    Camel,
    #[serde(alias = "snake_case")]
    Snake,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct LintConfig {
    pub naming_style: Option<NameStyle>,
    pub assignment_operator: LintLevel,
    pub boolean_shorthand: LintLevel,
    /// Accepted so existing configs keep loading; never read (a missing comma is a parse error).
    pub missing_comma: LintLevel,
    pub trailing_comma: LintLevel,
    /// Off by default: an S3 method's formals must match its generic, so some go unused.
    pub unused_parameter: LintLevel,
    /// Off by default: an import may exist for re-export or a side effect.
    pub unused_import: LintLevel,
    /// Off by default: rebinding a `base` name is often deliberate (an S3 method, script masking).
    pub shadows_builtin: LintLevel,
    pub shadows_namespace: LintLevel,
    /// Warns by default: S4, R6 and Reference Classes still run, but ry recommends
    /// records and tagged unions, which it checks statically.
    pub object_system: LintLevel,
    /// Copied from `[check] object-systems = false` when the config loads, which turns
    /// every definition into an error whatever `object_system` says.
    #[serde(skip)]
    pub object_systems_disabled: bool,
}

/// Sorted by position.
pub fn lint_file(db: &dyn Db, file: SourceFile, config: &LintConfig) -> Vec<Diagnostic> {
    let syntax = file_syntax_lints(db, file, *config);
    let mut diagnostics = syntax.always.clone();
    if !syntax.unless_redefined.is_empty() {
        let redefined: Vec<&str> = item_spans(db, file)
            .iter()
            .filter_map(|span| item_naming(db, span.item).as_ref())
            .flat_map(|naming| naming.bindings.values())
            .map(|binding| binding.name.as_str())
            .filter(|name| object_system_function(name).is_some())
            .collect();
        diagnostics.extend(
            syntax
                .unless_redefined
                .iter()
                .filter(|(function, _)| !redefined.contains(&function.as_str()))
                .map(|(_, diagnostic)| diagnostic.clone()),
        );
    }
    if let Some(severity) = lint_severity(config.unused_parameter, None) {
        let generics = s3_generics(db, file);
        for span in item_spans(db, file) {
            let Some(naming) = item_naming(db, span.item) else {
                continue;
            };
            // An S3 method's formals must match its generic, and a generic only forwards its
            // formals to `UseMethod`, so unused formals in either are correct code.
            let exempt = span.item.name(db).as_deref().is_some_and(|name| {
                is_s3_method_name(db, name, &generics) || generics.contains(name)
            });
            if exempt {
                continue;
            }
            for unused in &naming.unused_parameters {
                diagnostics.push(Diagnostic::new(
                    unused.range + span.range.start(),
                    severity,
                    "unused-parameter",
                    format!("parameter `{}` is never used.", unused.name),
                ));
            }
        }
    }
    shadow_lints(db, file, config, &mut diagnostics);
    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start(), diagnostic.range.end()));
    diagnostics
}

/// Memoized so that revalidating an untouched file never needs its tree, which may have
/// been evicted. It reads only syntax, so a cold check can compute it while the tree is
/// still in memory, before naming has run.
#[salsa::tracked(returns(ref))]
pub fn file_syntax_lints(db: &dyn Db, file: SourceFile, config: LintConfig) -> SyntaxLints {
    let mut lints = SyntaxLints::default();
    syntax_lints(&parse(db, file).syntax_node(), &config, &mut lints);
    lints
}

#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct SyntaxLints {
    always: Vec<Diagnostic>,
    /// Findings on a bare call to a class-defining function, keyed by its name. A file that
    /// binds `setClass` itself calls its own function, not the methods one, so these hold
    /// only when the file does not redefine the name.
    unless_redefined: Vec<(String, Diagnostic)>,
}

/// A top-level binding wins over a same-named stub export for every bare read in the project.
fn shadow_lints(
    db: &dyn Db,
    file: SourceFile,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let builtin_severity = lint_severity(config.shadows_builtin, None);
    let namespace_severity = lint_severity(config.shadows_namespace, None);
    if builtin_severity.is_none() && namespace_severity.is_none() {
        return;
    }
    let Some(library) = stubs(db) else {
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
            if binding.kind != BindingKind::TopLevel {
                continue;
            }
            let range = binding.range + span.range.start();
            if is_builtin(&binding.name) {
                if let Some(severity) = &builtin_severity {
                    diagnostics.push(Diagnostic::new(
                        range,
                        *severity,
                        "shadows-builtin",
                        format!("Top-level binding `{}` shadows a builtin.", binding.name),
                    ));
                }
            } else if let Some(severity) = &namespace_severity
                && let Some(namespace) = declaring_namespace(db, &binding.name)
            {
                diagnostics.push(Diagnostic::new(
                    range,
                    *severity,
                    "shadows-namespace",
                    format!(
                        "Top-level binding `{}` shadows `{namespace}::{}`.",
                        binding.name, binding.name
                    ),
                ));
            }
        }
    }
}

fn syntax_lints(root: &SyntaxNode, config: &LintConfig, lints: &mut SyntaxLints) {
    let diagnostics = &mut lints.always;
    // `#:` annotation regions and inline annotations are type syntax, not R identifiers.
    let mut annotation_depth = 0usize;
    for event in root.preorder_with_tokens() {
        match event {
            WalkEvent::Enter(NodeOrToken::Node(node)) => match node.kind() {
                SyntaxKind::ANNOTATION => annotation_depth += 1,
                _ if is_inline_annotation(&node) => annotation_depth += 1,
                SyntaxKind::BINARY_EXPR => binary_lints(&node, config, diagnostics),
                SyntaxKind::ARGUMENT_LIST
                    if node
                        .parent()
                        .is_some_and(|parent| parent.kind() == SyntaxKind::CALL_EXPR) =>
                {
                    trailing_comma_lint(&node, config, diagnostics);
                }
                SyntaxKind::PARAMETER => parameter_name_lint(&node, config, diagnostics),
                SyntaxKind::CALL_EXPR => {
                    object_system_lint(&node, config, &mut lints.unless_redefined, diagnostics);
                }
                _ => {}
            },
            WalkEvent::Enter(NodeOrToken::Token(token))
                if token.kind() == SyntaxKind::IDENT && annotation_depth == 0 =>
            {
                let message = match token.text() {
                    "T" => "Use TRUE, not T, for Boolean values",
                    "F" => "Use FALSE, not F, for Boolean values",
                    _ => continue,
                };
                push_lint(
                    diagnostics,
                    config.boolean_shorthand,
                    Severity::Warning,
                    "boolean-shorthand",
                    token.text_range(),
                    message,
                );
            }
            WalkEvent::Leave(NodeOrToken::Node(node))
                if node.kind() == SyntaxKind::ANNOTATION || is_inline_annotation(&node) =>
            {
                annotation_depth -= 1;
            }
            _ => {}
        }
    }
}

fn binary_lints(node: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    let Some(binary) = BinaryExpr::cast(node.clone()) else {
        return;
    };
    let Some(operator) = binary.operator() else {
        return;
    };
    if operator.kind() == SyntaxKind::EQ {
        // Only the `=` token: several lints often fire on one statement, and whole-statement
        // ranges would stack identical squiggles.
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
            // Excludes the trailing trivia (a comment or newline) some expression nodes swallow.
            let start = target.text_range().start();
            let mut end = target.text_range().end();
            let mut token = target.last_token();
            while let Some(trivia) = token.filter(|token| token.kind().is_trivia()) {
                end = trivia.text_range().start();
                token = trivia
                    .prev_token()
                    .filter(|previous| previous.text_range().start() >= start);
            }
            diagnostics.push(Diagnostic::new(
                TextRange::new(start, end),
                Severity::Warning,
                "naming-style",
                message,
            ));
        }
    }
}

/// Only the definitions: a class has to be defined before anything can use it, so they
/// mark every place the object system enters the code without flagging each `new()`.
fn object_system_lint(
    call: &SyntaxNode,
    config: &LintConfig,
    unless_redefined: &mut Vec<(String, Diagnostic)>,
    always: &mut Vec<Diagnostic>,
) {
    let Some(callee) = call.first_child() else {
        return;
    };
    let (package, name) = match callee.kind() {
        SyntaxKind::NAME => (None, callee),
        SyntaxKind::NAMESPACE_EXPR => {
            let mut names = callee
                .children()
                .filter(|child| child.kind() == SyntaxKind::NAME);
            let (Some(package), Some(name)) = (names.next(), names.next()) else {
                return;
            };
            (Some(package.text().to_string()), name)
        }
        _ => return,
    };
    let function = name.text().to_string();
    let Some((system, defines, home)) = object_system_function(&function) else {
        return;
    };
    if package.as_deref().is_some_and(|package| package != home) {
        return;
    }
    let mut found = Vec::new();
    let range = name.text_range();
    if config.object_systems_disabled {
        found.push(Diagnostic::new(
            range,
            Severity::Error,
            "object-system",
            format!(
                "{system} are turned off in this project (`[check] object-systems = false`), so `{function}` cannot define {defines}. Use a record or a tagged union instead"
            ),
        ));
    } else {
        push_lint(
            &mut found,
            config.object_system,
            Severity::Warning,
            "object-system",
            range,
            format!(
                "`{function}` defines {defines}. ry recommends a record or a tagged union instead"
            ),
        );
    }
    match package {
        Some(_) => always.extend(found),
        None => unless_redefined.extend(
            found
                .into_iter()
                .map(|diagnostic| (function.clone(), diagnostic)),
        ),
    }
}

/// The object system a class-defining function belongs to, what it defines, and its home
/// package.
fn object_system_function(function: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match function {
        "setClass" => ("S4 classes", "an S4 class", "methods"),
        "setClassUnion" => ("S4 classes", "an S4 class union", "methods"),
        "setGeneric" => ("S4 classes", "an S4 generic", "methods"),
        "setMethod" => ("S4 classes", "an S4 method", "methods"),
        "setValidity" => ("S4 classes", "an S4 validity check", "methods"),
        "setRefClass" => ("Reference Classes", "a Reference Class", "methods"),
        "R6Class" => ("R6 classes", "an R6 class", "R6"),
        _ => return None,
    })
}

fn trailing_comma_lint(
    argument_list: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut last_comma = None;
    for element in argument_list.children_with_tokens() {
        match element {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMA => {
                last_comma = Some(token.text_range());
            }
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::ARGUMENT => {
                last_comma = None;
            }
            _ => {}
        }
    }
    if let Some(range) = last_comma {
        push_lint(
            diagnostics,
            config.trailing_comma,
            // R reads the comma as an empty argument, which only fails when the callee
            // evaluates it (`c(1, )`); a closure that leaves it unforced runs fine, so the
            // comma is a style finding, not a certain run-time error.
            Severity::Warning,
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
        diagnostics.push(Diagnostic::new(
            name_token.text_range(),
            Severity::Warning,
            "naming-style",
            message,
        ));
    }
}

fn naming_style_message(actual: &str, style: NameStyle, subject: &str) -> Option<String> {
    // SCREAMING_SNAKE_CASE is R's conventional constant spelling, so it conforms under every style.
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
    if let Some(severity) = lint_severity(level, Some(default_severity)) {
        diagnostics.push(Diagnostic::new(range, severity, code, message.into()));
    }
}

/// An opt-in lint has no default severity, so `Default` leaves it off.
pub fn lint_severity(level: LintLevel, default: Option<Severity>) -> Option<Severity> {
    match level {
        LintLevel::Default => default,
        LintLevel::Off => None,
        LintLevel::Warn => Some(Severity::Warning),
        LintLevel::Error => Some(Severity::Error),
    }
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
