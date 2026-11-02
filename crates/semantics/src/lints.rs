use {
    crate::{
        Db, SourceFile,
        diagnostics::{Applicability, Diagnostic, Severity},
        is_s3_method_name, item_naming, item_spans,
        naming::BindingKind,
        parse, s3_generics,
        stubs::{declaring_namespace, stubs},
    },
    std::cell::LazyCell,
    syntax::{
        Direction, NodeOrToken, NodeRef, SyntaxKind, SyntaxNode, TextRange, TreeEvent,
        ast::BinaryExpr,
        is_inline_annotation_kind,
        native_values::{object_system_function, object_systems_disabled_message},
        node_at, node_text, walk_tree,
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
    /// Off by default: `expect_equal(x, NULL)` and kin, which a specific expectation says better.
    pub testthat_expectation: LintLevel,
    /// Off by default: `stop(paste0(...))` and kin, which join their arguments themselves.
    pub condition_message: LintLevel,
    /// Off by default: `!(x == y)`, which is `x != y`.
    pub comparison_negation: LintLevel,
    /// Off by default: `all(!x)`, which is `!any(x)`.
    pub outer_negation: LintLevel,
    /// Off by default: a regular expression without regex syntax, which `fixed = TRUE` matches faster.
    pub fixed_regex: LintLevel,
    /// Off by default: `which(grepl(...))`, which is `grep(...)`.
    pub which_grepl: LintLevel,
    /// Off by default: `length(levels(x))`, which is `nlevels(x)`.
    pub length_levels: LintLevel,
    /// Off by default: a `browser()` call, which a debugging session leaves behind.
    pub browser_call: LintLevel,
    /// Off by default: a number such as `.5` without its leading zero.
    pub numeric_leading_zero: LintLevel,
    /// `length(x == y)`, which counts the comparison's elements instead of comparing a length.
    pub length_test: LintLevel,
    /// Off by default: an assignment inside an `if` or `while` condition.
    pub implicit_assignment: LintLevel,
    /// Off by default: `while (TRUE)`, which `repeat` spells directly.
    pub while_true: LintLevel,
    /// Off by default: `x %in% 1`, which is `x == 1` apart from NA.
    pub scalar_in: LintLevel,
    /// Off by default: `x[order(x)]`, which is `sort(x, na.last = TRUE)`.
    pub order_subset: LintLevel,
    /// Off by default: `apply(x, 1, sum)` and kin, which `rowSums(x)` and kin compute in C.
    pub matrix_apply: LintLevel,
    /// Off by default: `x <- {}`, which assigns NULL.
    pub empty_assignment: LintLevel,
    /// Off by default: `lapply(x, function(y) f(y))`, which is `lapply(x, f)`.
    pub unnecessary_lambda: LintLevel,
    /// Off by default: `expect_equal(2, x)`, which puts the expected value where testthat
    /// reports the value under test.
    pub yoda_test: LintLevel,
    /// Off by default: `nchar(x) == 0`, which `!nzchar(x)` answers without counting.
    pub nzchar: LintLevel,
    /// Off by default: `sample(1:n, k)`, which is `sample.int(n, k)`.
    pub sample_int: LintLevel,
    /// Off by default: `c("a")` or `c()`, where `c()` combines nothing.
    pub unnecessary_concatenation: LintLevel,
    /// Off by default: a line longer than `max_line_length` characters. The formatter never
    /// splits a line, so this is the one check that keeps lines short.
    pub line_length: LintLevel,
    /// The limit for `line_length`, 80 when unset.
    pub max_line_length: Option<usize>,
    /// Copied from `[check] object-systems = false` when the config loads, which turns
    /// every definition into an error whatever `object_system` says.
    #[serde(skip)]
    pub object_systems_disabled: bool,
    /// Off by default: data.table code runs unchanged, and moving it to `DataFrame` is optional.
    pub data_table: LintLevel,
}

/// Each lint's config key, which is also its code, with its level, for code that sets levels by
/// name, such as reading lintr's configuration. The destructuring names every field, so a new
/// field fails to compile until it is listed here or explicitly skipped.
pub fn lint_levels_mut(config: &mut LintConfig) -> Vec<(&'static str, &mut LintLevel)> {
    let LintConfig {
        assignment_operator,
        boolean_shorthand,
        trailing_comma,
        unused_parameter,
        unused_import,
        shadows_builtin,
        shadows_namespace,
        object_system,
        equals_na,
        vector_logic,
        length_sequence,
        class_equals,
        commented_code,
        redundant_return,
        duplicate_argument,
        unreachable_code,
        sprintf_arguments,
        any_is_na,
        paste_sep,
        lengths,
        literal_coercion,
        redundant_equals,
        single_quotes,
        testthat_expectation,
        condition_message,
        comparison_negation,
        outer_negation,
        fixed_regex,
        which_grepl,
        length_levels,
        browser_call,
        numeric_leading_zero,
        length_test,
        implicit_assignment,
        while_true,
        scalar_in,
        order_subset,
        matrix_apply,
        empty_assignment,
        unnecessary_lambda,
        yoda_test,
        nzchar,
        sample_int,
        unnecessary_concatenation,
        line_length,
        data_table,
        naming_style: _,
        missing_comma: _,
        max_line_length: _,
        object_systems_disabled: _,
    } = config;
    vec![
        ("assignment-operator", assignment_operator),
        ("boolean-shorthand", boolean_shorthand),
        ("trailing-comma", trailing_comma),
        ("unused-parameter", unused_parameter),
        ("unused-import", unused_import),
        ("shadows-builtin", shadows_builtin),
        ("shadows-namespace", shadows_namespace),
        ("object-system", object_system),
        ("equals-na", equals_na),
        ("vector-logic", vector_logic),
        ("length-sequence", length_sequence),
        ("class-equals", class_equals),
        ("commented-code", commented_code),
        ("redundant-return", redundant_return),
        ("duplicate-argument", duplicate_argument),
        ("unreachable-code", unreachable_code),
        ("sprintf-arguments", sprintf_arguments),
        ("any-is-na", any_is_na),
        ("paste-sep", paste_sep),
        ("lengths", lengths),
        ("literal-coercion", literal_coercion),
        ("redundant-equals", redundant_equals),
        ("single-quotes", single_quotes),
        ("testthat-expectation", testthat_expectation),
        ("condition-message", condition_message),
        ("comparison-negation", comparison_negation),
        ("outer-negation", outer_negation),
        ("fixed-regex", fixed_regex),
        ("which-grepl", which_grepl),
        ("length-levels", length_levels),
        ("browser-call", browser_call),
        ("numeric-leading-zero", numeric_leading_zero),
        ("length-test", length_test),
        ("implicit-assignment", implicit_assignment),
        ("while-true", while_true),
        ("scalar-in", scalar_in),
        ("order-subset", order_subset),
        ("matrix-apply", matrix_apply),
        ("empty-assignment", empty_assignment),
        ("unnecessary-lambda", unnecessary_lambda),
        ("yoda-test", yoda_test),
        ("nzchar", nzchar),
        ("sample-int", sample_int),
        ("unnecessary-concatenation", unnecessary_concatenation),
        ("line-length", line_length),
        ("data-table", data_table),
    ]
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
    line_length_lint(root, config, diagnostics);
    // `#:` annotation regions and inline annotations are type syntax, not R identifiers.
    // One entry per open node, so a `Leave` knows whether its node opened a region.
    let mut opens_annotation = Vec::new();
    let mut annotation_depth = 0usize;
    // The walk reads borrowed nodes and makes an owned one only where a cheap test says a lint
    // may fire.
    walk_tree(root.node_ref(), |event| match event {
        TreeEvent::Enter {
            node,
            parent,
            range,
        } => {
            let kind = node.kind();
            let opens = kind == SyntaxKind::ANNOTATION
                || is_inline_annotation_kind(kind, parent.map(NodeRef::kind));
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
                    data_table_lint(&red, config, &mut lints.unless_redefined, diagnostics);
                    call_lints(&red, config, diagnostics);
                }
                SyntaxKind::BRACE_EXPR => {
                    unreachable_code_lint(&red, config, &mut lints.unless_redefined);
                }
                SyntaxKind::UNARY_EXPR => comparison_negation_lint(&red, config, diagnostics),
                SyntaxKind::IF_EXPR | SyntaxKind::WHILE_EXPR => {
                    vector_logic_lint(&red, config, diagnostics);
                    implicit_assignment_lint(&red, config, diagnostics);
                    while_true_lint(&red, config, diagnostics);
                }
                SyntaxKind::SUBSET_EXPR => order_subset_lint(&red, config, diagnostics),
                SyntaxKind::FUNCTION_DEF => {
                    redundant_return_lint(&red, config, &mut lints.unless_redefined);
                }
                _ => {}
            }
        }
        TreeEvent::Leave(_) => {
            if opens_annotation.pop() == Some(true) {
                annotation_depth -= 1;
            }
        }
        TreeEvent::Token { kind, text, range } => {
            if kind == SyntaxKind::COMMENT && annotation_depth == 0 {
                commented_code_lint(text, range, config, diagnostics);
            }
            if kind == SyntaxKind::STRING && annotation_depth == 0 {
                single_quotes_lint(text, range, config, diagnostics);
            }
            if matches!(
                kind,
                SyntaxKind::DOUBLE | SyntaxKind::INTEGER | SyntaxKind::COMPLEX
            ) && annotation_depth == 0
                && text.starts_with('.')
            {
                push_lint(
                    diagnostics,
                    opt_in(config.numeric_leading_zero),
                    Severity::Warning,
                    "numeric-leading-zero",
                    range,
                    format!("Write `0{text}` with a leading zero, which is easier to read"),
                    Some((
                        Applicability::Safe,
                        "Add a leading zero",
                        format!("0{text}"),
                    )),
                );
            }
        }
    });
}

/// Counts characters, not bytes, so a line of accented text is measured as it reads. The range
/// starts at the first character past the limit, which is where an editor's ruler sits.
fn line_length_lint(root: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    let Some(severity) = lint_severity(opt_in(config.line_length), None) else {
        return;
    };
    let limit = config.max_line_length.unwrap_or(80);
    let text = node_text(root);
    let mut line_start = 0usize;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let length = content.chars().count();
        if let Some((overflow, _)) = content.char_indices().nth(limit) {
            let start = line_start + overflow;
            let end = line_start + content.len();
            if let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end)) {
                diagnostics.push(Diagnostic::new(
                    TextRange::new(start.into(), end.into()),
                    severity,
                    "line-length",
                    format!("This line is {length} characters long, over the limit of {limit}"),
                ));
            }
        }
        line_start += line.len();
    }
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

/// A conservative test on a borrowed node for whether the lint for `node` can report anything:
/// `false` only when it certainly reports nothing.
fn may_lint(node: NodeRef<'_>, parent: Option<NodeRef<'_>>, config: &LintConfig) -> bool {
    match node.kind() {
        SyntaxKind::BINARY_EXPR => node.child_kinds().any(|kind| {
            matches!(
                kind,
                SyntaxKind::EQ | SyntaxKind::EQ2 | SyntaxKind::BANG_EQ | SyntaxKind::COLON
            ) || ((config.naming_style.is_some() || is_enabled(config.empty_assignment, None))
                && kind == SyntaxKind::LESS_MINUS)
                || (is_enabled(config.empty_assignment, None) && kind == SyntaxKind::LESS2_MINUS)
                || (is_enabled(config.scalar_in, None) && kind == SyntaxKind::SPECIAL)
                || (is_enabled(config.nzchar, None) && kind == SyntaxKind::GREATER)
        }),
        SyntaxKind::IF_EXPR | SyntaxKind::WHILE_EXPR => {
            (is_enabled(config.while_true, None) && node.kind() == SyntaxKind::WHILE_EXPR)
                || is_enabled(config.implicit_assignment, None)
                || (is_enabled(config.vector_logic, Some(Severity::Warning))
                    && node.child_kinds().any(|kind| {
                        matches!(
                            kind,
                            SyntaxKind::BINARY_EXPR
                                | SyntaxKind::UNARY_EXPR
                                | SyntaxKind::PAREN_EXPR
                                | SyntaxKind::CALL_EXPR
                        )
                    }))
        }
        SyntaxKind::SUBSET_EXPR => is_enabled(config.order_subset, None),
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
        SyntaxKind::UNARY_EXPR => {
            is_enabled(config.comparison_negation, None)
                && node.child_kinds().any(|kind| kind == SyntaxKind::BANG)
                && node
                    .child_kinds()
                    .any(|kind| kind == SyntaxKind::PAREN_EXPR)
        }
        SyntaxKind::CALL_EXPR => {
            let mut children = node.child_nodes();
            let callee_lints = match children.next() {
                Some(callee) if callee.kind() == SyntaxKind::NAME => callee
                    .single_token_text()
                    .is_none_or(|name| callee_may_lint(name, config)),
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
    if matches!(
        operator.kind(),
        SyntaxKind::EQ2 | SyntaxKind::BANG_EQ | SyntaxKind::GREATER
    ) && let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs())
    {
        nzchar_lint(node, operator.kind(), &lhs, &rhs, config, diagnostics);
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
    if operator.kind() == SyntaxKind::SPECIAL
        && operator.text() == "%in%"
        && let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs())
        && is_constant(&rhs)
        && !is_na_literal(&rhs)
        && !is_null_literal(&rhs)
    {
        push_lint(
            diagnostics,
            opt_in(config.scalar_in),
            Severity::Warning,
            "scalar-in",
            significant_range(node),
            "Use `==` to compare with one value: `x %in% y` with a single value on the right is `x == y`, apart from NA",
            Some((
                Applicability::Unsafe,
                "Change to `==`",
                in_place_of(
                    node,
                    format!("{} == {}", significant_text(&lhs), significant_text(&rhs)),
                ),
            )),
        );
    }
    if matches!(
        operator.kind(),
        SyntaxKind::LESS_MINUS | SyntaxKind::LESS2_MINUS | SyntaxKind::EQ
    ) && let Some(value) = binary.rhs()
        && value.kind() == SyntaxKind::BRACE_EXPR
        && value.first_child().is_none()
        && !value
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::COMMENT)
    {
        push_lint(
            diagnostics,
            opt_in(config.empty_assignment),
            Severity::Warning,
            "empty-assignment",
            significant_range(&value),
            "Assign NULL instead of an empty `{}`, which evaluates to NULL",
            Some((Applicability::Safe, "Change to `NULL`", "NULL".to_owned())),
        );
    }
    if let Some(style) = config.naming_style
        && matches!(operator.kind(), SyntaxKind::LESS_MINUS | SyntaxKind::EQ)
        && let Some(target) = binary
            .lhs()
            .filter(|target| target.kind() == SyntaxKind::NAME)
    {
        let name = node_text(&target).into_owned();
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
            (Some(node_text(&package).into_owned()), name)
        }
        _ => return,
    };
    let function = node_text(&name);
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
                .map(|diagnostic| (function.to_string(), diagnostic)),
        ),
    }
}

/// Like `object_system_lint`, only the calls that create a data.table: every other data.table
/// function needs one first, so these mark where data.table enters the code.
fn data_table_lint(
    call: &SyntaxNode,
    config: &LintConfig,
    unless_redefined: &mut Vec<(String, Diagnostic)>,
    always: &mut Vec<Diagnostic>,
) {
    if lint_severity(config.data_table, None).is_none() {
        return;
    }
    let Some(callee) = call.first_child() else {
        return;
    };
    let (qualified, name) = match callee.kind() {
        SyntaxKind::NAME => (false, callee),
        SyntaxKind::NAMESPACE_EXPR => {
            let mut names = callee
                .children()
                .filter(|child| child.kind() == SyntaxKind::NAME);
            let (Some(package), Some(name)) = (names.next(), names.next()) else {
                return;
            };
            if package.text() != "data.table" {
                return;
            }
            (true, name)
        }
        _ => return,
    };
    let function = name.text().to_string();
    let Some(equivalent) = data_table_equivalent(&function) else {
        return;
    };
    let mut found = Vec::new();
    push_lint(
        &mut found,
        config.data_table,
        Severity::Warning,
        "data-table",
        name.text_range(),
        format!(
            "`{function}` creates a data.table. Its typed equivalent is {equivalent}, see https://ry-lang.org/guides/from-data-table/"
        ),
        None,
    );
    if qualified {
        always.extend(found);
    } else {
        unless_redefined.extend(
            found
                .into_iter()
                .map(|diagnostic| (function.clone(), diagnostic)),
        );
    }
}

/// The DataFrame call that replaces a data.table constructor.
fn data_table_equivalent(function: &str) -> Option<&'static str> {
    Some(match function {
        "data.table" => "`DataFrame()`",
        "fread" => "`DataFrame.read_csv()`",
        "as.data.table" | "setDT" => "`as.DataFrame()`",
        _ => return None,
    })
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

/// Whether a call to the bare callee `name` can report anything, so the pre-check makes an
/// owned node only for calls some enabled lint looks at.
fn callee_may_lint(name: &str, config: &LintConfig) -> bool {
    let opted = |level| is_enabled(level, None);
    match name {
        "seq" => is_enabled(config.length_sequence, Some(Severity::Warning)),
        "sprintf" | "gettextf" => is_enabled(config.sprintf_arguments, Some(Severity::Warning)),
        "any" => opted(config.any_is_na) || opted(config.outer_negation),
        "all" => opted(config.outer_negation),
        "paste" => opted(config.paste_sep),
        "sapply" | "vapply" => opted(config.lengths) || opted(config.unnecessary_lambda),
        "lapply" | "Filter" | "Map" | "map" | "map_chr" | "map_dbl" | "map_int" | "map_lgl"
        | "walk" | "keep" | "discard" => opted(config.unnecessary_lambda),
        "apply" => opted(config.matrix_apply),
        "sample" => opted(config.sample_int),
        "c" => opted(config.unnecessary_concatenation),
        "as.integer" | "as.numeric" | "as.double" | "as.logical" | "as.character" => {
            opted(config.literal_coercion)
        }
        "expect_equal" | "expect_identical" => {
            opted(config.testthat_expectation) || opted(config.yoda_test)
        }
        "expect_true" | "expect_false" => opted(config.testthat_expectation),
        "stop" | "warning" | "message" | "packageStartupMessage" => opted(config.condition_message),
        "grepl" | "grep" | "sub" | "gsub" | "regexpr" | "gregexpr" | "regexec" | "strsplit" => {
            opted(config.fixed_regex)
        }
        "which" => opted(config.which_grepl),
        "length" => {
            opted(config.length_levels) || is_enabled(config.length_test, Some(Severity::Warning))
        }
        "browser" => opted(config.browser_call),
        name => {
            object_system_function(name).is_some()
                || (opted(config.data_table) && data_table_equivalent(name).is_some())
        }
    }
}

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
                    let text = node_text(&name);
                    (unquoted(&text).to_owned(), significant_range(&name))
                })
            } else {
                None
            };
            let value = children.next();
            CallArgument { node, name, value }
        })
        .collect()
}

fn unquoted(name: &str) -> &str {
    name.strip_prefix(['`', '"', '\''])
        .and_then(|inner| inner.strip_suffix(['`', '"', '\'']))
        .unwrap_or(name)
}

/// The name of a bare callee, such as `sprintf` in `sprintf(...)` but not in `base::sprintf(...)`.
fn bare_callee(call: &SyntaxNode) -> Option<String> {
    call.first_child()
        .filter(|callee| callee.kind() == SyntaxKind::NAME)
        .map(|callee| node_text(&callee).into_owned())
}

fn call_lints(call: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    let callee = bare_callee(call);
    if repeats_argument_name(call) {
        duplicate_argument_lint(
            callee.as_deref(),
            &call_arguments(call),
            config,
            diagnostics,
        );
    }
    let Some(callee) = callee else {
        return;
    };
    // Lazy because collecting the arguments allocates, and most callees have no lint below.
    let arguments = LazyCell::new(|| call_arguments(call));
    let positional = LazyCell::new(|| {
        arguments
            .iter()
            .filter(|argument| argument.name.is_none())
            .filter_map(|argument| argument.value.as_ref())
            .collect::<Vec<&SyntaxNode>>()
    });
    let all_positional = LazyCell::new(|| positional.len() == arguments.len());
    match callee.as_str() {
        "seq" if *all_positional && positional.len() == 1 => {
            length_sequence_lint(call, positional[0], true, config, diagnostics);
        }
        "sprintf" | "gettextf" => sprintf_arguments_lint(&callee, &arguments, config, diagnostics),
        "any" | "all" if *all_positional && positional.len() == 1 => {
            outer_negation_lint(call, &callee, positional[0], config, diagnostics);
            if callee != "any" {
                return;
            }
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
        "sapply" | "vapply" | "lapply" | "Filter" | "Map" | "map" | "map_chr" | "map_dbl"
        | "map_int" | "map_lgl" | "walk" | "keep" | "discard" => {
            unnecessary_lambda_lint(&arguments, config, diagnostics);
            if !matches!(callee.as_str(), "sapply" | "vapply") {
                return;
            }
            let expected = if callee == "sapply" { 2 } else { 3 };
            let counts_lengths = *all_positional
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
            if *all_positional && positional.len() == 1 =>
        {
            literal_coercion_lint(call, &callee, positional[0], config, diagnostics);
        }
        "expect_equal" | "expect_identical" | "expect_true" | "expect_false" if *all_positional => {
            testthat_expectation_lint(call, &callee, &positional, config, diagnostics);
            if let [expected, actual] = positional[..]
                && matches!(callee.as_str(), "expect_equal" | "expect_identical")
                && is_constant(expected)
                && !is_null_literal(expected)
                && !is_constant(actual)
            {
                push_lint(
                    diagnostics,
                    opt_in(config.yoda_test),
                    Severity::Warning,
                    "yoda-test",
                    significant_range(call),
                    format!(
                        "Put the value under test first: testthat reports the first argument of {callee}() as the actual value"
                    ),
                    (!has_comment(call)).then(|| {
                        (
                            Applicability::Unsafe,
                            "Swap the arguments",
                            format!(
                                "{callee}({}, {})",
                                significant_text(actual),
                                significant_text(expected)
                            ),
                        )
                    }),
                );
            }
        }
        "apply" => matrix_apply_lint(call, &arguments, config, diagnostics),
        "sample" => sample_int_lint(call, &arguments, config, diagnostics),
        "c" if arguments.len() <= 1 && !is_quoted(call) => {
            let constant = match &arguments[..] {
                [] => Some("NULL".to_owned()),
                [argument] => argument
                    .value
                    .as_ref()
                    .filter(|value| argument.name.is_none() && is_constant(value))
                    .map(significant_text),
                _ => None,
            };
            let has_comment = call
                .descendants_with_tokens()
                .any(|element| element.kind() == SyntaxKind::COMMENT);
            if let Some(constant) = constant {
                push_lint(
                    diagnostics,
                    opt_in(config.unnecessary_concatenation),
                    Severity::Warning,
                    "unnecessary-concatenation",
                    significant_range(call),
                    format!("Write `{constant}` directly: c() has nothing to combine here"),
                    (!has_comment).then(|| {
                        (
                            Applicability::Safe,
                            "Remove `c()`",
                            in_place_of(call, constant.clone()),
                        )
                    }),
                );
            }
        }
        "stop" | "warning" | "message" | "packageStartupMessage" => {
            condition_message_lint(&callee, &arguments, config, diagnostics);
        }
        "grepl" | "grep" | "sub" | "gsub" | "regexpr" | "gregexpr" | "regexec" | "strsplit" => {
            fixed_regex_lint(call, &callee, &arguments, config, diagnostics);
        }
        "which" if *all_positional && positional.len() == 1 => {
            // grep() puts `value` fifth where grepl() has `fixed`, so a fifth positional
            // argument would change meaning in the rewritten call.
            if positional[0].kind() != SyntaxKind::CALL_EXPR
                || bare_callee(positional[0]).as_deref() != Some("grepl")
                || call_arguments(positional[0])
                    .iter()
                    .filter(|argument| argument.name.is_none())
                    .count()
                    > 4
            {
                return;
            }
            let Some(grepl_arguments) = positional[0]
                .children()
                .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
            else {
                return;
            };
            push_lint(
                diagnostics,
                opt_in(config.which_grepl),
                Severity::Warning,
                "which-grepl",
                significant_range(call),
                "Use grep(...), which returns the matching positions directly, instead of which(grepl(...))",
                Some((
                    Applicability::Safe,
                    "Change to `grep()`",
                    format!("grep{}", significant_text(&grepl_arguments)),
                )),
            );
        }
        "length" if *all_positional && positional.len() == 1 => {
            length_test_lint(call, positional[0], config, diagnostics);
            if let Some(("levels", value)) = single_argument_of(positional[0], &["levels"]) {
                push_lint(
                    diagnostics,
                    opt_in(config.length_levels),
                    Severity::Warning,
                    "length-levels",
                    significant_range(call),
                    "Use nlevels(x) instead of length(levels(x))",
                    Some((
                        Applicability::Safe,
                        "Change to `nlevels()`",
                        format!("nlevels({})", significant_text(&value)),
                    )),
                );
            }
        }
        "browser" if !is_quoted(call) => push_lint(
            diagnostics,
            opt_in(config.browser_call),
            Severity::Warning,
            "browser-call",
            significant_range(call),
            "Remove this browser() call, which pauses the code whenever it runs in an interactive session",
            None,
        ),
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

/// Whether two named arguments of `call` may share a name. A name of several tokens answers yes and leaves the
/// decision to [`duplicate_argument_lint`].
fn repeats_argument_name(call: &SyntaxNode) -> bool {
    let Some(list) = call
        .node_ref()
        .child_nodes()
        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
    else {
        return false;
    };
    let mut seen: Vec<&str> = Vec::new();
    for argument in list
        .child_nodes()
        .filter(|child| child.kind() == SyntaxKind::ARGUMENT)
    {
        if !argument.child_kinds().any(|kind| kind == SyntaxKind::EQ) {
            continue;
        }
        let Some(name) = argument.child_nodes().next() else {
            continue;
        };
        let Some(text) = name.single_token_text() else {
            return true;
        };
        let name = unquoted(text);
        if name.is_empty() {
            continue;
        }
        if seen.contains(&name) {
            return true;
        }
        seen.push(name);
    }
    false
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
    if is_quoted(block) {
        return;
    }
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
                .is_some_and(|value| matches!(&*node_text(value), "\"\"" | "''"))
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
        // Beyond `.Machine$integer.max`, `as.integer()` gives NA where `L` keeps a double.
        ("as.integer", SyntaxKind::DOUBLE)
            if text.bytes().all(|byte| byte.is_ascii_digit())
                && text
                    .parse::<u64>()
                    .is_ok_and(|value| value <= 2_147_483_647) =>
        {
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
    let (value, other) = match (boolean_literal(lhs), boolean_literal(rhs)) {
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
        Some((
            Applicability::Unsafe,
            "Drop the comparison",
            in_place_of(comparison, replacement),
        )),
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

/// An assignment in a condition reads like a comparison (`if (x <- 0)` for `if (x < 0)`), and
/// its side effect is easy to miss. A function defined in the condition is its own scope.
fn implicit_assignment_lint(
    node: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !is_enabled(config.implicit_assignment, None) {
        return;
    }
    let Some(condition) = node.first_child() else {
        return;
    };
    let keyword = if node.kind() == SyntaxKind::IF_EXPR {
        "if"
    } else {
        "while"
    };
    let mut pending = vec![condition];
    while let Some(expression) = pending.pop() {
        if expression.kind() == SyntaxKind::FUNCTION_DEF {
            continue;
        }
        let assigns = BinaryExpr::cast(expression.clone())
            .and_then(|binary| binary.operator())
            .is_some_and(|operator| {
                matches!(
                    operator.kind(),
                    SyntaxKind::LESS_MINUS
                        | SyntaxKind::LESS2_MINUS
                        | SyntaxKind::MINUS_GREATER
                        | SyntaxKind::MINUS_GREATER2
                )
            });
        if assigns {
            push_lint(
                diagnostics,
                opt_in(config.implicit_assignment),
                Severity::Warning,
                "implicit-assignment",
                significant_range(&expression),
                format!(
                    "Move this assignment out of the `{keyword}` condition, where it reads like a comparison"
                ),
                None,
            );
        }
        pending.extend(expression.children());
    }
}

fn while_true_lint(node: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    if node.kind() != SyntaxKind::WHILE_EXPR
        || !node
            .first_child()
            .is_some_and(|condition| boolean_literal(&condition) == Some(true))
    {
        return;
    }
    let Some(close) = node
        .children_with_tokens()
        .find(|element| element.kind() == SyntaxKind::R_PAREN)
    else {
        return;
    };
    let range = TextRange::new(significant_range(node).start(), close.text_range().end());
    let has_comment = node
        .children_with_tokens()
        .take_while(|element| element.kind() != SyntaxKind::R_PAREN)
        .any(|element| element.kind() == SyntaxKind::COMMENT);
    // `while (TRUE)x` needs no space after the parenthesis, but `repeatx` would be a name.
    let spaced = close
        .next_sibling_or_token()
        .is_none_or(|next| next.kind().is_trivia());
    let replacement = if spaced { "repeat" } else { "repeat " };
    push_lint(
        diagnostics,
        opt_in(config.while_true),
        Severity::Warning,
        "while-true",
        range,
        "Use `repeat` for a loop that only ends with `break`, instead of `while (TRUE)`",
        (!has_comment).then(|| {
            (
                Applicability::Safe,
                "Change to `repeat`",
                replacement.to_owned(),
            )
        }),
    );
}

/// `order()` puts NA last and `sort()` drops it by default, so the fix keeps NA with
/// `na.last = TRUE`. It is unsafe because `x[order(x)]` also works on a value whose `sort()`
/// method sorts differently.
fn order_subset_lint(node: &SyntaxNode, config: &LintConfig, diagnostics: &mut Vec<Diagnostic>) {
    let Some(object) = node.first_child() else {
        return;
    };
    let arguments = call_arguments(node);
    let [argument] = &arguments[..] else {
        return;
    };
    let Some(order) = argument
        .value
        .as_ref()
        .filter(|value| argument.name.is_none() && bare_callee(value).as_deref() == Some("order"))
    else {
        return;
    };
    let order_arguments = call_arguments(order);
    let mut sorted = None;
    let mut decreasing = None;
    for order_argument in &order_arguments {
        match (&order_argument.name, &order_argument.value) {
            (None, Some(value)) if sorted.is_none() => sorted = Some(value),
            (Some((name, _)), Some(value)) if name == "decreasing" => decreasing = Some(value),
            _ => return,
        }
    }
    if sorted.is_none_or(|sorted| significant_text(sorted) != significant_text(&object)) {
        return;
    }
    let object = significant_text(&object);
    let decreasing = decreasing
        .map(|value| format!(", decreasing = {}", significant_text(value)))
        .unwrap_or_default();
    push_lint(
        diagnostics,
        opt_in(config.order_subset),
        Severity::Warning,
        "order-subset",
        significant_range(node),
        "Use sort(x) instead of x[order(x)], which builds an index only to subset with it",
        Some((
            Applicability::Unsafe,
            "Change to `sort()`",
            format!("sort({object}{decreasing}, na.last = TRUE)"),
        )),
    );
}

/// `apply()` calls its function once per row or column from R code, where the `rowSums()`
/// family loops in C. The fix is unsafe: `apply()` turns a data frame into a matrix first, so
/// the two disagree on a data frame with a non-numeric column.
fn matrix_apply_lint(
    call: &SyntaxNode,
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (positional, named): (Vec<&CallArgument>, Vec<&CallArgument>) = arguments
        .iter()
        .partition(|argument| argument.name.is_none());
    let [matrix, margin, function] = positional[..] else {
        return;
    };
    let na_rm = match &named[..] {
        [] => None,
        [argument]
            if argument
                .name
                .as_ref()
                .is_some_and(|(name, _)| name == "na.rm") =>
        {
            argument.value.as_ref()
        }
        _ => return,
    };
    let (Some(matrix), Some(margin), Some(function)) =
        (&matrix.value, &margin.value, &function.value)
    else {
        return;
    };
    let prefix = match &*node_text(margin) {
        "1" | "1L" => "row",
        "2" | "2L" => "col",
        _ => return,
    };
    let suffix = match &*node_text(function) {
        "sum" if function.kind() == SyntaxKind::NAME => "Sums",
        "mean" if function.kind() == SyntaxKind::NAME => "Means",
        _ => return,
    };
    let replacement = format!(
        "{prefix}{suffix}({}{})",
        significant_text(matrix),
        na_rm
            .map(|value| format!(", na.rm = {}", significant_text(value)))
            .unwrap_or_default()
    );
    let title = format!("Change to `{prefix}{suffix}()`");
    push_lint(
        diagnostics,
        opt_in(config.matrix_apply),
        Severity::Warning,
        "matrix-apply",
        significant_range(call),
        format!(
            "Use {prefix}{suffix}(), which computes in C, instead of apply() with {}()",
            function.text()
        ),
        (!has_comment(call)).then_some((Applicability::Unsafe, title.as_str(), replacement)),
    );
}

/// The fix is unsafe because extra arguments the apply function forwards through its `...`
/// reach the named function, where the anonymous one would have rejected them.
fn unnecessary_lambda_lint(
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for function in arguments
        .iter()
        .filter_map(|argument| argument.value.as_ref())
        .filter(|value| value.kind() == SyntaxKind::FUNCTION_DEF)
    {
        let Some(parameters) = function
            .children()
            .find(|child| child.kind() == SyntaxKind::PARAMETER_LIST)
        else {
            continue;
        };
        let mut parameter_nodes = parameters
            .children()
            .filter(|child| child.kind() == SyntaxKind::PARAMETER);
        let (Some(parameter), None) = (parameter_nodes.next(), parameter_nodes.next()) else {
            continue;
        };
        // A default or a type annotation is a child node; a bare parameter is one token.
        if parameter.first_child().is_some() {
            continue;
        }
        let Some(name) = parameter
            .children_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| token.kind() == SyntaxKind::IDENT)
        else {
            continue;
        };
        let Some(body) = function.children().last().filter(|body| {
            body.kind() == SyntaxKind::CALL_EXPR
                && !body
                    .descendants_with_tokens()
                    .any(|element| element.kind() == SyntaxKind::COMMENT)
        }) else {
            continue;
        };
        let Some(callee) = body.first_child().filter(|callee| {
            matches!(callee.kind(), SyntaxKind::NAME | SyntaxKind::NAMESPACE_EXPR)
        }) else {
            continue;
        };
        let body_arguments = call_arguments(&body);
        let forwards_parameter = match &body_arguments[..] {
            [argument] => {
                argument.name.is_none()
                    && argument.value.as_ref().is_some_and(|value| {
                        value.kind() == SyntaxKind::NAME && value.text() == name.text()
                    })
            }
            _ => false,
        };
        // These read their argument's expression, not its value, so the wrapper is needed.
        let quoting = matches!(
            &*node_text(&callee),
            "quote"
                | "bquote"
                | "substitute"
                | "missing"
                | "match.arg"
                | "deparse"
                | "enquo"
                | "enexpr"
                | "ensym"
        );
        if !forwards_parameter || quoting || callee.text() == name.text() {
            continue;
        }
        let callee = significant_text(&callee);
        push_lint(
            diagnostics,
            opt_in(config.unnecessary_lambda),
            Severity::Warning,
            "unnecessary-lambda",
            significant_range(function),
            format!("Pass `{callee}` itself instead of a function that only calls it"),
            Some((
                Applicability::Unsafe,
                &format!("Change to `{callee}`"),
                callee.clone(),
            )),
        );
    }
}

/// `nchar(x) == 0` counts every string to test for emptiness. The fix is unsafe because the two
/// disagree on `NA`: `nchar(NA)` is 2, and `nzchar()` calls a missing string non-empty.
fn nzchar_lint(
    comparison: &SyntaxNode,
    operator: SyntaxKind,
    lhs: &SyntaxNode,
    rhs: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(("nchar", value)) = single_argument_of(lhs, &["nchar"]) else {
        return;
    };
    if !matches!(&*node_text(rhs), "0" | "0L") {
        return;
    }
    let negation = if operator == SyntaxKind::EQ2 { "!" } else { "" };
    let replacement = format!("{negation}nzchar({})", significant_text(&value));
    push_lint(
        diagnostics,
        opt_in(config.nzchar),
        Severity::Warning,
        "nzchar",
        significant_range(comparison),
        format!("Use `{replacement}`, which tests for an empty string without counting characters"),
        Some((
            Applicability::Unsafe,
            "Change to `nzchar()`",
            in_place_of(comparison, replacement.clone()),
        )),
    );
}

/// `sample(1:n, k)` draws the same values as `sample.int(n, k)` but builds `1:n` first. The fix
/// is unsafe because `1:0` is `c(1, 0)`, which `sample()` draws from where `sample.int(0, k)`
/// fails.
fn sample_int_lint(
    call: &SyntaxNode,
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(first) = arguments.first().filter(|argument| argument.name.is_none()) else {
        return;
    };
    let Some(value) = &first.value else {
        return;
    };
    let Some(sequence) = BinaryExpr::cast(value.clone()) else {
        return;
    };
    let (Some(operator), Some(lhs), Some(count)) =
        (sequence.operator(), sequence.lhs(), sequence.rhs())
    else {
        return;
    };
    if operator.kind() != SyntaxKind::COLON || !matches!(&*node_text(&lhs), "1" | "1L") {
        return;
    }
    let Some(list) = call
        .children()
        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
    else {
        return;
    };
    let list_start = list.text_range().start();
    let first_range = significant_range(value) - list_start;
    let list_text = node_text(&list).into_owned();
    let (Some(before), Some(after)) = (
        list_text.get(..usize::from(first_range.start())),
        list_text.get(usize::from(first_range.end())..),
    ) else {
        return;
    };
    let count = significant_text(&count);
    push_lint(
        diagnostics,
        opt_in(config.sample_int),
        Severity::Warning,
        "sample-int",
        significant_range(call),
        format!(
            "Use sample.int({count}, ...), which draws the same values without building 1:{count}"
        ),
        Some((
            Applicability::Unsafe,
            "Change to `sample.int()`",
            format!("sample.int{before}{count}{after}"),
        )),
    );
}

/// `length(x == y)` counts the comparison's elements, which is `length(x)` whatever `y` is,
/// so the comparison was surely meant to apply to the length.
fn length_test_lint(
    call: &SyntaxNode,
    value: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(comparison) = BinaryExpr::cast(value.clone()) else {
        return;
    };
    let (Some(operator), Some(lhs), Some(rhs)) =
        (comparison.operator(), comparison.lhs(), comparison.rhs())
    else {
        return;
    };
    if !matches!(
        operator.kind(),
        SyntaxKind::EQ2
            | SyntaxKind::BANG_EQ
            | SyntaxKind::LESS
            | SyntaxKind::GREATER
            | SyntaxKind::LESS_EQ
            | SyntaxKind::GREATER_EQ
    ) {
        return;
    }
    let operator = operator.text().to_owned();
    let lhs = significant_text(&lhs);
    let rhs = significant_text(&rhs);
    push_lint(
        diagnostics,
        config.length_test,
        Severity::Warning,
        "length-test",
        significant_range(call),
        format!(
            "`length({lhs} {operator} {rhs})` is the length of the comparison, not a test of the length. Did you mean `length({lhs}) {operator} {rhs}`?"
        ),
        Some((
            Applicability::Unsafe,
            "Compare the length",
            in_place_of(call, format!("length({lhs}) {operator} {rhs}")),
        )),
    );
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

/// `all(!x)` is `!any(x)` and `any(!x)` is `!all(x)` by De Morgan's laws, which hold for NA in
/// R's three-valued logic too. The negation then runs once on the result, not on every element.
fn outer_negation_lint(
    call: &SyntaxNode,
    callee: &str,
    value: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // `!!x` is rlang's injection operator, not a double negation.
    let Some(operand) = negated_operand(value).filter(|operand| negated_operand(operand).is_none())
    else {
        return;
    };
    let other = if callee == "all" { "any" } else { "all" };
    push_lint(
        diagnostics,
        opt_in(config.outer_negation),
        Severity::Warning,
        "outer-negation",
        significant_range(call),
        format!("Use `!{other}(x)` instead of `{callee}(!x)`, which negates every element first"),
        Some((
            Applicability::Safe,
            &format!("Change to `!{other}()`"),
            in_place_of(call, format!("!{other}({})", significant_text(&operand))),
        )),
    );
}

/// `!(x == y)` is `x != y`, and every comparison has such a negation, which also holds for NA.
fn comparison_negation_lint(
    node: &SyntaxNode,
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(parenthesized) = negated_operand(node) else {
        return;
    };
    // In `!!(x == y)` the outer `!` makes rlang's injection operator, which must keep its operand.
    if parenthesized.kind() != SyntaxKind::PAREN_EXPR
        || node.parent().as_ref().and_then(negated_operand).as_ref() == Some(node)
    {
        return;
    }
    let Some(comparison) = parenthesized.first_child().and_then(BinaryExpr::cast) else {
        return;
    };
    let (Some(operator), Some(lhs), Some(rhs)) =
        (comparison.operator(), comparison.lhs(), comparison.rhs())
    else {
        return;
    };
    let negated = match operator.kind() {
        SyntaxKind::EQ2 => "!=",
        SyntaxKind::BANG_EQ => "==",
        SyntaxKind::LESS => ">=",
        SyntaxKind::GREATER => "<=",
        SyntaxKind::LESS_EQ => ">",
        SyntaxKind::GREATER_EQ => "<",
        _ => return,
    };
    let has_comment = node
        .descendants_with_tokens()
        .any(|element| element.kind() == SyntaxKind::COMMENT);
    let fix = (!has_comment).then(|| {
        (
            Applicability::Safe,
            "Negate the comparison",
            in_place_of(
                node,
                format!(
                    "{} {negated} {}",
                    significant_text(&lhs),
                    significant_text(&rhs)
                ),
            ),
        )
    });
    push_lint(
        diagnostics,
        opt_in(config.comparison_negation),
        Severity::Warning,
        "comparison-negation",
        significant_range(node),
        format!("Use `{negated}` instead of negating `{}`", operator.text()),
        fix,
    );
}

/// testthat has an expectation for each of these checks, and its failure message names what
/// went wrong (`x is not NULL`) instead of printing two values. The fixes that change which
/// values pass are unsafe. `expect_true(x)` ignores attributes that `expect_equal(x, TRUE)`
/// compares, and `expect_false(x)` accepts only `FALSE` where `expect_true(!x)` also passes
/// for `0`. `expect_length()` and `expect_named()` reject a value without a length or names,
/// and `expect_s3_class()` rejects an S4 object that `inherits()` accepts.
fn testthat_expectation_lint(
    call: &SyntaxNode,
    callee: &str,
    positional: &[&SyntaxNode],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let text = |node: &SyntaxNode| significant_text(node);
    let (applicability, expectation, replacement) = match (callee, positional) {
        ("expect_equal" | "expect_identical", [actual, expected]) => {
            if is_null_literal(expected) {
                let replacement = format!("expect_null({})", text(actual));
                (Applicability::Safe, "expect_null", replacement)
            } else if let Some(value) = boolean_literal(expected) {
                let expectation = if value { "expect_true" } else { "expect_false" };
                let replacement = format!("{expectation}({})", text(actual));
                (Applicability::Unsafe, expectation, replacement)
            } else if let Some((function, inner)) = single_argument_of(actual, &["length", "names"])
            {
                let expectation = if function == "length" {
                    "expect_length"
                } else {
                    "expect_named"
                };
                let replacement = format!("{expectation}({}, {})", text(&inner), text(expected));
                (Applicability::Unsafe, expectation, replacement)
            } else {
                return;
            }
        }
        ("expect_true" | "expect_false", [value]) => {
            if let Some(operand) = negated_operand(value) {
                let expectation = if callee == "expect_true" {
                    "expect_false"
                } else {
                    "expect_true"
                };
                let replacement = format!("{expectation}({})", text(&operand));
                (Applicability::Unsafe, expectation, replacement)
            } else if callee == "expect_false" {
                return;
            } else if let Some((_, inner)) = single_argument_of(value, &["is.null"]) {
                let replacement = format!("expect_null({})", text(&inner));
                (Applicability::Safe, "expect_null", replacement)
            } else {
                let inherits = (value.kind() == SyntaxKind::CALL_EXPR
                    && bare_callee(value).as_deref() == Some("inherits"))
                .then(|| call_arguments(value))
                .filter(|arguments| {
                    arguments.len() == 2 && arguments.iter().all(|argument| argument.name.is_none())
                });
                let Some([object, class]) = inherits.as_deref() else {
                    return;
                };
                let (Some(object), Some(class)) = (&object.value, &class.value) else {
                    return;
                };
                let replacement = format!("expect_s3_class({}, {})", text(object), text(class));
                (Applicability::Unsafe, "expect_s3_class", replacement)
            }
        }
        _ => return,
    };
    push_lint(
        diagnostics,
        opt_in(config.testthat_expectation),
        Severity::Warning,
        "testthat-expectation",
        significant_range(call),
        format!(
            "Use {expectation}(), which says what the test checks and fails with a clearer message"
        ),
        Some((
            applicability,
            &format!("Change to `{expectation}()`"),
            replacement,
        )),
    );
}

/// `stop()`, `warning()` and `message()` join their arguments with no separator, so a
/// `paste0()` inside them is redundant, and it hides each piece from translation, which
/// works argument by argument. The fix is unsafe because `paste0()` recycles vectors
/// element-wise while the condition joins them end to end.
fn condition_message_lint(
    callee: &str,
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for value in arguments
        .iter()
        .filter(|argument| argument.name.is_none())
        .filter_map(|argument| argument.value.as_ref())
        .filter(|value| value.kind() == SyntaxKind::CALL_EXPR)
    {
        let Some(paste) = bare_callee(value) else {
            continue;
        };
        let pasted = call_arguments(value);
        let named = |wanted: &str| {
            pasted.iter().find(|argument| {
                argument
                    .name
                    .as_ref()
                    .is_some_and(|(name, _)| name == wanted)
            })
        };
        let separator = named("sep");
        let joins = match paste.as_str() {
            "paste0" => separator.is_none(),
            "paste" => separator
                .and_then(|argument| argument.value.as_ref())
                .is_some_and(|value| matches!(&*node_text(value), "\"\"" | "''")),
            _ => false,
        };
        if !joins || named("collapse").is_some() || named("recycle0").is_some() {
            continue;
        }
        let rest: Vec<String> = pasted
            .iter()
            .filter(|argument| separator.is_none_or(|separator| separator.node != argument.node))
            .map(|argument| significant_text(&argument.node))
            .collect();
        let has_comment = value
            .descendants_with_tokens()
            .any(|element| element.kind() == SyntaxKind::COMMENT);
        let fix = (!has_comment && !rest.is_empty()).then(|| {
            (
                Applicability::Unsafe,
                "Pass the pieces directly",
                rest.join(", "),
            )
        });
        push_lint(
            diagnostics,
            opt_in(config.condition_message),
            Severity::Warning,
            "condition-message",
            significant_range(value),
            format!("{callee}() joins its arguments itself, so {paste}() is not needed"),
            fix,
        );
    }
}

/// A pattern with no regular expression syntax matches the same text as a fixed string, and
/// `fixed = TRUE` skips compiling and running a regex. Calls that set `fixed`, `perl` or
/// `ignore.case` (which `fixed` does not support) are left alone, and so are `sub()` and
/// `gsub()` with a replacement that may hold backreferences. The fix is unsafe because R may
/// mark the encoding of a fixed match's result differently: DBI quotes identifiers without
/// `fixed = TRUE` for that reason.
fn fixed_regex_lint(
    call: &SyntaxNode,
    callee: &str,
    arguments: &[CallArgument],
    config: &LintConfig,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let formals: &[&str] = match callee {
        "grepl" => &["pattern", "x", "ignore.case", "perl", "fixed", "useBytes"],
        "grep" => &[
            "pattern",
            "x",
            "ignore.case",
            "perl",
            "value",
            "fixed",
            "useBytes",
            "invert",
        ],
        "sub" | "gsub" => &[
            "pattern",
            "replacement",
            "x",
            "ignore.case",
            "perl",
            "fixed",
            "useBytes",
        ],
        "strsplit" => &["x", "split", "fixed", "perl", "useBytes"],
        _ => &[
            "pattern",
            "text",
            "ignore.case",
            "perl",
            "fixed",
            "useBytes",
        ],
    };
    let Some(matched) = match_formals(arguments, formals) else {
        return;
    };
    let value_of = |formal: &str| {
        formals
            .iter()
            .position(|candidate| *candidate == formal)
            .and_then(|index| matched.get(index).copied().flatten())
    };
    if ["fixed", "perl", "ignore.case"]
        .iter()
        .any(|formal| value_of(formal).is_some())
    {
        return;
    }
    let plain_literal = |node: &SyntaxNode| {
        node.kind() == SyntaxKind::LITERAL
            && node.first_token().is_some_and(|token| {
                token.kind() == SyntaxKind::STRING && {
                    let text = token.text();
                    let inner = text.get(1..text.len().saturating_sub(1)).unwrap_or("");
                    !inner.is_empty()
                        && !inner.contains(|character: char| {
                            matches!(
                                character,
                                '\\' | '.'
                                    | '|'
                                    | '('
                                    | ')'
                                    | '['
                                    | ']'
                                    | '{'
                                    | '}'
                                    | '^'
                                    | '$'
                                    | '*'
                                    | '+'
                                    | '?'
                            )
                        })
                }
            })
    };
    let pattern_formal = if callee == "strsplit" {
        "split"
    } else {
        "pattern"
    };
    let Some(pattern) = value_of(pattern_formal).filter(|pattern| plain_literal(pattern)) else {
        return;
    };
    if matches!(callee, "sub" | "gsub")
        && !value_of("replacement").is_some_and(|replacement| {
            replacement.kind() == SyntaxKind::LITERAL
                && replacement.first_token().is_some_and(|token| {
                    token.kind() == SyntaxKind::STRING && !token.text().contains('\\')
                })
        })
    {
        return;
    }
    let Some(severity) = lint_severity(opt_in(config.fixed_regex), None) else {
        return;
    };
    let closing = call
        .children()
        .find(|child| child.kind() == SyntaxKind::ARGUMENT_LIST)
        .and_then(|list| list.last_token())
        .filter(|token| token.kind() == SyntaxKind::R_PAREN);
    let before_closing = closing
        .as_ref()
        .and_then(|token| {
            std::iter::successors(token.prev_token(), |token| token.prev_token())
                .find(|token| !token.kind().is_trivia())
        })
        .map(|token| token.kind());
    let diagnostic = Diagnostic::new(
        significant_range(pattern),
        severity,
        "fixed-regex",
        format!(
            "This pattern has no regular expression syntax, so {callee}() matches it faster with `fixed = TRUE`"
        ),
    );
    diagnostics.push(match closing {
        Some(closing) if before_closing != Some(SyntaxKind::COMMA) => diagnostic.with_fix(
            Applicability::Unsafe,
            "Add `fixed = TRUE`".to_owned(),
            TextRange::empty(closing.text_range().start()),
            ", fixed = TRUE".to_owned(),
        ),
        _ => diagnostic,
    });
}

/// R's argument matching for exact names and positions: each named argument takes its formal,
/// and the positional ones fill the remaining formals in order. `None` when an argument is
/// `...`, its name is not exactly a formal (R would try a partial match), or there are more
/// arguments than formals.
fn match_formals<'a>(
    arguments: &'a [CallArgument],
    formals: &[&str],
) -> Option<Vec<Option<&'a SyntaxNode>>> {
    let mut matched: Vec<Option<&SyntaxNode>> = vec![None; formals.len()];
    for argument in arguments {
        let value = argument.value.as_ref()?;
        if value.kind() == SyntaxKind::NAME && value.text() == "..." {
            return None;
        }
        if let Some((name, _)) = &argument.name {
            let index = formals.iter().position(|formal| formal == name)?;
            *matched.get_mut(index)? = Some(value);
        }
    }
    let mut free = (0..formals.len())
        .filter(|index| matched[*index].is_none())
        .collect::<Vec<_>>()
        .into_iter();
    for argument in arguments.iter().filter(|argument| argument.name.is_none()) {
        let index = free.next()?;
        *matched.get_mut(index)? = argument.value.as_ref();
    }
    Some(matched)
}

/// Whether `node` sits inside `quote()`, `bquote()`, `substitute()` or `expression()`, which
/// build code to run elsewhere, such as a debugging helper that injects a `browser()` call.
fn is_quoted(node: &SyntaxNode) -> bool {
    node.ancestors().skip(1).any(|ancestor| {
        ancestor.kind() == SyntaxKind::CALL_EXPR
            && matches!(
                bare_callee(&ancestor).as_deref(),
                Some("quote" | "bquote" | "substitute" | "expression")
            )
    })
}

/// The operand of a `!x` expression.
fn negated_operand(node: &SyntaxNode) -> Option<SyntaxNode> {
    let negates = node.kind() == SyntaxKind::UNARY_EXPR
        && node
            .children_with_tokens()
            .find(|element| !element.kind().is_trivia())
            .is_some_and(|element| element.kind() == SyntaxKind::BANG);
    negates.then(|| node.first_child()).flatten()
}

/// The text that replaces `node`, in parentheses unless `node` sits where nothing binds to
/// it. A replacement such as `!any(x)` binds more loosely than `+`, so `all(!x) + 1` must
/// become `(!any(x)) + 1`.
fn in_place_of(node: &SyntaxNode, replacement: String) -> String {
    let stands_alone = node.parent().is_none_or(|parent| match parent.kind() {
        SyntaxKind::ARGUMENT
        | SyntaxKind::PAREN_EXPR
        | SyntaxKind::BRACE_EXPR
        | SyntaxKind::SOURCE_FILE
        | SyntaxKind::IF_EXPR
        | SyntaxKind::WHILE_EXPR
        | SyntaxKind::FOR_EXPR
        | SyntaxKind::REPEAT_EXPR
        | SyntaxKind::FUNCTION_DEF => true,
        SyntaxKind::BINARY_EXPR => {
            let binary = BinaryExpr::cast(parent.clone());
            binary
                .as_ref()
                .and_then(BinaryExpr::operator)
                .is_some_and(|operator| {
                    matches!(
                        operator.kind(),
                        SyntaxKind::LESS_MINUS | SyntaxKind::LESS2_MINUS | SyntaxKind::EQ
                    )
                })
                && binary.and_then(|binary| binary.rhs()).as_ref() == Some(node)
        }
        _ => false,
    });
    if stands_alone {
        replacement
    } else {
        format!("({replacement})")
    }
}

fn boolean_literal(node: &SyntaxNode) -> Option<bool> {
    (node.kind() == SyntaxKind::LITERAL)
        .then(|| node.first_token())
        .flatten()
        .and_then(|token| match token.kind() {
            SyntaxKind::TRUE_KW => Some(true),
            SyntaxKind::FALSE_KW => Some(false),
            _ => None,
        })
}

/// A literal number, string or logical, possibly negated, such as `2`, `"a"` or `-1`.
fn has_comment(node: &SyntaxNode) -> bool {
    node.descendants_with_tokens()
        .any(|element| element.kind() == SyntaxKind::COMMENT)
}

fn is_constant(node: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::LITERAL => true,
        SyntaxKind::UNARY_EXPR => {
            node.children_with_tokens()
                .find(|element| !element.kind().is_trivia())
                .is_some_and(|element| element.kind() == SyntaxKind::MINUS)
                && node
                    .first_child()
                    .is_some_and(|operand| is_constant(&operand))
        }
        _ => false,
    }
}

fn is_null_literal(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::LITERAL
        && node
            .first_token()
            .is_some_and(|token| token.kind() == SyntaxKind::NULL_KW)
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
    node_text(node)[range].to_owned()
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
