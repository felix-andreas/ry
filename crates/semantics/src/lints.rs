use {
    crate::{
        Db, SourceFile,
        diagnostics::{Applicability, Diagnostic, Severity},
        is_s3_method_name, item_naming, item_spans,
        naming::BindingKind,
        parse, s3_generics,
        stubs::{declaring_namespace, stubs},
    },
    rowan::{Direction, NodeOrToken},
    syntax::{
        GreenEvent, GreenRef, SyntaxKind, SyntaxNode, TextRange,
        ast::BinaryExpr,
        is_green_inline_annotation,
        native_values::{object_system_function, object_systems_disabled_message},
        node_at, walk_green,
    },
};

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
    /// `x == NA`, which is always `NA`.
    pub equals_na: LintLevel,
    /// `&` or `|` in an `if` or `while` condition.
    pub vector_logic: LintLevel,
    /// `1:length(x)` and its kin, which count down to 0 for an empty input.
    pub length_sequence: LintLevel,
    /// `class(x) == "a"`, which misses objects with several classes.
    pub class_equals: LintLevel,
    /// Off by default: dead code kept in comments is a style choice, not a bug.
    pub commented_code: LintLevel,
    /// Off by default: an explicit final `return()` is a style choice.
    pub redundant_return: LintLevel,
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
            .filter(|name| {
                syntax
                    .unless_redefined
                    .iter()
                    .any(|(function, _)| function == name)
            })
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

/// Memoized so that relinting after an edit elsewhere never walks this file's tree again. It
/// reads only syntax, so a cold check computes it in its first pass, before naming has run.
#[salsa::tracked(returns(ref))]
pub fn file_syntax_lints(db: &dyn Db, file: SourceFile, config: LintConfig) -> SyntaxLints {
    let mut lints = SyntaxLints::default();
    syntax_lints(&parse(db, file).syntax_node(), &config, &mut lints);
    lints
}

#[derive(Debug, Clone, PartialEq, Eq, Default, salsa::SalsaValue)]
pub struct SyntaxLints {
    always: Vec<Diagnostic>,
    /// Findings that hold only while the file does not bind the name they are keyed by: a bare
    /// call to a class-defining function (a file that binds `setClass` calls its own function),
    /// and `T` or `F` (a file that binds `T`, even as a parameter, may mean that binding, and
    /// the fix to `TRUE` would change what the code does).
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
    // One entry per open node, so a `Leave` knows whether its node opened a region.
    let mut opens_annotation = Vec::new();
    let mut annotation_depth = 0usize;
    // The walk reads the green tree and builds a red node only where a cheap green test says
    // a lint may fire, because a red walk of every file was a large share of the cold check.
    walk_green(root, |event| match event {
        GreenEvent::Enter {
            node,
            parent,
            range,
        } => {
            let kind = node.kind();
            let opens = kind == SyntaxKind::ANNOTATION
                || is_green_inline_annotation(kind, parent.map(GreenRef::kind));
            opens_annotation.push(opens);
            annotation_depth += usize::from(opens);
            if opens || !may_lint(node, parent, config) {
                return;
            }
            let Some(red) = node_at(root, kind, range) else {
                panic!("syntax_lints: no node of kind {kind:?} at {range:?}");
            };
            match kind {
                SyntaxKind::BINARY_EXPR => binary_lints(&red, config, diagnostics),
                SyntaxKind::ARGUMENT_LIST => trailing_comma_lint(&red, config, diagnostics),
                SyntaxKind::PARAMETER => parameter_name_lint(&red, config, diagnostics),
                SyntaxKind::NAME if annotation_depth == 0 => {
                    boolean_shorthand_lint(&red, config, &mut lints.unless_redefined);
                }
                SyntaxKind::CALL_EXPR => {
                    object_system_lint(&red, config, &mut lints.unless_redefined, diagnostics);
                }
                SyntaxKind::IF_EXPR | SyntaxKind::WHILE_EXPR => {
                    vector_logic_lint(&red, config, diagnostics);
                }
                SyntaxKind::FUNCTION_DEF => {
                    redundant_return_lint(&red, config, &mut lints.unless_redefined);
                }
                _ => {}
            }
        }
        GreenEvent::Leave(_) => {
            if opens_annotation.pop() == Some(true) {
                annotation_depth -= 1;
            }
        }
        GreenEvent::Token { kind, text, range } => {
            if kind == SyntaxKind::COMMENT && annotation_depth == 0 {
                commented_code_lint(text, range, config, diagnostics);
            }
        }
    });
}

/// `T` and `F` are flagged only where they are read as values: an argument name, an assignment
/// target, or a `$` or `@` field is a different name that happens to be spelled `T`, and
/// rewriting it to `TRUE` would not parse.
fn boolean_shorthand_lint(
    name: &SyntaxNode,
    config: &LintConfig,
    unless_redefined: &mut Vec<(String, Diagnostic)>,
) {
    let Some(token) = name.first_token() else {
        return;
    };
    if let Some(parent) = name.parent() {
        let is_first = parent.first_child().as_ref() == Some(name);
        let next_kind = name
            .siblings_with_tokens(Direction::Next)
            .skip(1)
            .find(|element| !element.kind().is_trivia())
            .map(|element| element.kind());
        let is_target = match parent.kind() {
            SyntaxKind::ARGUMENT => next_kind == Some(SyntaxKind::EQ),
            SyntaxKind::DOLLAR_EXPR | SyntaxKind::AT_EXPR => !is_first,
            SyntaxKind::BINARY_EXPR => match BinaryExpr::cast(parent.clone())
                .and_then(|binary| binary.operator())
                .map(|operator| operator.kind())
            {
                Some(
                    SyntaxKind::LESS_MINUS
                    | SyntaxKind::LESS2_MINUS
                    | SyntaxKind::EQ
                    | SyntaxKind::COLON_EQ,
                ) => is_first,
                Some(SyntaxKind::MINUS_GREATER | SyntaxKind::MINUS_GREATER2) => !is_first,
                _ => false,
            },
            _ => false,
        };
        if is_target {
            return;
        }
    }
    let (message, title, replacement) = match token.text() {
        "T" => (
            "Use TRUE, not T, for Boolean values",
            "Change to `TRUE`",
            "TRUE",
        ),
        "F" => (
            "Use FALSE, not F, for Boolean values",
            "Change to `FALSE`",
            "FALSE",
        ),
        _ => return,
    };
    let replacement = replacement.to_owned();
    let mut diagnostics = Vec::new();
    push_lint(
        &mut diagnostics,
        config.boolean_shorthand,
        Severity::Warning,
        "boolean-shorthand",
        token.text_range(),
        message,
        Some((Applicability::Safe, title, replacement)),
    );
    unless_redefined.extend(
        diagnostics
            .into_iter()
            .map(|diagnostic| (token.text().to_owned(), diagnostic)),
    );
}

/// A conservative green-tree test for whether the red lint for `node` can report anything:
/// `false` only when it certainly reports nothing.
fn may_lint(node: GreenRef<'_>, parent: Option<GreenRef<'_>>, config: &LintConfig) -> bool {
    match node.kind() {
        SyntaxKind::BINARY_EXPR => node.child_kinds().any(|kind| {
            matches!(
                kind,
                SyntaxKind::EQ | SyntaxKind::EQ2 | SyntaxKind::BANG_EQ | SyntaxKind::COLON
            ) || (config.naming_style.is_some() && kind == SyntaxKind::LESS_MINUS)
        }),
        SyntaxKind::IF_EXPR | SyntaxKind::WHILE_EXPR => {
            is_enabled(config.vector_logic, Some(Severity::Warning))
                && node.child_kinds().any(|kind| {
                    matches!(
                        kind,
                        SyntaxKind::BINARY_EXPR | SyntaxKind::UNARY_EXPR | SyntaxKind::PAREN_EXPR
                    )
                })
        }
        SyntaxKind::FUNCTION_DEF => is_enabled(config.redundant_return, None),
        SyntaxKind::ARGUMENT_LIST => {
            parent.is_some_and(|parent| parent.kind() == SyntaxKind::CALL_EXPR)
                && node
                    .child_kinds()
                    .fold(false, |comma_last, kind| match kind {
                        SyntaxKind::COMMA => true,
                        SyntaxKind::ARGUMENT => false,
                        _ => comma_last,
                    })
        }
        SyntaxKind::PARAMETER => config.naming_style.is_some(),
        SyntaxKind::NAME => matches!(node.single_token_text(), Some("T" | "F")),
        SyntaxKind::CALL_EXPR => match node.child_nodes().next() {
            Some(callee) if callee.kind() == SyntaxKind::NAME => callee
                .single_token_text()
                .is_none_or(|name| object_system_function(name).is_some()),
            Some(callee) => callee.kind() == SyntaxKind::NAMESPACE_EXPR,
            None => false,
        },
        _ => false,
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
            Some((Applicability::Safe, "Change to `<-`", "<-".to_owned())),
        );
    }
    if matches!(operator.kind(), SyntaxKind::EQ2 | SyntaxKind::BANG_EQ)
        && let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs())
    {
        comparison_lints(node, operator.kind(), &lhs, &rhs, config, diagnostics);
    }
    if operator.kind() == SyntaxKind::COLON
        && let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs())
    {
        length_sequence_lint(node, &lhs, &rhs, config, diagnostics);
    }
    if let Some(style) = config.naming_style
        && matches!(operator.kind(), SyntaxKind::LESS_MINUS | SyntaxKind::EQ)
        && let Some(target) = binary
            .lhs()
            .filter(|target| target.kind() == SyntaxKind::NAME)
    {
        let name = target.text().to_string();
        if let Some(message) = naming_style_message(&name, style, "Variable") {
            diagnostics.push(Diagnostic::new(
                significant_range(&target),
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
            object_systems_disabled_message(system, defines, &function),
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
            None,
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

fn trailing_comma_lint(
    argument_list: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // The comma, and the removal that also takes the blanks after it, so `g(1, )` becomes
    // `g(1)` rather than `g(1 )`.
    let mut last_comma: Option<(TextRange, TextRange)> = None;
    for element in argument_list.children_with_tokens() {
        match element {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::COMMA => {
                last_comma = Some((token.text_range(), token.text_range()));
            }
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::WHITESPACE => {
                if let Some((_, removal)) = &mut last_comma {
                    *removal = removal.cover(token.text_range());
                }
            }
            NodeOrToken::Node(node) if node.kind() == SyntaxKind::ARGUMENT => {
                last_comma = None;
            }
            _ => {}
        }
    }
    let Some((comma, removal)) = last_comma else {
        return;
    };
    // R reads the comma as an empty argument, which only fails when the callee evaluates it
    // (`c(1, )`); a closure that leaves it unforced runs fine, so the comma is a style finding,
    // not a certain run-time error.
    if let Some(severity) = lint_severity(config.trailing_comma, Some(Severity::Warning)) {
        diagnostics.push(
            Diagnostic::new(
                comma,
                severity,
                "trailing-comma",
                "Unexpected comma after last argument".to_owned(),
            )
            .with_fix(
                Applicability::Safe,
                "Remove the trailing comma".to_owned(),
                removal,
                String::new(),
            ),
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

/// `x == NA` and `class(x) == "a"`, with `!=` alike. Each fix swaps the comparison for the test
/// it was meant to be, so it changes what the code computes and is unsafe.
fn comparison_lints(
    comparison: &SyntaxNode,
    operator: SyntaxKind,
    lhs: &SyntaxNode,
    rhs: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let negation = if operator == SyntaxKind::BANG_EQ {
        "!"
    } else {
        ""
    };
    let range = significant_range(comparison);
    let other = if is_na_literal(rhs) {
        Some(lhs)
    } else if is_na_literal(lhs) {
        Some(rhs)
    } else {
        None
    };
    if let Some(other) = other {
        push_lint(
            diagnostics,
            config.equals_na,
            Severity::Warning,
            "equals-na",
            range,
            "Comparing with NA always gives NA, whatever the other side holds. Use is.na() to test for missing values",
            Some((
                Applicability::Unsafe,
                "Change to `is.na()`",
                format!("{negation}is.na({})", significant_text(other)),
            )),
        );
        return;
    }
    let class_call = |node: &SyntaxNode, other: &SyntaxNode| {
        is_string_literal(other)
            .then(|| single_argument_of(node, &["class"]))
            .flatten()
            .map(|(_, argument)| (argument, significant_text(other)))
    };
    if let Some((argument, class)) = class_call(lhs, rhs).or_else(|| class_call(rhs, lhs)) {
        push_lint(
            diagnostics,
            config.class_equals,
            Severity::Warning,
            "class-equals",
            range,
            "Comparing class() with a string misses objects that have several classes. Use inherits()",
            Some((
                Applicability::Unsafe,
                "Change to `inherits()`",
                format!(
                    "{negation}inherits({}, {class})",
                    significant_text(&argument)
                ),
            )),
        );
    }
}

/// `1:length(x)` is `c(1, 0)` for an empty `x`, so a loop over it runs twice instead of never.
/// The fix makes the empty case run zero times, which is what the loop meant but not what it
/// did, so it is unsafe.
fn length_sequence_lint(
    sequence: &SyntaxNode,
    lhs: &SyntaxNode,
    rhs: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let starts_at_one = lhs.kind() == SyntaxKind::LITERAL
        && lhs
            .first_token()
            .is_some_and(|token| matches!(token.text(), "1" | "1L"));
    if !starts_at_one {
        return;
    }
    let Some((function, argument)) =
        single_argument_of(rhs, &["length", "nrow", "ncol", "NROW", "NCOL"])
    else {
        return;
    };
    let argument = significant_text(&argument);
    let (message, title, replacement) = if function == "length" {
        (
            "`1:length(...)` is `c(1, 0)` when the vector is empty. Use seq_along()".to_owned(),
            "Change to `seq_along()`",
            format!("seq_along({argument})"),
        )
    } else {
        (
            format!("`1:{function}(...)` is `c(1, 0)` when the count is zero. Use seq_len()"),
            "Change to `seq_len()`",
            format!("seq_len({function}({argument}))"),
        )
    };
    push_lint(
        diagnostics,
        config.length_sequence,
        Severity::Warning,
        "length-sequence",
        significant_range(sequence),
        message,
        Some((Applicability::Unsafe, title, replacement)),
    );
}

/// `&` and `|` work element by element and always evaluate both sides; a condition wants one
/// value, which `&&` and `||` insist on. Only operators that decide the condition itself count,
/// so `if (any(x & y))` is fine. The fix is unsafe because `&&` skips evaluating its right side.
fn vector_logic_lint(node: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    let Some(condition) = node.first_child() else {
        return;
    };
    let mut pending = vec![condition];
    while let Some(expression) = pending.pop() {
        match expression.kind() {
            SyntaxKind::PAREN_EXPR | SyntaxKind::UNARY_EXPR => {
                pending.extend(expression.children());
            }
            SyntaxKind::BINARY_EXPR => {
                let Some(binary) = BinaryExpr::cast(expression.clone()) else {
                    continue;
                };
                let Some(operator) = binary.operator() else {
                    continue;
                };
                let scalar = match operator.kind() {
                    SyntaxKind::AMP => "&&",
                    SyntaxKind::PIPE => "||",
                    SyntaxKind::AMP2 | SyntaxKind::PIPE2 => "",
                    _ => continue,
                };
                pending.extend(binary.lhs());
                pending.extend(binary.rhs());
                if scalar.is_empty() {
                    continue;
                }
                let keyword = if node.kind() == SyntaxKind::IF_EXPR {
                    "an `if`"
                } else {
                    "a `while`"
                };
                push_lint(
                    diagnostics,
                    config.vector_logic,
                    Severity::Warning,
                    "vector-logic",
                    operator.text_range(),
                    format!(
                        "Use {scalar}, not {}, in {keyword} condition: {} works element by element and always evaluates both sides",
                        operator.text(),
                        operator.text()
                    ),
                    Some((
                        Applicability::Unsafe,
                        &format!("Change to `{scalar}`"),
                        scalar.to_owned(),
                    )),
                );
            }
            _ => {}
        }
    }
}

/// A `return(x)` whose value the function would return anyway: the last expression of the body,
/// or of a branch of an `if` in that place. Keyed on `return` because a file that binds its own
/// `return` may mean that function.
fn redundant_return_lint(
    function: &SyntaxNode,
    config: &LintConfig,
    unless_redefined: &mut Vec<(String, Diagnostic)>,
) {
    let Some(body) = function
        .children()
        .filter(|child| {
            !matches!(
                child.kind(),
                SyntaxKind::PARAMETER_LIST | SyntaxKind::RETURN_TYPE | SyntaxKind::PURE_MARKER
            )
        })
        .last()
    else {
        return;
    };
    let mut diagnostics = Vec::new();
    let mut pending = vec![body];
    while let Some(tail) = pending.pop() {
        match tail.kind() {
            SyntaxKind::BRACE_EXPR => pending.extend(tail.children().last()),
            SyntaxKind::IF_EXPR => pending.extend(tail.children().skip(1)),
            SyntaxKind::CALL_EXPR => {
                if let Some(("return", value)) = single_argument_of(&tail, &["return"]) {
                    push_lint(
                        &mut diagnostics,
                        config.redundant_return,
                        Severity::Warning,
                        "redundant-return",
                        significant_range(&tail),
                        "This return() is redundant: a function returns the value of its last expression",
                        Some((
                            Applicability::Safe,
                            "Remove `return()`",
                            significant_text(&value),
                        )),
                    );
                }
            }
            _ => {}
        }
    }
    unless_redefined.extend(
        diagnostics
            .into_iter()
            .map(|diagnostic| ("return".to_owned(), diagnostic)),
    );
}

/// A comment whose text parses as R and does something (calls, assigns, defines, branches,
/// loops or indexes), which prose almost never does. Roxygen (`#'`), plumber (`#*`), Quarto
/// (`#|`), shebang (`#!`) and printed output (`#>`) comments are other tools' syntax.
fn commented_code_lint(
    text: &str,
    range: TextRange,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !is_enabled(config.commented_code, None) {
        return;
    }
    let after_hashes = text.trim_start_matches('#');
    if after_hashes.starts_with(['\'', '*', '|', '!', '>']) {
        return;
    }
    let code = after_hashes.trim();
    // Suppression and formatter directives look like calls (`ry: allow(...)`).
    if code.is_empty()
        || ["ry:", "fmt:", "nolint"]
            .iter()
            .any(|directive| code.starts_with(directive))
    {
        return;
    }
    let parsed = syntax::parse(code);
    if !parsed.errors().is_empty() {
        return;
    }
    let does_something = parsed
        .syntax_node()
        .descendants()
        .any(|node| match node.kind() {
            // Prose such as `Returns (invisibly)` parses as a call, but code rarely puts a
            // space between a function and its parenthesis.
            SyntaxKind::CALL_EXPR => node
                .children()
                .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
                .and_then(|arguments| arguments.prev_sibling_or_token())
                .is_none_or(|before| before.kind() != SyntaxKind::WHITESPACE),
            SyntaxKind::FUNCTION_DEF
            | SyntaxKind::IF_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::SUBSET_EXPR
            | SyntaxKind::SUBSET2_EXPR
            | SyntaxKind::DOLLAR_EXPR => true,
            SyntaxKind::BINARY_EXPR => BinaryExpr::cast(node)
                .and_then(|binary| binary.operator())
                .is_some_and(|operator| {
                    matches!(
                        operator.kind(),
                        SyntaxKind::LESS_MINUS
                            | SyntaxKind::LESS2_MINUS
                            | SyntaxKind::MINUS_GREATER
                            | SyntaxKind::MINUS_GREATER2
                    )
                }),
            _ => false,
        });
    if does_something {
        push_lint(
            diagnostics,
            config.commented_code,
            Severity::Warning,
            "commented-code",
            range,
            "This comment is code. Delete it, since version control keeps the history",
            None,
        );
    }
}

fn is_na_literal(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::LITERAL
        && node.first_token().is_some_and(|token| {
            matches!(
                token.kind(),
                SyntaxKind::NA_KW
                    | SyntaxKind::NA_INTEGER_KW
                    | SyntaxKind::NA_REAL_KW
                    | SyntaxKind::NA_COMPLEX_KW
                    | SyntaxKind::NA_CHARACTER_KW
            )
        })
}

fn is_string_literal(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::LITERAL
        && node.first_token().is_some_and(|token| {
            matches!(token.kind(), SyntaxKind::STRING | SyntaxKind::RAW_STRING)
        })
}

/// The callee (one of `names`, called bare) and the value of a call with exactly one argument,
/// passed by position: `length(x)` but not `length(x = x)` or `base::length(x)`.
fn single_argument_of(
    call: &SyntaxNode,
    names: &[&'static str],
) -> Option<(&'static str, SyntaxNode)> {
    if call.kind() != SyntaxKind::CALL_EXPR {
        return None;
    }
    let mut children = call.children();
    let callee = children
        .next()
        .filter(|callee| callee.kind() == SyntaxKind::NAME)?;
    let callee_text = callee.text();
    let name = names.iter().copied().find(|name| callee_text == *name)?;
    let arguments = children.find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)?;
    let mut argument_nodes = arguments
        .children()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT);
    let argument = argument_nodes.next()?;
    if argument_nodes.next().is_some()
        || arguments
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::COMMA)
        || argument
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::EQ)
    {
        return None;
    }
    let value = argument.first_child()?;
    Some((name, value))
}

/// The node's range without the leading or trailing trivia (a comment or newline) some
/// expression nodes swallow.
fn significant_range(node: &SyntaxNode) -> TextRange {
    let significant = |token: &syntax::SyntaxToken| !token.kind().is_trivia();
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token());
    let start = tokens
        .find(significant)
        .map_or(node.text_range().start(), |token| {
            token.text_range().start()
        });
    let end = std::iter::successors(node.last_token(), |token| {
        token
            .prev_token()
            .filter(|previous| previous.text_range().start() >= node.text_range().start())
    })
    .find(significant)
    .map_or(node.text_range().end(), |token| token.text_range().end());
    TextRange::new(start, end.max(start))
}

fn significant_text(node: &SyntaxNode) -> String {
    let range = significant_range(node) - node.text_range().start();
    node.text().to_string()[range].to_owned()
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

/// `fix` is the fix's applicability, its title, and the text that replaces the whole reported
/// range.
fn push_lint(
    diagnostics: &mut Vec<Diagnostic>,
    level: LintLevel,
    default_severity: Severity,
    code: &'static str,
    range: TextRange,
    message: impl Into<String>,
    fix: Option<(Applicability, &str, String)>,
) {
    if let Some(severity) = lint_severity(level, Some(default_severity)) {
        let diagnostic = Diagnostic::new(range, severity, code, message.into());
        diagnostics.push(match fix {
            Some((applicability, title, replacement)) => {
                diagnostic.with_fix(applicability, title.to_owned(), range, replacement)
            }
            None => diagnostic,
        });
    }
}

fn is_enabled(level: LintLevel, default: Option<Severity>) -> bool {
    lint_severity(level, default).is_some()
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
