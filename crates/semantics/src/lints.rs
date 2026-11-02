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
    /// Off by default: the same argument name twice in one call. A function that collects
    /// `...` keeps both values on purpose (cli bullets in `c()`, repeated HTML `class`
    /// attributes), and a syntax lint cannot tell such a callee from one with a matching formal.
    pub duplicate_argument: LintLevel,
    /// Statements after `return()`, `stop()`, `break` or `next` in the same block.
    pub unreachable_code: LintLevel,
    /// A literal `sprintf()` format whose conversions do not match the values passed.
    pub sprintf_arguments: LintLevel,
    /// Off by default: `any(is.na(x))`, which `anyNA(x)` answers without a copy.
    pub any_is_na: LintLevel,
    /// Off by default: `paste(..., sep = "")`, which is `paste0(...)`.
    pub paste_sep: LintLevel,
    /// Off by default: `sapply(x, length)`, which is `lengths(x)`.
    pub lengths: LintLevel,
    /// Off by default: `as.integer(1)` and kin, which a literal spells directly.
    pub literal_coercion: LintLevel,
    /// Off by default: `x == TRUE` and kin.
    pub redundant_equals: LintLevel,
    /// Off by default: a string in single quotes.
    pub single_quotes: LintLevel,
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
                    call_lints(&red, config, diagnostics);
                }
                SyntaxKind::BRACE_EXPR => {
                    unreachable_code_lint(&red, config, &mut lints.unless_redefined);
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
            if kind == SyntaxKind::STRING && annotation_depth == 0 {
                single_quotes_lint(text, range, config, diagnostics);
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
        SyntaxKind::CALL_EXPR => {
            let mut children = node.child_nodes();
            let callee_lints = match children.next() {
                Some(callee) if callee.kind() == SyntaxKind::NAME => {
                    callee.single_token_text().is_none_or(|name| {
                        object_system_function(name).is_some() || CALL_LINT_NAMES.contains(&name)
                    })
                }
                Some(callee) => callee.kind() == SyntaxKind::NAMESPACE_EXPR,
                None => false,
            };
            callee_lints
                || (is_enabled(config.duplicate_argument, None)
                    && children
                        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
                        .is_some_and(|arguments| {
                            arguments
                                .child_nodes()
                                .filter(|argument| {
                                    argument.child_kinds().any(|kind| kind == SyntaxKind::EQ)
                                })
                                .nth(1)
                                .is_some()
                        }))
        }
        SyntaxKind::BRACE_EXPR => {
            is_enabled(config.unreachable_code, Some(Severity::Warning)) && {
                let mut statements = node.child_nodes();
                statements.by_ref().any(|statement| match statement.kind() {
                    SyntaxKind::BREAK_EXPR | SyntaxKind::NEXT_EXPR => true,
                    SyntaxKind::CALL_EXPR => statement.child_nodes().next().is_some_and(|callee| {
                        matches!(callee.single_token_text(), Some("return" | "stop"))
                    }),
                    _ => false,
                }) && statements.next().is_some()
            }
        }
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
        redundant_equals_lint(node, operator.kind(), &lhs, &rhs, config, diagnostics);
    }
    if operator.kind() == SyntaxKind::COLON
        && let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs())
        && lhs.kind() == SyntaxKind::LITERAL
        && lhs
            .first_token()
            .is_some_and(|token| matches!(token.text(), "1" | "1L"))
    {
        length_sequence_lint(node, &rhs, false, config, diagnostics);
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
/// `1:n` or `seq(n)` where `n` is a length or a count, written as `seq(...)` when `seq_call`.
fn length_sequence_lint(
    sequence: &SyntaxNode,
    count: &SyntaxNode,
    seq_call: bool,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((function, argument)) =
        single_argument_of(count, &["length", "nrow", "ncol", "NROW", "NCOL"])
    else {
        return;
    };
    let argument = significant_text(&argument);
    let written = if seq_call {
        format!("seq({function}(...))")
    } else {
        format!("1:{function}(...)")
    };
    let (message, title, replacement) = if function == "length" {
        (
            format!("`{written}` is `c(1, 0)` when the vector is empty. Use seq_along()"),
            "Change to `seq_along()`",
            format!("seq_along({argument})"),
        )
    } else {
        (
            format!("`{written}` is `c(1, 0)` when the count is zero. Use seq_len()"),
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

/// Bare callees some call lint looks at, so the green pre-check can skip every other call.
const CALL_LINT_NAMES: &[&str] = &[
    "seq",
    "sprintf",
    "gettextf",
    "any",
    "paste",
    "sapply",
    "vapply",
    "as.integer",
    "as.numeric",
    "as.double",
    "as.logical",
    "as.character",
];

/// Functions that evaluate their arguments in order, so naming a column twice is how they
/// update it (`mutate(x = x + 1, x = x * 2)`).
const SEQUENTIAL_ARGUMENT_FUNCTIONS: &[&str] =
    &["mutate", "transmute", "summarise", "summarize", "reframe"];

/// An argument of a call: its name without quotes or backticks, and its value.
struct CallArgument {
    node: SyntaxNode,
    name: Option<(String, TextRange)>,
    value: Option<SyntaxNode>,
}

fn call_arguments(call: &SyntaxNode) -> Vec<CallArgument> {
    let Some(list) = call
        .children()
        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
    else {
        return Vec::new();
    };
    list.children()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
        .map(|node| {
            let named = node
                .children_with_tokens()
                .any(|element| element.kind() == SyntaxKind::EQ);
            let mut children = node.children();
            let name = if named {
                children.next().map(|name| {
                    let text = name.text().to_string();
                    let unquoted = text
                        .strip_prefix(['`', '"', '\''])
                        .and_then(|inner| inner.strip_suffix(['`', '"', '\'']))
                        .unwrap_or(&text)
                        .to_owned();
                    (unquoted, significant_range(&name))
                })
            } else {
                None
            };
            let value = children.next();
            CallArgument { node, name, value }
        })
        .collect()
}

/// The name of a bare callee, such as `sprintf` in `sprintf(...)` but not in `base::sprintf(...)`.
fn bare_callee(call: &SyntaxNode) -> Option<String> {
    call.first_child()
        .filter(|callee| callee.kind() == SyntaxKind::NAME)
        .map(|callee| callee.text().to_string())
}

fn call_lints(call: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    let callee = bare_callee(call);
    let arguments = call_arguments(call);
    duplicate_argument_lint(callee.as_deref(), &arguments, config, diagnostics);
    let Some(callee) = callee else {
        return;
    };
    let positional: Vec<&SyntaxNode> = arguments
        .iter()
        .filter(|argument| argument.name.is_none())
        .filter_map(|argument| argument.value.as_ref())
        .collect();
    let all_positional = positional.len() == arguments.len();
    match callee.as_str() {
        "seq" if all_positional && positional.len() == 1 => {
            length_sequence_lint(call, positional[0], true, config, diagnostics);
        }
        "sprintf" | "gettextf" => sprintf_arguments_lint(&callee, &arguments, config, diagnostics),
        "any" if all_positional && positional.len() == 1 => {
            if let Some(("is.na", value)) = single_argument_of(positional[0], &["is.na"]) {
                push_lint(
                    diagnostics,
                    opt_in(config.any_is_na),
                    Severity::Warning,
                    "any-is-na",
                    significant_range(call),
                    "Use anyNA(x), which stops at the first NA instead of building a vector for any()",
                    Some((
                        Applicability::Safe,
                        "Change to `anyNA()`",
                        format!("anyNA({})", significant_text(&value)),
                    )),
                );
            }
        }
        "paste" => paste_sep_lint(call, &arguments, config, diagnostics),
        "sapply" | "vapply" => {
            let expected = if callee == "sapply" { 2 } else { 3 };
            let counts_lengths = all_positional
                && positional.len() == expected
                && positional[1].kind() == SyntaxKind::NAME
                && positional[1].text() == "length";
            if counts_lengths {
                push_lint(
                    diagnostics,
                    opt_in(config.lengths),
                    Severity::Warning,
                    "lengths",
                    significant_range(call),
                    format!(
                        "Use lengths(x), which counts every element's length without calling length() through {callee}()"
                    ),
                    Some((
                        Applicability::Unsafe,
                        "Change to `lengths()`",
                        format!("lengths({})", significant_text(positional[0])),
                    )),
                );
            }
        }
        "as.integer" | "as.numeric" | "as.double" | "as.logical" | "as.character"
            if all_positional && positional.len() == 1 =>
        {
            literal_coercion_lint(call, &callee, positional[0], config, diagnostics);
        }
        _ => {}
    }
}

/// An opt-in lint has no default severity, so `Default` leaves it off.
fn opt_in(level: LintLevel) -> LintLevel {
    match level {
        LintLevel::Default => LintLevel::Off,
        level => level,
    }
}

/// A closure stops with "formal argument matched by multiple actual arguments", and `c()` or
/// `list()` keeps both values under one name, which code that reads it by name then misses.
fn duplicate_argument_lint(
    callee: Option<&str>,
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if callee.is_some_and(|callee| SEQUENTIAL_ARGUMENT_FUNCTIONS.contains(&callee)) {
        return;
    }
    let mut seen: Vec<&str> = Vec::new();
    for (name, range) in arguments
        .iter()
        .filter_map(|argument| argument.name.as_ref())
    {
        if name.is_empty() {
            continue;
        }
        if seen.contains(&name.as_str()) {
            push_lint(
                diagnostics,
                opt_in(config.duplicate_argument),
                Severity::Warning,
                "duplicate-argument",
                *range,
                format!(
                    "`{name}` is passed twice in this call. A function stops with \"matched by multiple actual arguments\", and c() or list() keeps both values under one name"
                ),
                None,
            );
        } else {
            seen.push(name);
        }
    }
}

/// Statements after `return()`, `stop()`, `break` or `next` in the same block never run.
/// `return` and `stop` count only while the file does not define them.
fn unreachable_code_lint(
    block: &SyntaxNode,
    config: &LintConfig,
    unless_redefined: &mut Vec<(String, Diagnostic)>,
) {
    let statements: Vec<SyntaxNode> = block.children().collect();
    let Some((index, exit)) = statements
        .iter()
        .enumerate()
        .find_map(|(index, statement)| {
            let exit = match statement.kind() {
                SyntaxKind::BREAK_EXPR => "break".to_owned(),
                SyntaxKind::NEXT_EXPR => "next".to_owned(),
                SyntaxKind::CALL_EXPR => bare_callee(statement)
                    .filter(|callee| matches!(callee.as_str(), "return" | "stop"))?,
                _ => return None,
            };
            Some((index, exit))
        })
    else {
        return;
    };
    let (Some(first), Some(last)) = (statements.get(index + 1), statements.last()) else {
        return;
    };
    let range = TextRange::new(
        significant_range(first).start(),
        significant_range(last).end(),
    );
    let written = if matches!(exit.as_str(), "break" | "next") {
        format!("`{exit}`")
    } else {
        format!("{exit}()")
    };
    let mut diagnostics = Vec::new();
    push_lint(
        &mut diagnostics,
        config.unreachable_code,
        Severity::Warning,
        "unreachable-code",
        range,
        format!("This code never runs, because {written} above it leaves the block first"),
        None,
    );
    unless_redefined.extend(
        diagnostics
            .into_iter()
            .map(|diagnostic| (exit.clone(), diagnostic)),
    );
}

/// `sprintf("%d of %d", x)` stops with "too few arguments", and a value no conversion uses is
/// dropped with a warning. Checked only when the format is a literal and every value is passed
/// by position, since `...` or a vector format could supply any count.
fn sprintf_arguments_lint(
    callee: &str,
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let format_index = arguments
        .iter()
        .position(|argument| {
            argument
                .name
                .as_ref()
                .is_some_and(|(name, _)| name == "fmt")
        })
        .unwrap_or(0);
    let Some(format) = arguments.get(format_index) else {
        return;
    };
    let other_named = arguments
        .iter()
        .enumerate()
        .any(|(index, argument)| index != format_index && argument.name.is_some());
    let spreads = arguments.iter().any(|argument| {
        argument
            .value
            .as_ref()
            .is_some_and(|value| value.kind() == SyntaxKind::NAME && value.text() == "...")
    });
    if other_named || spreads {
        return;
    }
    let Some(literal) = format
        .value
        .as_ref()
        .filter(|value| value.kind() == SyntaxKind::LITERAL)
    else {
        return;
    };
    let Some(token) = literal
        .first_token()
        .filter(|token| token.kind() == SyntaxKind::STRING)
    else {
        return;
    };
    let text = token.text();
    let Some(inner) = text.get(1..text.len().saturating_sub(1)) else {
        return;
    };
    let Some(wanted) = format_conversions(inner) else {
        return;
    };
    let given = arguments.len() - 1;
    if wanted == given {
        return;
    }
    let plural = |count: usize, word: &str| {
        if count == 1 {
            format!("{count} {word}")
        } else {
            format!("{count} {word}s")
        }
    };
    let consequence = if given < wanted {
        "R stops with \"too few arguments\""
    } else {
        "R ignores the extra values with a warning"
    };
    push_lint(
        diagnostics,
        config.sprintf_arguments,
        Severity::Warning,
        "sprintf-arguments",
        significant_range(literal),
        format!(
            "This format wants {} but {callee}() gets {}, so {consequence}",
            plural(wanted, "value"),
            plural(given, "value"),
        ),
        None,
    );
}

/// How many values a C-style format consumes, or `None` for a format this does not model
/// (`%1$s` positions, an unknown conversion, a trailing `%`).
fn format_conversions(format: &str) -> Option<usize> {
    let bytes = format.as_bytes();
    let mut count = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        index += 1;
        if bytes.get(index) == Some(&b'%') {
            index += 1;
            continue;
        }
        while bytes
            .get(index)
            .is_some_and(|byte| matches!(byte, b'-' | b'+' | b' ' | b'#' | b'0'))
        {
            index += 1;
        }
        for part in 0..2 {
            if part == 1 {
                if bytes.get(index) != Some(&b'.') {
                    break;
                }
                index += 1;
            }
            if bytes.get(index) == Some(&b'*') {
                count += 1;
                index += 1;
            } else {
                while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                    index += 1;
                }
            }
        }
        match bytes.get(index) {
            Some(
                b'a' | b'A' | b'd' | b'i' | b'f' | b'e' | b'E' | b'g' | b'G' | b'o' | b's' | b'x'
                | b'X',
            ) => {
                count += 1;
                index += 1;
            }
            _ => return None,
        }
    }
    Some(count)
}

/// `paste(..., sep = "")` is `paste0(...)`. The fix rebuilds the call from its other arguments,
/// so it is offered only when no comment sits inside the call.
fn paste_sep_lint(
    call: &SyntaxNode,
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(separator) = arguments.iter().find(|argument| {
        argument
            .name
            .as_ref()
            .is_some_and(|(name, _)| name == "sep")
            && argument
                .value
                .as_ref()
                .is_some_and(|value| matches!(value.text().to_string().as_str(), "\"\"" | "''"))
    }) else {
        return;
    };
    let has_comment = call
        .descendants_with_tokens()
        .any(|element| element.kind() == SyntaxKind::COMMENT);
    let fix = (!has_comment).then(|| {
        let rest: Vec<String> = arguments
            .iter()
            .filter(|argument| argument.node != separator.node)
            .map(|argument| significant_text(&argument.node))
            .collect();
        (
            Applicability::Safe,
            "Change to `paste0()`",
            format!("paste0({})", rest.join(", ")),
        )
    });
    push_lint(
        diagnostics,
        opt_in(config.paste_sep),
        Severity::Warning,
        "paste-sep",
        significant_range(call),
        "Use paste0(...) instead of paste(..., sep = \"\")",
        fix,
    );
}

/// `as.integer(1)` is `1L`: the literal says the type directly.
fn literal_coercion_lint(
    call: &SyntaxNode,
    callee: &str,
    value: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if value.kind() != SyntaxKind::LITERAL {
        return;
    }
    let Some(token) = value.first_token() else {
        return;
    };
    let text = token.text();
    let replacement = match (callee, token.kind()) {
        ("as.integer", SyntaxKind::DOUBLE) if text.bytes().all(|byte| byte.is_ascii_digit()) => {
            format!("{text}L")
        }
        ("as.integer", SyntaxKind::INTEGER) => text.to_owned(),
        ("as.numeric" | "as.double", SyntaxKind::DOUBLE) => text.to_owned(),
        ("as.numeric" | "as.double", SyntaxKind::INTEGER) => text.trim_end_matches('L').to_owned(),
        ("as.logical", SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW) => text.to_owned(),
        ("as.character", SyntaxKind::STRING) => text.to_owned(),
        _ => return,
    };
    push_lint(
        diagnostics,
        opt_in(config.literal_coercion),
        Severity::Warning,
        "literal-coercion",
        significant_range(call),
        format!("Write the literal `{replacement}` instead of coercing it with {callee}()"),
        Some((Applicability::Safe, "Use the literal", replacement)),
    );
}

/// `x == TRUE` is `x` and `x == FALSE` is `!x` for a logical `x`. The fix is unsafe because
/// `==` coerces: `1 == TRUE` and `"TRUE" == TRUE` are both `TRUE`.
fn redundant_equals_lint(
    comparison: &SyntaxNode,
    operator: SyntaxKind,
    lhs: &SyntaxNode,
    rhs: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let boolean = |node: &SyntaxNode| {
        (node.kind() == SyntaxKind::LITERAL)
            .then(|| node.first_token())
            .flatten()
            .and_then(|token| match token.kind() {
                SyntaxKind::TRUE_KW => Some(true),
                SyntaxKind::FALSE_KW => Some(false),
                _ => None,
            })
    };
    let (value, other) = match (boolean(lhs), boolean(rhs)) {
        (None, Some(value)) => (value, lhs),
        (Some(value), None) => (value, rhs),
        _ => return,
    };
    let keeps = value == (operator == SyntaxKind::EQ2);
    let other = significant_text(other);
    let replacement = if keeps { other } else { format!("!{other}") };
    let literal = if value { "TRUE" } else { "FALSE" };
    push_lint(
        diagnostics,
        opt_in(config.redundant_equals),
        Severity::Warning,
        "redundant-equals",
        significant_range(comparison),
        format!("Comparing with {literal} is redundant for a logical value. Write `{replacement}`"),
        Some((Applicability::Unsafe, "Drop the comparison", replacement)),
    );
}

/// The tidyverse style and lintr prefer double quotes. The fix swaps the delimiters only when
/// the text holds no double quote and no escaped single quote, so it never needs new escapes.
fn single_quotes_lint(
    text: &str,
    range: TextRange,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !is_enabled(config.single_quotes, None) {
        return;
    }
    let Some(inner) = text
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    else {
        return;
    };
    let fix = (!inner.contains('"') && !inner.contains("\\'")).then(|| {
        (
            Applicability::Safe,
            "Change to double quotes",
            format!("\"{inner}\""),
        )
    });
    push_lint(
        diagnostics,
        config.single_quotes,
        Severity::Warning,
        "single-quotes",
        range,
        "Use double quotes for strings",
        fix,
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
