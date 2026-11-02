//! Refactoring actions: extract a variable or a function, inline a variable, and move a type
//! annotation between its `#:` comment and its inline spelling. Each is a rewrite of the syntax
//! that is offered only where it keeps every name resolving and the code parsing;
//! `testing::check_refactor_invariants` pins that property on sampled selections.

use {
    crate::{
        CodeAction, CodeActionKind, TextEdit, child_node, child_token, global_declaration_exists,
        references, removal_range,
    },
    semantics::{
        Db, ProjectFiles, SourceFile, item_hir, item_naming, item_node, item_tree,
        naming::BindingKind, stubs::stub_declaration,
    },
    std::collections::HashSet,
    syntax::{
        Direction, SyntaxElement,
        SyntaxKind::{self, *},
        SyntaxNode, SyntaxToken, TextRange, TextSize,
        ast::BinaryExpr,
        infix_binding_power,
    },
};

/// The refactorings available for `selection`: the extractions need a non-empty selection, and
/// the others read the name or statement at its start.
pub fn refactors(db: &dyn Db, file: SourceFile, selection: TextRange) -> Vec<CodeAction> {
    let root = semantics::parse(db, file).syntax_node();
    let text = file.text(db);
    let mut actions = Vec::new();
    let trimmed = trim_selection(text, selection);
    if !trimmed.is_empty() {
        actions.extend(extract_variable(db, file, &root, text, trimmed));
        actions.extend(extract_function(db, file, &root, text, trimmed));
    }
    actions.extend(inline_variable(db, file, &root, text, selection.start()));
    actions.extend(convert_annotation(&root, text, selection.start()));
    actions
}

fn extract_variable(
    db: &dyn Db,
    file: SourceFile,
    root: &SyntaxNode,
    text: &str,
    range: TextRange,
) -> Option<CodeAction> {
    let expression = expression_at(root, range)?;
    let statement = enclosing_statement(&expression)?;
    if statement == expression || is_assignment(&expression) || is_captured(&statement) {
        return None;
    }
    if !evaluated_first(&expression, &statement, has_call(&expression)) {
        return None;
    }
    if has_quiet_reads(db, file, range) {
        return None;
    }
    let indent = line_indent(text, statement.text_range().start())?;
    // Above the statement's `#:` annotation, which keeps typing the statement.
    let insert_at = leading_comments_start(&statement);
    let name = fresh_name(db, root, "value");
    let value = slice(text, range);
    let definition = if needs_grouping(&expression, value) {
        format!("{name} <- ({value})\n{indent}")
    } else {
        format!("{name} <- {value}\n{indent}")
    };
    // Same-offset insertion and replacement may apply in either order, so they become one edit.
    let edits = if insert_at == range.start() {
        vec![TextEdit {
            range,
            replacement: format!("{definition}{name}"),
        }]
    } else {
        vec![
            TextEdit {
                range: TextRange::empty(insert_at),
                replacement: definition,
            },
            TextEdit {
                range,
                replacement: name,
            },
        ]
    };
    Some(CodeAction {
        title: "Extract into variable".to_owned(),
        kind: CodeActionKind::ExtractVariable,
        edits,
    })
}

/// What an extracted function replaces: whole statements of one block, or one expression.
enum Extraction {
    Statements(Vec<SyntaxNode>),
    Expression(SyntaxNode),
}

fn extract_function(
    db: &dyn Db,
    file: SourceFile,
    root: &SyntaxNode,
    text: &str,
    range: TextRange,
) -> Option<CodeAction> {
    let extraction = match statements_in(root, range) {
        Some(statements) => Extraction::Statements(statements),
        None => {
            let expression = expression_at(root, range)?;
            if enclosing_statement(&expression).as_ref() == Some(&expression)
                || in_annotation_or_pattern(&expression)
                || is_assignment(&expression)
                || assignment_target_of(&expression)
            {
                return None;
            }
            Extraction::Expression(expression)
        }
    };
    let nodes: Vec<SyntaxNode> = match &extraction {
        Extraction::Statements(statements) => statements.clone(),
        Extraction::Expression(expression) => vec![expression.clone()],
    };
    if nodes
        .iter()
        .any(|node| !can_move_into_function(node) || is_captured(node))
        || has_quiet_reads(db, file, range)
    {
        return None;
    }
    let inside_function = nodes
        .first()?
        .ancestors()
        .any(|ancestor| ancestor.kind() == FUNCTION_DEF);
    let assigned = assigned_names(&nodes);
    // A top-level assignment defines a global, which a function would make local.
    if !inside_function && !assigned.is_empty() {
        return None;
    }
    // Inside a loop, code after the selection runs again before it on the next pass, so a read
    // anywhere in the outermost loop of the function sees what the selection assigns.
    let enclosing_loop = nodes
        .first()?
        .ancestors()
        .skip(1)
        .take_while(|ancestor| ancestor.kind() != FUNCTION_DEF)
        .filter(|ancestor| matches!(ancestor.kind(), FOR_EXPR | WHILE_EXPR | REPEAT_EXPR))
        .last()
        .map(|ancestor| ancestor.text_range());
    let mut parameters: Vec<(TextSize, String)> = Vec::new();
    let mut outputs: Vec<String> = Vec::new();
    for item in item_tree(db, file) {
        let (Some(node), Some(hir), Some(naming)) = (
            item_node(db, *item),
            item_hir(db, *item),
            item_naming(db, *item),
        ) else {
            continue;
        };
        if !ranges_intersect(node.text_range(), range) {
            continue;
        }
        let offset = node.text_range().start();
        for (expression, binding) in naming.resolutions.iter() {
            let Some(info) = naming.bindings.get(binding) else {
                continue;
            };
            if info.kind == BindingKind::TopLevel {
                continue;
            }
            let read = hir.range(*expression) + offset;
            let name = info.name.as_str();
            if range.contains_range(read) && !range.contains_range(info.range + offset) {
                if !parameters.iter().any(|(_, known)| known == name) {
                    parameters.push((read.start(), name.to_owned()));
                }
            } else if !range.contains_range(read)
                && (read.start() >= range.end()
                    || enclosing_loop.is_some_and(|loop_range| loop_range.contains_range(read)))
                && assigned.contains(name)
                && !outputs.iter().any(|known| known == name)
            {
                outputs.push(name.to_owned());
            }
        }
    }
    let output = match (&extraction, outputs.as_slice()) {
        (_, []) => None,
        (Extraction::Statements(_), [single]) => Some(single.clone()),
        _ => return None,
    };
    parameters.sort();
    let arguments: Vec<&str> = parameters.iter().map(|(_, name)| name.as_str()).collect();
    let arguments = arguments.join(", ");
    let name = fresh_name(db, root, "extracted");
    let first = nodes.first()?;
    let base_indent = line_indent(text, first.text_range().start()).unwrap_or_default();
    let body = reindent(root, text, range, &base_indent);
    let definition = match (&extraction, &output) {
        (Extraction::Expression(expression), _)
            if needs_grouping(expression, slice(text, range)) =>
        {
            format!("{name} <- function({arguments}) (\n{body}\n)\n\n")
        }
        (Extraction::Expression(_), _) if !body.contains('\n') => {
            format!("{name} <- function({arguments}) {}\n\n", body.trim_start())
        }
        (_, Some(output)) => {
            format!("{name} <- function({arguments}) {{\n{body}\n  {output}\n}}\n\n")
        }
        (_, None) => format!("{name} <- function({arguments}) {{\n{body}\n}}\n\n"),
    };
    let call = match &output {
        Some(output) => format!("{output} <- {name}({arguments})"),
        None => format!("{name}({arguments})"),
    };
    let top_level = root
        .children()
        .find(|child| child.text_range().contains_range(range))?;
    let insert_at = leading_comments_start(&top_level);
    // An editor may apply an insertion and a replacement that start at the same offset in
    // either order, so a selection that begins its top-level statement becomes one edit.
    let edits = if insert_at == range.start() {
        vec![TextEdit {
            range,
            replacement: format!("{definition}{call}"),
        }]
    } else {
        vec![
            TextEdit {
                range: TextRange::empty(insert_at),
                replacement: definition,
            },
            TextEdit {
                range,
                replacement: call,
            },
        ]
    };
    Some(CodeAction {
        title: "Extract into function".to_owned(),
        kind: CodeActionKind::ExtractFunction,
        edits,
    })
}

fn inline_variable(
    db: &dyn Db,
    file: SourceFile,
    root: &SyntaxNode,
    text: &str,
    offset: TextSize,
) -> Option<CodeAction> {
    let files = ProjectFiles::try_get(db)?;
    let name_token = ident_at(root, offset)?;
    let occurrences = references(db, files, file, name_token.text_range().start(), true);
    if occurrences.iter().any(|occurrence| occurrence.file != file) {
        return None;
    }
    let mut declarations = occurrences
        .iter()
        .filter(|occurrence| occurrence.is_declaration);
    let declaration = declarations.next()?;
    if declarations.next().is_some() {
        return None;
    }
    let declared = name_node_at(root, declaration.range)?;
    let statement = declared.parent()?;
    let binary = BinaryExpr::cast(statement.clone())?;
    let operator = binary.operator()?.kind();
    if !matches!(operator, LESS_MINUS | EQ)
        || binary.lhs().as_ref() != Some(&declared)
        || !statement
            .parent()
            .is_some_and(|parent| is_statement_container(parent.kind()))
        || child_node(&statement, TYPE_ASCRIPTION).is_some()
        || previous_node_is_annotation(&statement)
    {
        return None;
    }
    let value = binary.rhs()?;
    let reads: Vec<SyntaxNode> = occurrences
        .iter()
        .filter(|occurrence| !occurrence.is_declaration)
        .map(|occurrence| name_node_at(root, occurrence.range))
        .collect::<Option<_>>()?;
    let last_read = reads.iter().map(|read| read.text_range().end()).max()?;
    let is_literal = value.kind() == LITERAL;
    // An assignment in the value (`x <- y <- 0`) would move after the reads of what it assigns.
    if value.descendants().any(|node| is_assignment(&node)) {
        return None;
    }
    // A call may have effects, so it must still run exactly once and before anything else
    // runs: its single read is evaluated first in the very next statement.
    if has_call(&value) {
        let next = statement.next_sibling()?;
        let [read] = reads.as_slice() else {
            return None;
        };
        if !next.text_range().contains_range(read.text_range())
            || !evaluated_first(read, &next, true)
        {
            return None;
        }
    }
    for read in &reads {
        if read.text_range().start() < statement.text_range().end()
            || assignment_target_of(read)
            || in_annotation_or_pattern(read)
            || is_captured(read)
        {
            return None;
        }
        // A read in a loop or function the definition is not in would evaluate the value at a
        // different time, or more than once.
        let deferred = read.ancestors().any(|ancestor| {
            matches!(
                ancestor.kind(),
                FUNCTION_DEF | FOR_EXPR | WHILE_EXPR | REPEAT_EXPR
            ) && !ancestor.text_range().contains_range(statement.text_range())
        });
        if deferred && !is_literal {
            return None;
        }
    }
    let value_names: HashSet<String> = value
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| token.kind() == IDENT)
        .map(|token| token.text().to_owned())
        .collect();
    let between = TextRange::new(statement.text_range().end(), last_read);
    if rebinds_any(root, &value_names, between) {
        return None;
    }
    let value_text = slice(text, value.text_range());
    let mut edits = vec![TextEdit {
        range: removal_range(text, statement.text_range()),
        replacement: String::new(),
    }];
    for read in &reads {
        let replacement = if needs_parentheses(&value, read) {
            format!("({value_text})")
        } else {
            value_text.to_owned()
        };
        edits.push(TextEdit {
            range: read.text_range(),
            replacement,
        });
    }
    Some(CodeAction {
        title: format!("Inline variable `{}`", declared.text()),
        kind: CodeActionKind::InlineVariable,
        edits,
    })
}

fn convert_annotation(root: &SyntaxNode, text: &str, offset: TextSize) -> Option<CodeAction> {
    let at = root.token_at_offset(offset);
    let token = at
        .clone()
        .right_biased()
        .filter(|token| !token.kind().is_trivia())
        .or_else(|| at.left_biased())?;
    let top = token.parent_ancestors().find(|ancestor| {
        ancestor
            .parent()
            .is_some_and(|parent| is_statement_container(parent.kind()))
    })?;
    let (annotation, statement) = if top.kind() == ANNOTATION {
        let statement = next_statement(&top)?;
        (Some(top), statement)
    } else {
        let annotation = previous_node_is_annotation(&top)
            .then(|| top.prev_sibling())
            .flatten();
        (annotation, top)
    };
    let binary = BinaryExpr::cast(statement.clone())?;
    let target = binary.lhs().filter(|lhs| lhs.kind() == NAME)?;
    if !matches!(binary.operator()?.kind(), LESS_MINUS | EQ) {
        return None;
    }
    let value = binary.rhs()?;
    let function = (value.kind() == FUNCTION_DEF).then_some(value);
    match annotation {
        Some(annotation) => comment_to_inline(text, &annotation, &statement, &target, function),
        None => inline_to_comment(text, &statement, &target, function),
    }
}

fn comment_to_inline(
    text: &str,
    annotation: &SyntaxNode,
    statement: &SyntaxNode,
    target: &SyntaxNode,
    function: Option<SyntaxNode>,
) -> Option<CodeAction> {
    if slice(text, annotation.text_range()).contains('\n') || has_inline_types(statement, &function)
    {
        return None;
    }
    let marker = child_token(annotation, ANNOTATION_MARKER)?;
    let body: Vec<SyntaxNode> = annotation.children().collect();
    let mut edits = vec![TextEdit {
        range: removal_range(text, annotation.text_range()),
        replacement: String::new(),
    }];
    let signature = body.iter().find(|node| node.kind() == TYPE_FUNCTION);
    match (function, signature) {
        (Some(function), Some(signature)) => {
            if body
                .iter()
                .any(|node| !matches!(node.kind(), TYPE_FUNCTION | TYPE_BINDER_LIST))
            {
                return None;
            }
            let typed: Vec<(String, String)> = child_node(signature, TYPE_PARAMETER_LIST)?
                .children()
                .filter(|child| child.kind() == TYPE_PARAMETER)
                .map(|parameter| {
                    let name = parameter_name(&parameter)?;
                    let colon = child_token(&parameter, COLON)?;
                    let type_text = slice(
                        text,
                        TextRange::new(colon.text_range().end(), parameter.text_range().end()),
                    );
                    Some((name, type_text.trim().to_owned()))
                })
                .collect::<Option<_>>()?;
            let parameters: Vec<SyntaxNode> = child_node(&function, PARAMETER_LIST)?
                .children()
                .filter(|child| child.kind() == PARAMETER)
                .collect();
            if parameters.len() != typed.len() {
                return None;
            }
            for parameter in &parameters {
                let name_token = parameter_name_token(parameter)?;
                let (_, type_text) = typed.iter().find(|(name, _)| name == name_token.text())?;
                edits.push(TextEdit {
                    range: TextRange::empty(name_token.text_range().end()),
                    replacement: format!(": {type_text}"),
                });
            }
            if let Some(binders) = body.iter().find(|node| node.kind() == TYPE_BINDER_LIST) {
                let keyword = child_token(&function, FUNCTION_KW)?;
                edits.push(TextEdit {
                    range: TextRange::empty(keyword.text_range().end()),
                    replacement: slice(text, binders.text_range()).to_owned(),
                });
            }
            if let Some(arrow) = child_token(signature, MINUS_GREATER) {
                let result = slice(
                    text,
                    TextRange::new(arrow.text_range().end(), signature.text_range().end()),
                );
                let list = child_node(&function, PARAMETER_LIST)?;
                edits.push(TextEdit {
                    range: TextRange::empty(list.text_range().end()),
                    replacement: format!(" -> {}", result.trim()),
                });
            }
        }
        _ => {
            let directives_allowed = body
                .iter()
                .filter(|node| node.kind() == ANNOTATION_DIRECTIVE)
                .all(|directive| {
                    child_token(directive, IDENT).is_some_and(|name| {
                        matches!(name.text(), "trust" | "if-unknown" | "if" | "new")
                    })
                });
            if !directives_allowed || body.iter().any(|node| node.kind() == TYPE_BINDER_LIST) {
                return None;
            }
            let type_text = slice(
                text,
                TextRange::new(marker.text_range().end(), annotation.text_range().end()),
            )
            .trim();
            if type_text.is_empty() {
                return None;
            }
            edits.push(TextEdit {
                range: TextRange::empty(target.text_range().end()),
                replacement: format!(": {type_text}"),
            });
        }
    }
    if statement.text_range().start() < annotation.text_range().end() {
        return None;
    }
    Some(CodeAction {
        title: "Convert to inline annotation".to_owned(),
        kind: CodeActionKind::ConvertAnnotation,
        edits,
    })
}

fn inline_to_comment(
    text: &str,
    statement: &SyntaxNode,
    target: &SyntaxNode,
    function: Option<SyntaxNode>,
) -> Option<CodeAction> {
    let indent = line_indent(text, statement.text_range().start())?;
    let ascription = child_node(statement, TYPE_ASCRIPTION);
    let function = function.filter(|function| has_inline_types(statement, &Some(function.clone())));
    let (comment, mut edits) = match (ascription, function) {
        (Some(_), Some(_)) | (None, None) => return None,
        (Some(ascription), None) => {
            let colon = child_token(&ascription, COLON)?;
            let type_text = slice(
                text,
                TextRange::new(colon.text_range().end(), ascription.text_range().end()),
            );
            (
                type_text.trim().to_owned(),
                vec![TextEdit {
                    range: TextRange::new(target.text_range().end(), ascription.text_range().end()),
                    replacement: String::new(),
                }],
            )
        }
        (None, Some(function)) => {
            if child_token(&function, AT).is_some()
                || child_node(&function, ANNOTATION_DIRECTIVE).is_some()
            {
                return None;
            }
            let list = child_node(&function, PARAMETER_LIST)?;
            let mut edits = Vec::new();
            let mut parameters = Vec::new();
            for parameter in list.children().filter(|child| child.kind() == PARAMETER) {
                let name = parameter_name_token(&parameter)?;
                let ascription = child_node(&parameter, TYPE_ASCRIPTION)?;
                let colon = child_token(&ascription, COLON)?;
                let type_text = slice(
                    text,
                    TextRange::new(colon.text_range().end(), ascription.text_range().end()),
                )
                .trim();
                let optional = child_token(&parameter, EQ).is_some();
                parameters.push(if optional {
                    format!("[{}]: {type_text}", name.text())
                } else {
                    format!("{}: {type_text}", name.text())
                });
                edits.push(TextEdit {
                    range: TextRange::new(name.text_range().end(), ascription.text_range().end()),
                    replacement: String::new(),
                });
            }
            let binders = child_node(&function, TYPE_BINDER_LIST);
            let mut comment = String::new();
            if let Some(binders) = &binders {
                comment.push_str(slice(text, binders.text_range()));
                comment.push(' ');
                edits.push(TextEdit {
                    range: binders.text_range(),
                    replacement: String::new(),
                });
            }
            comment.push_str(&format!("fn({})", parameters.join(", ")));
            if let Some(result) = child_node(&function, RETURN_TYPE) {
                let arrow = child_token(&result, MINUS_GREATER)?;
                let result_text = slice(
                    text,
                    TextRange::new(arrow.text_range().end(), result.text_range().end()),
                );
                comment.push_str(&format!(" -> {}", result_text.trim()));
                edits.push(TextEdit {
                    range: TextRange::new(list.text_range().end(), result.text_range().end()),
                    replacement: String::new(),
                });
            }
            (comment, edits)
        }
    };
    edits.insert(
        0,
        TextEdit {
            range: TextRange::empty(statement.text_range().start()),
            replacement: format!("#: {comment}\n{indent}"),
        },
    );
    Some(CodeAction {
        title: "Convert to `#:` annotation".to_owned(),
        kind: CodeActionKind::ConvertAnnotation,
        edits,
    })
}

fn has_inline_types(statement: &SyntaxNode, function: &Option<SyntaxNode>) -> bool {
    child_node(statement, TYPE_ASCRIPTION).is_some()
        || function.as_ref().is_some_and(|function| {
            child_node(function, TYPE_BINDER_LIST).is_some()
                || child_node(function, RETURN_TYPE).is_some()
                || child_node(function, PARAMETER_LIST).is_some_and(|list| {
                    list.children()
                        .any(|parameter| child_node(&parameter, TYPE_ASCRIPTION).is_some())
                })
        })
}

fn parameter_name_token(parameter: &SyntaxNode) -> Option<SyntaxToken> {
    parameter
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find(|token| matches!(token.kind(), IDENT | DOTS))
}

/// The name of a `#:` signature's parameter, without the brackets of an optional one.
fn parameter_name(parameter: &SyntaxNode) -> Option<String> {
    if let Some(name) = child_node(parameter, NAME) {
        return Some(name.text().to_string());
    }
    child_token(parameter, DOTS).map(|dots| dots.text().to_owned())
}

fn trim_selection(text: &str, selection: TextRange) -> TextRange {
    let start = usize::from(selection.start()).min(text.len());
    let end = usize::from(selection.end()).min(text.len()).max(start);
    let Some(selected) = text.get(start..end) else {
        return TextRange::empty(selection.start());
    };
    let leading = selected.len() - selected.trim_start().len();
    let trimmed = selected.trim().len();
    TextRange::at(
        TextSize::from((start + leading) as u32),
        TextSize::from(trimmed as u32),
    )
}

fn slice(text: &str, range: TextRange) -> &str {
    text.get(usize::from(range.start())..usize::from(range.end()))
        .unwrap_or_default()
}

fn is_statement_container(kind: SyntaxKind) -> bool {
    matches!(kind, SOURCE_FILE | BRACE_EXPR)
}

fn is_expression(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        NAME | LITERAL
            | BINARY_EXPR
            | UNARY_EXPR
            | PAREN_EXPR
            | BRACE_EXPR
            | CALL_EXPR
            | SUBSET_EXPR
            | SUBSET2_EXPR
            | DOLLAR_EXPR
            | AT_EXPR
            | NAMESPACE_EXPR
            | FUNCTION_DEF
            | IF_EXPR
            | FOR_EXPR
            | WHILE_EXPR
            | REPEAT_EXPR
            | TUPLE_EXPR
            | RECORD_EXPR
            | ARRAY_EXPR
            | TAG_EXPR
            | MATCH_EXPR
            | NEW_EXPR
    )
}

/// The expression spanning exactly `range`, in a place that holds a value: not a `$` field, a
/// half of `pkg::name`, an argument's name, a parameter, a loop variable, or type syntax. A bare
/// name is never one, because it is already a variable.
fn expression_at(root: &SyntaxNode, range: TextRange) -> Option<SyntaxNode> {
    let start = match root.covering_element(range) {
        SyntaxElement::Node(node) => node,
        SyntaxElement::Token(token) => token.parent()?,
    };
    let expression = start
        .ancestors()
        .take_while(|node| node.text_range() == range)
        .filter(|node| is_expression(node.kind()))
        .last()
        .filter(|expression| expression.kind() != NAME)?;
    let parent = expression.parent()?;
    let is_first = parent.first_child().as_ref() == Some(&expression);
    let is_value = match parent.kind() {
        DOLLAR_EXPR | AT_EXPR => is_first,
        FOR_EXPR => !is_first,
        NAMESPACE_EXPR | PARAMETER => false,
        ARGUMENT => !(expression.kind() == NAME && child_token(&parent, EQ).is_some() && is_first),
        _ => true,
    };
    (is_value && !in_annotation_or_pattern(&expression)).then_some(expression)
}

/// Whole statements of one block that `range` spans exactly.
fn statements_in(root: &SyntaxNode, range: TextRange) -> Option<Vec<SyntaxNode>> {
    let start = match root.covering_element(range) {
        SyntaxElement::Node(node) => node,
        SyntaxElement::Token(token) => token.parent()?,
    };
    let container = start
        .ancestors()
        .find(|node| is_statement_container(node.kind()))?;
    let statements: Vec<SyntaxNode> = container
        .children()
        .filter(|child| range.contains_range(child.text_range()))
        .collect();
    let first = statements.first()?;
    let last = statements.last()?;
    (first.text_range().start() == range.start()
        && last.text_range().end() == range.end()
        && statements
            .iter()
            .all(|statement| statement.kind() != ANNOTATION))
    .then_some(statements)
}

fn enclosing_statement(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors().find(|ancestor| {
        ancestor
            .parent()
            .is_some_and(|parent| is_statement_container(parent.kind()))
    })
}

fn in_annotation_or_pattern(node: &SyntaxNode) -> bool {
    node.ancestors().any(|ancestor| {
        matches!(
            ancestor.kind(),
            ANNOTATION
                | TYPE_ASCRIPTION
                | RETURN_TYPE
                | TYPE_DEFINITION
                | PARAMETER_LIST
                | MATCH_GUARD
                | PATTERN_WILDCARD
                | PATTERN_BIND
                | PATTERN_TAG
                | PATTERN_FIELD
                | PATTERN_LIST
        )
    })
}

fn is_assignment(node: &SyntaxNode) -> bool {
    BinaryExpr::cast(node.clone())
        .and_then(|binary| binary.operator())
        .is_some_and(|operator| is_assignment_operator(operator.kind()))
}

fn is_assignment_operator(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        LESS_MINUS | LESS2_MINUS | MINUS_GREATER | MINUS_GREATER2 | EQ | COLON_EQ
    )
}

/// Whether `child` is the side of the assignment `parent` that receives the value.
fn is_assignment_target(parent: &SyntaxNode, child: &SyntaxNode) -> bool {
    let Some(binary) = BinaryExpr::cast(parent.clone()) else {
        return false;
    };
    match binary.operator().map(|operator| operator.kind()) {
        Some(LESS_MINUS | LESS2_MINUS | EQ | COLON_EQ) => binary.lhs().as_ref() == Some(child),
        Some(MINUS_GREATER | MINUS_GREATER2) => binary.rhs().as_ref() == Some(child),
        _ => false,
    }
}

/// Whether `node` is, or is inside, the receiving side of an assignment (`x` in `x$a <- 1`).
fn assignment_target_of(node: &SyntaxNode) -> bool {
    let mut child = node.clone();
    for parent in node.ancestors().skip(1) {
        if is_assignment_target(&parent, &child) {
            return true;
        }
        if parent
            .parent()
            .is_some_and(|grandparent| is_statement_container(grandparent.kind()))
        {
            return false;
        }
        child = parent;
    }
    false
}

/// `%>%` and its kin take their right side unevaluated.
fn is_pipe_special(binary: &SyntaxNode) -> bool {
    child_token(binary, SPECIAL)
        .is_some_and(|special| matches!(special.text(), "%>%" | "%<>%" | "%T>%" | "%$%"))
}

fn callee_name(call: &SyntaxNode) -> Option<String> {
    let callee = call.first_child()?;
    match callee.kind() {
        NAME => Some(callee.text().to_string()),
        NAMESPACE_EXPR => callee
            .children()
            .filter(|child| child.kind() == NAME)
            .last()
            .map(|name| name.text().to_string()),
        _ => None,
    }
}

/// How a call evaluates its arguments when that is not once each, in order, in the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgumentEvaluation {
    /// Taken as code: quoted, or evaluated in a data mask where a name may mean a column.
    Captured,
    /// Evaluated later, maybe not at all, or under a condition handler or changed state that
    /// code moved out of the call would escape.
    Guarded,
}

fn argument_evaluation(call: &SyntaxNode) -> Option<ArgumentEvaluation> {
    let name = callee_name(call)?;
    match name.as_str() {
        "quote" | "bquote" | "substitute" | "expression" | "alist" | "enquote" | "with"
        | "within" | "subset" | "transform" | "local" | "eval" | "evalq" | "filter" | "mutate"
        | "summarise" | "summarize" | "select" | "arrange" | "group_by" | "transmute"
        | "rename" | "reframe" | "across" | "test_that" | "expect_snapshot" => {
            Some(ArgumentEvaluation::Captured)
        }
        "tryCatch"
        | "try"
        | "withCallingHandlers"
        | "suppressWarnings"
        | "suppressMessages"
        | "suppressPackageStartupMessages"
        | "system.time"
        | "switch"
        | "on.exit"
        | "capture.output"
        | "invisible"
        | "expect_error"
        | "expect_warning"
        | "expect_message"
        | "expect_condition"
        | "expect_output"
        | "expect_silent"
        | "expect_no_error"
        | "expect_no_warning"
        | "expect_no_message" => Some(ArgumentEvaluation::Guarded),
        name if name.starts_with("with_") || name.starts_with("local_") => {
            Some(ArgumentEvaluation::Guarded)
        }
        _ => None,
    }
}

/// Whether evaluating `expression` once, just before `statement`, is what running `statement`
/// would do: every node between them evaluates it exactly once, and nothing that runs before it
/// inside `statement` assigns, or calls when `moves_call` (two calls may not trade places).
fn evaluated_first(expression: &SyntaxNode, statement: &SyntaxNode, moves_call: bool) -> bool {
    let mut child = expression.clone();
    for parent in expression.ancestors().skip(1) {
        let is_first = parent.first_child().as_ref() == Some(&child);
        let allowed = match parent.kind() {
            FUNCTION_DEF | FOR_EXPR | WHILE_EXPR | REPEAT_EXPR | MATCH_ARM => false,
            IF_EXPR | SUBSET_EXPR => is_first,
            BINARY_EXPR => {
                let operator = BinaryExpr::cast(parent.clone())
                    .and_then(|binary| binary.operator())
                    .map(|operator| operator.kind());
                match operator {
                    Some(AMP2 | PIPE2 | PIPE_GREATER) => is_first,
                    Some(TILDE) => false,
                    Some(SPECIAL) => is_first || !is_pipe_special(&parent),
                    _ => !is_assignment_target(&parent, &child),
                }
            }
            UNARY_EXPR => child_token(&parent, TILDE).is_none(),
            CALL_EXPR => is_first || argument_evaluation(&parent).is_none(),
            _ => true,
        };
        let runs_before = child.siblings(Direction::Prev).skip(1).any(|sibling| {
            sibling
                .descendants()
                .any(|node| is_assignment(&node) || (moves_call && node.kind() == CALL_EXPR))
        });
        if !allowed || runs_before {
            return false;
        }
        if parent == *statement {
            return true;
        }
        child = parent;
    }
    false
}

fn has_call(node: &SyntaxNode) -> bool {
    node.descendants().any(|node| node.kind() == CALL_EXPR)
}

/// Whether an expression moved to the right of `<-` or `function()` needs parentheses to stay
/// one expression: `?` binds more loosely than both, and a line break that parentheses around
/// the original made a continuation would end the statement on its own.
fn needs_grouping(expression: &SyntaxNode, text: &str) -> bool {
    let alone = syntax::parse(text);
    child_token(expression, QUESTION).is_some()
        || !alone.errors().is_empty()
        || alone.syntax_node().children().count() != 1
}

/// Whether `node` sits in code that is taken as code rather than evaluated in place: a formula,
/// an argument of `quote()` or a data-masking call, or the index of a `[`, where data.table
/// reads columns.
fn is_captured(node: &SyntaxNode) -> bool {
    let mut child = node.clone();
    for ancestor in node.ancestors().skip(1) {
        let captured = match ancestor.kind() {
            BINARY_EXPR | UNARY_EXPR => child_token(&ancestor, TILDE).is_some(),
            CALL_EXPR => argument_evaluation(&ancestor) == Some(ArgumentEvaluation::Captured),
            SUBSET_EXPR => ancestor.first_child().as_ref() != Some(&child),
            _ => false,
        };
        if captured {
            return true;
        }
        child = ancestor;
    }
    false
}

/// Whether the code reads its caller's frame or leaves it early, which a function boundary
/// would change: `return`, `<<-`, `...`, `missing()`, frame inspection, or a `break` whose
/// loop is outside.
fn can_move_into_function(node: &SyntaxNode) -> bool {
    for descendant in node.descendants_with_tokens() {
        match descendant {
            SyntaxElement::Token(token) => {
                let in_own_function = || {
                    token.parent_ancestors().any(|ancestor| {
                        ancestor.kind() == FUNCTION_DEF
                            && node.text_range().contains_range(ancestor.text_range())
                    })
                };
                match token.kind() {
                    LESS2_MINUS | MINUS_GREATER2 => return false,
                    DOTS if !in_own_function() => return false,
                    IDENT if token.text().starts_with("..") && !in_own_function() => {
                        return false;
                    }
                    _ => {}
                }
            }
            SyntaxElement::Node(child) => match child.kind() {
                BREAK_EXPR | NEXT_EXPR => {
                    let loop_inside = child.ancestors().any(|ancestor| {
                        matches!(ancestor.kind(), FOR_EXPR | WHILE_EXPR | REPEAT_EXPR)
                            && node.text_range().contains_range(ancestor.text_range())
                    });
                    if !loop_inside {
                        return false;
                    }
                }
                CALL_EXPR => {
                    let frame_bound = callee_name(&child).is_some_and(|name| {
                        matches!(
                            name.as_str(),
                            "return"
                                | "on.exit"
                                | "missing"
                                | "sys.call"
                                | "sys.function"
                                | "match.arg"
                                | "match.call"
                                | "parent.frame"
                                | "environment"
                                | "nargs"
                                | "Recall"
                                | "substitute"
                                | "assign"
                                | "get"
                                | "get0"
                                | "exists"
                                | "rm"
                                | "local"
                                | "eval"
                                | "evalq"
                                | "ls"
                        )
                    });
                    if frame_bound {
                        return false;
                    }
                }
                _ => {}
            },
        }
    }
    true
}

/// The names the statements assign in their own scope, outside any function they define.
fn assigned_names(nodes: &[SyntaxNode]) -> HashSet<String> {
    let mut names = HashSet::new();
    for node in nodes {
        for descendant in node.descendants() {
            let in_nested_function = descendant.ancestors().any(|ancestor| {
                ancestor.kind() == FUNCTION_DEF
                    && node.text_range().contains_range(ancestor.text_range())
            });
            if in_nested_function {
                continue;
            }
            if let Some(binary) = BinaryExpr::cast(descendant.clone()) {
                let target = match binary.operator().map(|operator| operator.kind()) {
                    Some(LESS_MINUS | EQ | COLON_EQ) => binary.lhs(),
                    Some(MINUS_GREATER) => binary.rhs(),
                    _ => None,
                };
                if let Some(name) = target.as_ref().and_then(assigned_variable) {
                    names.insert(name);
                }
            } else if descendant.kind() == FOR_EXPR
                && let Some(variable) = child_node(&descendant, NAME)
            {
                names.insert(variable.text().to_string());
            }
        }
    }
    names
}

/// The variable an assignment target writes: `x` in `x`, `x$a`, `x[i]` or `names(x)`.
fn assigned_variable(target: &SyntaxNode) -> Option<String> {
    match target.kind() {
        NAME => Some(target.text().to_string()),
        DOLLAR_EXPR | AT_EXPR | SUBSET_EXPR | SUBSET2_EXPR => {
            assigned_variable(&target.first_child()?)
        }
        CALL_EXPR => {
            let arguments = child_node(target, ARGUMENT_LIST)?;
            let first = arguments
                .children()
                .find(|child| child.kind() == ARGUMENT)?;
            assigned_variable(&first.first_child()?)
        }
        _ => None,
    }
}

/// Whether a name of `names` is assigned, looped over, or bound as a parameter in `range`.
fn rebinds_any(root: &SyntaxNode, names: &HashSet<String>, range: TextRange) -> bool {
    root.descendants().any(|node| {
        if !range.contains_range(node.text_range()) {
            return false;
        }
        let bound = match node.kind() {
            PARAMETER => parameter_name_token(&node).map(|token| token.text().to_owned()),
            FOR_EXPR => child_node(&node, NAME).map(|name| name.text().to_string()),
            BINARY_EXPR if is_assignment(&node) => {
                let binary = BinaryExpr::cast(node.clone());
                let target = binary.and_then(|binary| {
                    match binary.operator().map(|operator| operator.kind()) {
                        Some(MINUS_GREATER | MINUS_GREATER2) => binary.rhs(),
                        _ => binary.lhs(),
                    }
                });
                target.as_ref().and_then(assigned_variable)
            }
            _ => None,
        };
        bound.is_some_and(|name| names.contains(&name))
    })
}

fn has_quiet_reads(db: &dyn Db, file: SourceFile, range: TextRange) -> bool {
    item_tree(db, file).iter().any(|item| {
        let (Some(node), Some(hir), Some(naming)) = (
            item_node(db, *item),
            item_hir(db, *item),
            item_naming(db, *item),
        ) else {
            return false;
        };
        let offset = node.text_range().start();
        ranges_intersect(node.text_range(), range)
            && naming
                .quiet_reads
                .iter()
                .any(|(expression, _)| range.contains_range(hir.range(*expression) + offset))
    })
}

fn ranges_intersect(left: TextRange, right: TextRange) -> bool {
    left.start() < right.end() && right.start() < left.end()
}

/// The whitespace before `offset` on its line, or `None` when code precedes it there.
fn line_indent(text: &str, offset: TextSize) -> Option<String> {
    let offset = usize::from(offset).min(text.len());
    let line_start = text.get(..offset)?.rfind('\n').map_or(0, |at| at + 1);
    let before = text.get(line_start..offset)?;
    before
        .chars()
        .all(|character| character == ' ' || character == '\t')
        .then(|| before.to_owned())
}

/// Moves code from `base` indentation to one level inside a new function body.
/// Indents the code at `range` by two spaces past `base`, leaving alone the lines that continue
/// a multi-line string, whose leading spaces are part of its value.
fn reindent(root: &SyntaxNode, text: &str, range: TextRange, base: &str) -> String {
    let mut offset = range.start();
    slice(text, range)
        .split('\n')
        .map(|line| {
            let line_start = offset;
            offset += TextSize::of(line) + TextSize::of('\n');
            let in_string = root.token_at_offset(line_start).any(|token| {
                matches!(token.kind(), STRING | RAW_STRING)
                    && token.text_range().start() < line_start
            });
            if in_string {
                return line.to_owned();
            }
            let line = line.strip_prefix(base).unwrap_or(line);
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where a new definition goes before `statement`: above its documentation comments and `#:`
/// annotation, which belong to it.
fn leading_comments_start(statement: &SyntaxNode) -> TextSize {
    let mut start = statement.text_range().start();
    let mut newlines = 0;
    let mut element = statement.prev_sibling_or_token();
    while let Some(current) = element {
        match current.kind() {
            WHITESPACE => {}
            NEWLINE => {
                newlines += 1;
                if newlines > 1 {
                    break;
                }
            }
            COMMENT | ANNOTATION => {
                newlines = 0;
                start = current.text_range().start();
            }
            _ => break,
        }
        element = current.prev_sibling_or_token();
    }
    start
}

fn previous_node_is_annotation(statement: &SyntaxNode) -> bool {
    let mut newlines = 0;
    let mut element = statement.prev_sibling_or_token();
    while let Some(current) = element {
        match current.kind() {
            WHITESPACE => {}
            NEWLINE if newlines == 0 => newlines += 1,
            ANNOTATION => return true,
            _ => return false,
        }
        element = current.prev_sibling_or_token();
    }
    false
}

/// The statement a `#:` annotation documents: the next node, on the next line.
fn next_statement(annotation: &SyntaxNode) -> Option<SyntaxNode> {
    let mut newlines = 0;
    let mut element = annotation.next_sibling_or_token();
    while let Some(current) = element {
        match &current {
            SyntaxElement::Token(token) => match token.kind() {
                WHITESPACE => {}
                NEWLINE if newlines == 0 => newlines += 1,
                _ => return None,
            },
            SyntaxElement::Node(node) => return Some(node.clone()),
        }
        element = current.next_sibling_or_token();
    }
    None
}

fn ident_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    let at = root.token_at_offset(offset);
    at.clone()
        .right_biased()
        .filter(|token| token.kind() == IDENT)
        .or_else(|| at.left_biased().filter(|token| token.kind() == IDENT))
}

fn name_node_at(root: &SyntaxNode, range: TextRange) -> Option<SyntaxNode> {
    match root.covering_element(range) {
        SyntaxElement::Node(node) => node.ancestors().find(|node| node.kind() == NAME),
        SyntaxElement::Token(token) => token.parent().filter(|parent| parent.kind() == NAME),
    }
}

/// Whether the value needs parentheses where `read` stands, so that it keeps its grouping.
fn needs_parentheses(value: &SyntaxNode, read: &SyntaxNode) -> bool {
    let atomic = matches!(
        value.kind(),
        NAME | LITERAL
            | PAREN_EXPR
            | CALL_EXPR
            | SUBSET_EXPR
            | SUBSET2_EXPR
            | DOLLAR_EXPR
            | AT_EXPR
            | NAMESPACE_EXPR
            | TUPLE_EXPR
            | RECORD_EXPR
            | ARRAY_EXPR
            | TAG_EXPR
    );
    if atomic {
        return false;
    }
    let Some(parent) = read.parent() else {
        return false;
    };
    let alone = match parent.kind() {
        ARGUMENT | PAREN_EXPR => true,
        kind if is_statement_container(kind) => true,
        BINARY_EXPR if is_assignment(&parent) => !is_assignment_target(&parent, read),
        // `a - b * c` keeps `b * c` together, so inlining `b * c` there needs no parentheses.
        BINARY_EXPR if value.kind() == BINARY_EXPR && !is_assignment(value) => {
            let (
                Some((parent_operator, (parent_left, parent_right))),
                Some((operator, (left, right))),
            ) = (binary_operator(&parent), binary_operator(value))
            else {
                return true;
            };
            // R rejects `a < b == x`, so a comparison never meets another without parentheses.
            if is_comparison(parent_operator.kind()) && is_comparison(operator.kind()) {
                return true;
            }
            if read.text_range().end() <= parent_operator.text_range().start() {
                // A prefix `!`, `~` or `?`, or a construct like `function`, ends the value by
                // swallowing everything to its right, so `a + !b` before `< c` reads as
                // `a + !(b < c)`.
                parent_left < right && !right_spine_swallows(value)
            } else {
                left >= parent_right
            }
        }
        _ => false,
    };
    !alone
}

/// Whether the rightmost operand chain of `value` ends in a construct that takes everything
/// after it as its operand or body.
fn right_spine_swallows(value: &SyntaxNode) -> bool {
    let mut node = value.clone();
    loop {
        match node.kind() {
            FUNCTION_DEF | IF_EXPR | FOR_EXPR | WHILE_EXPR | REPEAT_EXPR => return true,
            UNARY_EXPR
                if node
                    .first_token()
                    .is_some_and(|token| matches!(token.kind(), BANG | TILDE | QUESTION)) =>
            {
                return true;
            }
            BINARY_EXPR | UNARY_EXPR => match node.last_child() {
                Some(child) => node = child,
                None => return false,
            },
            _ => return false,
        }
    }
}

fn is_comparison(kind: SyntaxKind) -> bool {
    matches!(kind, EQ2 | BANG_EQ | LESS | GREATER | LESS_EQ | GREATER_EQ)
}

/// The operator token of a binary expression, with its binding powers.
fn binary_operator(node: &SyntaxNode) -> Option<(SyntaxToken, (u8, u8))> {
    node.children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .find_map(|token| {
            let powers = infix_binding_power(token.kind())?;
            Some((token, powers))
        })
}

/// `base`, or `base` with the first numeric suffix that nothing in the file or the project's
/// libraries spells.
fn fresh_name(db: &dyn Db, root: &SyntaxNode, base: &str) -> String {
    let spelled: HashSet<String> = root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|token| token.kind() == IDENT)
        .map(|token| token.text().to_owned())
        .collect();
    let files = ProjectFiles::try_get(db);
    let taken = |name: &str| {
        spelled.contains(name)
            || stub_declaration(db, name).is_some()
            || files.is_some_and(|files| global_declaration_exists(db, files, name))
    };
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|suffix| format!("{base}{suffix}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| base.to_owned())
}
