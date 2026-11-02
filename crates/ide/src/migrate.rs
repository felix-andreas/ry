//! Rewrites common data.table code into DataFrame code. The editor offers each rewrite as a code
//! action and `ry migrate data-table` applies them to whole files, so both read this one engine.
//!
//! A rewrite is offered only for a form whose DataFrame equivalent computes the same table, up to
//! the differences `guides/from-data-table.md` lists (DataFrame aggregations skip `NA`, and a sort
//! puts `NA` last), and every other form is left alone: a wrong rewrite would be worse than none. Two facts decide
//! whether a form qualifies, and both come from the checker. A query `x[...]` is rewritten only
//! when `x` types as a table (a `data.table` or a `DataFrame`), because the same brackets index
//! plain vectors. And a bare name inside the brackets is a column when the table's columns are
//! known and include it; when they are not known, it is a column unless it names a variable of
//! the enclosing function or the project, since data.table looks a name up in the table first.
//!
//! The DataFrame verbs refuse a data.table, so a rewritten query, `:=` or `setorder()` whose table
//! is still a data.table converts it with `as.DataFrame()`. The whole-file migration drops that
//! conversion for a name every assignment of which the migration itself turns into a DataFrame,
//! such as `sales <- fread("sales.csv")`; a single rewrite applied alone always keeps it.

use {
    semantics::{
        Db, ProjectFiles, SourceFile,
        check::ItemCheck,
        diagnostics::TypeRenderer,
        hir::{ExprId, Symbol},
        item_check, item_hir, item_naming, item_node, item_tree, parse,
        naming::ItemNaming,
        project_global_names,
        types::{FrameKind, TyKind},
    },
    std::collections::{HashMap, HashSet},
    syntax::{
        SyntaxKind::{self, *},
        SyntaxNode, SyntaxToken, TextRange, TextSize, is_syntactic_name,
    },
};

/// One data.table form and the DataFrame code that replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataTableRewrite {
    /// What the rewrite does, as a code action title.
    pub title: &'static str,
    pub range: TextRange,
    /// The code in the whole-file migration, where the file's other rewrites also apply, or
    /// `None` when the whole-file migration leaves the form alone because its table would reach
    /// code that still needs a data.table.
    pub replacement: Option<String>,
    /// The code when this rewrite is applied on its own.
    pub standalone: String,
}

/// Every form in `file` that has a DataFrame equivalent, outermost first and in source order. A
/// query nested in another rewritten query is part of the outer rewrite, so no two overlap.
pub fn data_table_rewrites(db: &dyn Db, file: SourceFile) -> Vec<DataTableRewrite> {
    let globals = ProjectFiles::try_get(db).map(|files| project_global_names(db, files));
    let is_project_name = |name: Symbol| globals.is_some_and(|names| names.contains(&name));
    let alone = collect_rewrites(db, file, &is_project_name, &HashSet::new());
    let root = parse(db, file).syntax_node();
    // Leaving a form alone can make another form's table reach unrewritten code, so the
    // whole-file migration drops forms until every kept one is safe.
    let mut fates = vec![Fate::Keep; alone.len()];
    let together = loop {
        let kept: Vec<&Rewrite> = alone
            .iter()
            .zip(&fates)
            .filter(|(_, fate)| **fate != Fate::Drop)
            .map(|(rewrite, _)| rewrite)
            .collect();
        let converted = converted_names(db, file, &kept);
        let together = collect_rewrites(db, file, &is_project_name, &converted);
        // The conversions change only the code a rewrite emits, never which forms qualify, so the
        // two passes find the same forms in the same order.
        assert_eq!(
            alone.len(),
            together.len(),
            "the data.table migration found different forms with and without conversions"
        );
        let kept_ranges: Vec<TextRange> = kept.iter().map(|rewrite| rewrite.range).collect();
        let mut changed = false;
        for (rewrite, slot) in alone.iter().zip(fates.iter_mut()) {
            if *slot == Fate::Drop {
                continue;
            }
            let decided = fate(&root, rewrite, &kept_ranges);
            changed |= decided == Fate::Drop;
            *slot = decided;
        }
        if !changed {
            break together;
        }
    };
    alone
        .into_iter()
        .zip(together)
        .zip(fates)
        .map(|((alone, together), fate)| DataTableRewrite {
            title: alone.title,
            range: alone.range,
            replacement: match fate {
                Fate::Keep => Some(together.replacement),
                Fate::Wrap => Some(format!("data.table::as.data.table({})", together.replacement)),
                Fate::Drop => None,
            },
            standalone: alone.replacement,
        })
        .collect()
}

/// What the whole-file migration does with one rewrite. A rewrite turns a data.table into a
/// DataFrame, which the code around it must accept: the DataFrame verbs and the column reads
/// `$`, `[[`, `nrow()` and the like do, but an unrewritten query, `:=` or any other function
/// may need the data.table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Keep,
    /// The query is a function's return value: it runs on the DataFrame and converts the result
    /// back, so the function's callers still receive a data.table.
    Wrap,
    Drop,
}

/// Decides one rewrite's [`Fate`] given the ranges of the rewrites still kept.
fn fate(root: &SyntaxNode, rewrite: &Rewrite, kept: &[TextRange]) -> Fate {
    let safe = |name: &str, scope: &SyntaxNode| uses_are_safe(scope, name, kept);
    if let Some(name) = &rewrite.rebinds {
        return if safe(name, root) { Fate::Keep } else { Fate::Drop };
    }
    let element = root.covering_element(rewrite.range);
    let Some(mut value) = element
        .as_node()
        .cloned()
        .or_else(|| element.as_token().and_then(|token| token.parent()))
        .filter(|node| node.text_range() == rewrite.range)
    else {
        return Fate::Drop;
    };
    // A constructor rewrite replaces only the call's head.
    if let Some(call) = value.parent().filter(|parent| {
        parent.kind() == CALL_EXPR && parent.first_child().as_ref() == Some(&value)
    }) {
        value = call;
    }
    let value = outermost_parentheses(value);
    if is_returned(&value) {
        return if value.kind() == SUBSET_EXPR {
            Fate::Wrap
        } else {
            Fate::Drop
        };
    }
    if is_read(&value) {
        return Fate::Keep;
    }
    let Some(parent) = value.parent() else {
        return Fate::Drop;
    };
    if matches!(parent.kind(), SOURCE_FILE | BRACE_EXPR) {
        return Fate::Keep;
    }
    match assignment(&parent) {
        Some((name, assigned))
            if assigned == value
                && name.kind() == NAME
                && parent
                    .parent()
                    .is_some_and(|statement| matches!(statement.kind(), SOURCE_FILE | BRACE_EXPR))
                && !is_returned(&parent) =>
        {
            let scope = parent
                .ancestors()
                .find(|ancestor| ancestor.kind() == FUNCTION_DEF)
                .unwrap_or_else(|| root.clone());
            if safe(&name_text(&name), &scope) {
                Fate::Keep
            } else {
                Fate::Drop
            }
        }
        _ => Fate::Drop,
    }
}

/// Whether every use of `name` in `scope` accepts a DataFrame: it is assigned, it is the table of
/// a kept rewrite (or inside one), or it is only read through [`is_read`]. A nested function's
/// use counts too, which can only leave more forms alone.
fn uses_are_safe(scope: &SyntaxNode, name: &str, kept: &[TextRange]) -> bool {
    scope
        .descendants()
        .filter(|node| node.kind() == NAME && name_text(node) == name)
        .all(|node| {
            let range = node.text_range();
            kept.iter().any(|kept| kept.contains_range(range))
                || node.parent().and_then(|parent| assignment(&parent)).is_some_and(
                    |(target, _)| target == node,
                )
                || is_read(&outermost_parentheses(node))
        })
}

/// `name <- value`, `value -> name` or `name = value` as `(name, value)`.
fn assignment(node: &SyntaxNode) -> Option<(SyntaxNode, SyntaxNode)> {
    if node.kind() != BINARY_EXPR {
        return None;
    }
    let operator = node
        .children_with_tokens()
        .filter_map(|child| child.into_token())
        .find(|token| !token.kind().is_trivia())?;
    let mut operands = node.children();
    let (left, right) = (operands.next()?, operands.next()?);
    match operator.kind() {
        LESS_MINUS | LESS2_MINUS | EQ => Some((left, right)),
        MINUS_GREATER | MINUS_GREATER2 => Some((right, left)),
        _ => None,
    }
}

/// Whether `node` is only read by code that answers the same for a DataFrame as for a
/// data.table: `node$col`, `node[["col"]]`, or the one argument of a shape function such as
/// `nrow()`.
fn is_read(node: &SyntaxNode) -> bool {
    const SHAPE: &[&str] = &[
        "nrow", "ncol", "NROW", "NCOL", "dim", "names", "colnames", "length",
    ];
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        DOLLAR_EXPR | SUBSET2_EXPR => {
            parent.first_child().as_ref() == Some(node)
                && !parent
                    .parent()
                    .and_then(|grand| assignment(&grand))
                    .is_some_and(|(target, _)| target == parent)
        }
        ARGUMENT => {
            let Some(call) = parent.parent().and_then(|list| list.parent()) else {
                return false;
            };
            let Some(list) = child_node(&call, ARGUMENT_LIST) else {
                return false;
            };
            call.kind() == CALL_EXPR
                && first_token(&list).is_some_and(|token| token.kind() == L_PAREN)
                && arguments(&list).len() == 1
                && call.first_child().is_some_and(|callee| {
                    callee.kind() == NAME && SHAPE.contains(&name_text(&callee).as_str())
                })
        }
        _ => false,
    }
}

/// Whether `node` is the value its function returns: the function's body, the last expression
/// of its braces, an `if` branch in such a place, or the argument of `return()`.
fn is_returned(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        FUNCTION_DEF => node.kind() != PARAMETER_LIST,
        BRACE_EXPR => {
            parent.children().last().as_ref() == Some(node)
                && parent.parent().is_some_and(|grand| {
                    grand.kind() == FUNCTION_DEF || (grand.kind() != SOURCE_FILE && is_returned(&outermost_parentheses(parent.clone())))
                })
        }
        IF_EXPR => {
            parent.first_child().as_ref() != Some(node) && is_returned(&outermost_parentheses(parent.clone()))
        }
        ARGUMENT => parent
            .parent()
            .and_then(|list| list.parent())
            .filter(|call| call.kind() == CALL_EXPR)
            .and_then(|call| call.first_child())
            .is_some_and(|callee| callee.kind() == NAME && name_text(&callee) == "return"),
        _ => false,
    }
}

fn outermost_parentheses(mut node: SyntaxNode) -> SyntaxNode {
    while let Some(parent) = node.parent().filter(|parent| parent.kind() == PAREN_EXPR) {
        node = parent;
    }
    node
}

/// One pass over the file, with `converted` naming the tables the rest of the migration has
/// already turned into DataFrames.
fn collect_rewrites(
    db: &dyn Db,
    file: SourceFile,
    is_project_name: &dyn Fn(Symbol) -> bool,
    converted: &HashSet<String>,
) -> Vec<Rewrite> {
    let text = file.text(db);
    let mut rewrites = Vec::new();
    for &item in item_tree(db, file) {
        let Some(root) = item_node(db, item) else {
            continue;
        };
        let offset = root.text_range().start();
        let hir = item_hir(db, item).as_ref();
        let migration = Migration {
            db,
            text,
            offset,
            check: item_check(db, item),
            naming: item_naming(db, item).as_ref(),
            is_project_name,
            converted,
            expressions: hir
                .map(|hir| {
                    let mut expressions = HashMap::new();
                    for (index, range) in hir.ranges().iter().enumerate() {
                        expressions.entry(*range).or_insert(ExprId::new(index));
                    }
                    expressions
                })
                .unwrap_or_default(),
        };
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if let Some(rewrite) = migration.rewrite(&node) {
                rewrites.push(rewrite);
                continue;
            }
            let children: Vec<SyntaxNode> = node.children().collect();
            stack.extend(children.into_iter().rev());
        }
    }
    rewrites
}

/// The names that are DataFrames once every rewrite applies: each assignment of the name in the
/// file assigns a rewritten `fread()`, `data.table()`, `as.data.table()` or query, and no function
/// takes a parameter of that name. Any other binding, such as a parameter or a `data.frame()`,
/// may hold a data.table, so the name keeps its conversion.
fn converted_names(db: &dyn Db, file: SourceFile, rewrites: &[&Rewrite]) -> HashSet<String> {
    let rewritten: HashSet<TextRange> = rewrites.iter().map(|rewrite| rewrite.range).collect();
    let converts = |value: &SyntaxNode| {
        rewritten.contains(&value.text_range())
            || (value.kind() == CALL_EXPR
                && value
                    .first_child()
                    .is_some_and(|callee| rewritten.contains(&callee.text_range())))
    };
    let mut converted = HashSet::new();
    let mut unconverted = HashSet::new();
    for &item in item_tree(db, file) {
        let Some(root) = item_node(db, item) else {
            continue;
        };
        for node in root.descendants() {
            match node.kind() {
                PARAMETER => {
                    if let Some(name) = node.children_with_tokens().find_map(|child| {
                        child
                            .into_token()
                            .filter(|token| token.kind() == IDENT)
                            .map(|token| token.text().to_owned())
                    }) {
                        unconverted.insert(name);
                    }
                }
                BINARY_EXPR => {
                    let Some(operator) = node
                        .children_with_tokens()
                        .filter_map(|child| child.into_token())
                        .find(|token| !token.kind().is_trivia())
                    else {
                        continue;
                    };
                    let mut operands = node.children();
                    let (Some(left), Some(right)) = (operands.next(), operands.next()) else {
                        continue;
                    };
                    let (name, value) = match operator.kind() {
                        LESS_MINUS | LESS2_MINUS | EQ => (left, right),
                        MINUS_GREATER | MINUS_GREATER2 => (right, left),
                        _ => continue,
                    };
                    if name.kind() != NAME {
                        continue;
                    }
                    let name = name_text(&name);
                    if converts(&value) {
                        converted.insert(name);
                    } else {
                        unconverted.insert(name);
                    }
                }
                _ => {}
            }
        }
    }
    converted.retain(|name| !unconverted.contains(name));
    converted
}

/// `text` with every rewrite applied. The rewrites must not overlap, as
/// [`data_table_rewrites`] guarantees.
pub fn apply_data_table_rewrites(text: &str, rewrites: &[DataTableRewrite]) -> String {
    let mut ordered: Vec<(TextRange, &str)> = rewrites
        .iter()
        .filter_map(|rewrite| Some((rewrite.range, rewrite.replacement.as_deref()?)))
        .collect();
    ordered.sort_by_key(|(range, _)| range.start());
    let mut result = String::with_capacity(text.len());
    let mut at = 0usize;
    for (range, replacement) in ordered {
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        let Some(before) = text.get(at..start) else {
            continue;
        };
        result.push_str(before);
        result.push_str(replacement);
        at = end;
    }
    result.push_str(text.get(at..).unwrap_or_default());
    result
}

/// A rewrite as one pass of the migration finds it.
struct Rewrite {
    title: &'static str,
    range: TextRange,
    replacement: String,
    /// The name an `:=`, `setorder()` or `setDT()` rewrite rebinds to a DataFrame. Any other
    /// rewrite yields its table where it stands.
    rebinds: Option<String>,
}

/// The facts one item's rewrites read: its expressions' types and what its names resolve to.
struct Migration<'a, 'db> {
    db: &'db dyn Db,
    text: &'a str,
    /// Where the item starts: HIR ranges are relative to it.
    offset: TextSize,
    check: Option<&'a ItemCheck<'db>>,
    naming: Option<&'a ItemNaming>,
    is_project_name: &'a dyn Fn(Symbol) -> bool,
    /// Names the rest of the migration turns into DataFrames, see [`converted_names`].
    converted: &'a HashSet<String>,
    /// Each item-relative range to the first expression that spans it.
    expressions: HashMap<TextRange, ExprId>,
}

/// What the checker knows about a value a query reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Table {
    /// A value of any other type, such as a vector.
    Other,
    /// A table whose columns the checker does not know.
    Unknown,
    /// Each column's name and rendered type, in order.
    Known(Vec<(String, String)>),
}

/// Inside a query, an expression either yields one value per row or reduces a group to one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    Row,
    Aggregate,
}

/// Translated code.
struct Fragment {
    text: String,
    /// In row context, the expression reads a column; in aggregate context, it aggregates one.
    /// Without either it is a plain R value rather than a column expression.
    is_expression: bool,
    /// The code is a `|>` chain, which binds tighter than arithmetic but looser than `$` and
    /// `[`, so as an operand it needs parentheses.
    piped: bool,
}

/// A query's parsed brackets, `x[i, j, by = ..., on = ...]`.
struct Query {
    target: SyntaxNode,
    i: Option<SyntaxNode>,
    j: Option<SyntaxNode>,
    by: Option<SyntaxNode>,
    on: Option<SyntaxNode>,
}

/// One output column of `j` or `:=`: the name as it is written in the output, and its value.
struct Column {
    name: String,
    value: String,
}

const AGGREGATIONS: &[(&str, &str)] = &[
    ("sum", "Expr.sum"),
    ("mean", "Expr.mean"),
    ("median", "Expr.median"),
    ("min", "Expr.min"),
    ("max", "Expr.max"),
    ("sd", "Expr.std"),
    ("var", "Expr.var"),
    ("first", "Expr.first"),
    ("last", "Expr.last"),
];

/// Names base R binds that a query reads as values, never as columns, when the columns are not
/// known.
const BASE_CONSTANTS: &[&str] = &[
    "T",
    "F",
    "pi",
    "LETTERS",
    "letters",
    "month.abb",
    "month.name",
];

/// data.table's own symbols inside the brackets. Only `.N` has a DataFrame equivalent.
const QUERY_SYMBOLS: &[&str] = &[".N", ".SD", ".I", ".GRP", ".NGRP", ".BY", ".EACHI"];

impl<'db> Migration<'_, 'db> {
    fn rewrite(&self, node: &SyntaxNode) -> Option<Rewrite> {
        match node.kind() {
            SUBSET_EXPR => self.rewrite_subset(node),
            CALL_EXPR => self.rewrite_call(node),
            _ => None,
        }
    }

    fn rewrite_subset(&self, node: &SyntaxNode) -> Option<Rewrite> {
        let query = self.query(node)?;
        if query.j.as_ref().is_some_and(is_update) {
            let replacement = self.update(node, &query)?;
            return Some(Rewrite {
                title: "Rewrite `:=` with `DataFrame.with_columns`",
                range: node.text_range(),
                replacement,
                rebinds: Some(node_text(&query.target)),
            });
        }
        let (title, fragment) = self.query_expression(&query)?;
        let replacement = if fragment.piped && needs_parentheses(node) {
            format!("({})", fragment.text)
        } else {
            fragment.text
        };
        Some(Rewrite {
            title,
            range: node.text_range(),
            replacement,
            rebinds: None,
        })
    }

    /// The brackets of a query on a table, or `None` for `[[`, a vector index and any argument
    /// this engine does not translate.
    fn query(&self, node: &SyntaxNode) -> Option<Query> {
        let target = node.first_child()?;
        let list = child_node(node, ARGUMENT_LIST)?;
        if first_token(&list)?.kind() != L_BRACKET {
            return None;
        }
        let mut positional = Vec::new();
        let (mut by, mut on) = (None, None);
        for argument in arguments(&list) {
            match argument.tag {
                None => positional.push(argument.value),
                Some(tag) => {
                    let value = Some(argument.value?);
                    match tag_name(&tag)?.as_str() {
                        "by" if by.is_none() => by = value,
                        "on" if on.is_none() => on = value,
                        _ => return None,
                    }
                }
            }
        }
        let (i, j) = match positional.as_slice() {
            [i] => (i.clone(), None),
            [i, j] => (i.clone(), j.clone()),
            _ => return None,
        };
        if self.table(&target) == Table::Other {
            return None;
        }
        Some(Query {
            target,
            i,
            j,
            by,
            on,
        })
    }

    /// A query that yields a new table: a filter, a selection, a grouping or a join.
    fn query_expression(&self, query: &Query) -> Option<(&'static str, Fragment)> {
        if query.i.is_none() && query.j.is_none() {
            return None;
        }
        let table = self.table(&query.target);
        let target = self.target_text(&query.target);
        if let Some(on) = &query.on {
            if query.j.is_some() || query.by.is_some() {
                return None;
            }
            let other = query.i.as_ref()?;
            if other.kind() != NAME {
                return None;
            }
            let other_table = self.table(other);
            let keys = string_list(on)?;
            if other_table == Table::Other || keys.is_empty() {
                return None;
            }
            // data.table prefixes a clashing column of `i` with `i.`, a DataFrame join suffixes
            // it with `_right`, so a join with clashing columns has no equivalent, and one whose
            // columns are not known might have them.
            let (Table::Known(left), Table::Known(right)) = (&table, &other_table) else {
                return None;
            };
            let clashes = right.iter().any(|(name, _)| {
                !keys.contains(name) && left.iter().any(|(other, _)| other == name)
            });
            if clashes
                || keys.iter().any(|key| {
                    !left.iter().any(|(name, _)| name == key)
                        || !right.iter().any(|(name, _)| name == key)
                })
            {
                return None;
            }
            return Some((
                "Rewrite the join with `DataFrame.join`",
                Fragment {
                    // `x[i, on =]` lists `x`'s columns first and then `i`'s other columns, while
                    // the join keeps `i`'s rows in order and lists its columns first.
                    text: format!(
                        "DataFrame.join({}, {target}, on = {}, how = \"left\") |> DataFrame.select({})",
                        self.as_data_frame(other),
                        node_text(on),
                        left.iter()
                            .map(|(name, _)| name)
                            .chain(
                                right
                                    .iter()
                                    .map(|(name, _)| name)
                                    .filter(|name| !keys.contains(name))
                            )
                            .map(|name| quote(name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    is_expression: true,
                    piped: true,
                },
            ));
        }
        let mut text = target;
        if let Some(i) = &query.i {
            let conditions = self.filter_conditions(i, &table)?;
            text = format!("{text} |> DataFrame.filter({})", conditions.join(", "));
        }
        match (&query.j, &query.by) {
            (None, None) => {}
            (None, Some(_)) => return None,
            (Some(j), None) => {
                let columns = self.selection(j, &table)?;
                text = format!("{text} |> DataFrame.select({})", columns.join(", "));
            }
            (Some(j), Some(by)) => {
                let keys = self.group_keys(by, &table)?;
                let columns = self.aggregations(j, &table, &keys)?;
                let keys: Vec<String> = keys.iter().map(|key| quote(key)).collect();
                text = format!(
                    "{text} |> DataFrame.group_by({}) |> GroupBy.agg({})",
                    keys.join(", "),
                    render_columns(&columns)
                );
            }
        }
        Some((
            "Rewrite the query with DataFrame verbs",
            Fragment {
                text,
                is_expression: true,
                piped: true,
            },
        ))
    }

    /// The code a query's target becomes: a nested query's own rewrite, or the target as written.
    fn target_text(&self, target: &SyntaxNode) -> String {
        if target.kind() == SUBSET_EXPR
            && let Some(query) = self.query(target)
            && !query.j.as_ref().is_some_and(is_update)
            && let Some((_, fragment)) = self.query_expression(&query)
        {
            return fragment.text;
        }
        self.as_data_frame(target)
    }

    /// A table's code where a DataFrame verb reads it: a data.table goes through `as.DataFrame()`
    /// unless the migration already makes it a DataFrame.
    fn as_data_frame(&self, node: &SyntaxNode) -> String {
        let text = node_text(node);
        let is_data_table = self
            .expression_id(node)
            .and_then(|id| self.check?.expression_types.get(&id))
            .is_some_and(|ty| match ty.kind(self.db) {
                TyKind::Frame(FrameKind::DataTable, _) => true,
                TyKind::Named(name, _) => name.text(self.db) == "data.table",
                _ => false,
            });
        if !is_data_table || (node.kind() == NAME && self.converted.contains(&name_text(node))) {
            return text;
        }
        format!("as.DataFrame({text})")
    }

    /// `i` as the conditions of `DataFrame.filter`, one per operand of a top-level `&`. Any
    /// other `i`, such as row numbers or a table to join, is not a filter.
    fn filter_conditions(&self, i: &SyntaxNode, table: &Table) -> Option<Vec<String>> {
        let mut conjuncts = Vec::new();
        split_conjunction(i, &mut conjuncts);
        conjuncts
            .iter()
            .map(|condition| {
                if !is_logical(condition) {
                    return None;
                }
                let fragment = self.expression(condition, Context::Row, table)?;
                fragment.is_expression.then_some(fragment.text)
            })
            .collect()
    }

    /// `j` without `by`: the arguments of `DataFrame.select`. Every column must be computed per
    /// row, or every column must aggregate the whole table, because data.table would recycle a
    /// mix of the two.
    fn selection(&self, j: &SyntaxNode, table: &Table) -> Option<Vec<String>> {
        let items = list_items(j)?;
        let mut names = HashSet::new();
        let mut rendered = Vec::new();
        let mut context = None;
        for (position, (tag, value)) in items.iter().enumerate() {
            if tag.is_none() && value.kind() == NAME && context != Some(Context::Aggregate) {
                let name = name_text(value);
                if !self.is_column(value, &name, table) {
                    return None;
                }
                context = Some(Context::Row);
                names.insert(name.clone()).then_some(())?;
                rendered.push(quote(&name));
                continue;
            }
            let (fragment, used) = match context {
                Some(context) => (self.expression(value, context, table)?, context),
                None => match self.expression(value, Context::Row, table) {
                    Some(fragment) if fragment.is_expression => (fragment, Context::Row),
                    _ => (
                        self.expression(value, Context::Aggregate, table)?,
                        Context::Aggregate,
                    ),
                },
            };
            if !fragment.is_expression {
                return None;
            }
            context = Some(used);
            let (name, written) = output_name(tag.as_ref(), value, position)?;
            names.insert(name).then_some(())?;
            rendered.push(format!("{written} = {}", fragment.text));
        }
        Some(rendered)
    }

    /// `j` with `by`: the arguments of `GroupBy.agg`, each of which must aggregate.
    fn aggregations(&self, j: &SyntaxNode, table: &Table, keys: &[String]) -> Option<Vec<Column>> {
        let items = match list_items(j) {
            Some(items) => items,
            // `x[, sum(v), by = g]` names its one column `V1`, and `x[, .N, by = g]` names it
            // `N`.
            None => vec![(None, j.clone())],
        };
        let mut names: HashSet<String> = keys.iter().cloned().collect();
        let mut columns = Vec::new();
        for (position, (tag, value)) in items.iter().enumerate() {
            let fragment = self.expression(value, Context::Aggregate, table)?;
            if !fragment.is_expression {
                return None;
            }
            let (name, written) = output_name(tag.as_ref(), value, position)?;
            names.insert(name).then_some(())?;
            columns.push(Column {
                name: written,
                value: fragment.text,
            });
        }
        Some(columns)
    }

    /// The columns `by` names: `g`, `.(g, h)`, `list(g, h)`, `"g"` or `c("g", "h")`.
    fn group_keys(&self, by: &SyntaxNode, table: &Table) -> Option<Vec<String>> {
        if let Some(keys) = string_list(by) {
            return keys
                .into_iter()
                .map(|key| match table {
                    Table::Known(columns) if !columns.iter().any(|(name, _)| *name == key) => None,
                    // data.table reads `"g,h"` as two columns.
                    _ if key.contains(',') => None,
                    _ => Some(key),
                })
                .collect();
        }
        let names: Vec<SyntaxNode> = match by.kind() {
            NAME => vec![by.clone()],
            _ => list_items(by)?
                .into_iter()
                .map(|(tag, value)| tag.is_none().then_some(value))
                .collect::<Option<_>>()?,
        };
        let mut keys = Vec::new();
        for name in names {
            if name.kind() != NAME {
                return None;
            }
            let key = name_text(&name);
            if !self.is_column(&name, &key, table) || keys.contains(&key) {
                return None;
            }
            keys.push(key);
        }
        (!keys.is_empty()).then_some(keys)
    }

    /// `x[i, name := value]` and its spellings, as a statement that assigns
    /// `DataFrame.with_columns`. Only a statement outside any function qualifies: data.table
    /// updates the table in place, which a function's caller would see, while the rewrite
    /// rebinds only the name it assigns.
    fn update(&self, node: &SyntaxNode, query: &Query) -> Option<String> {
        if query.by.is_some() || query.on.is_some() || !is_top_level_statement(node) {
            return None;
        }
        let target = &query.target;
        if target.kind() != NAME {
            return None;
        }
        let table = self.table(target);
        let assignments = update_assignments(query.j.as_ref()?)?;
        let mut names = HashSet::new();
        let mut columns = Vec::new();
        let condition = match &query.i {
            None => None,
            Some(i) => {
                if !is_logical(i) {
                    return None;
                }
                let fragment = self.expression(i, Context::Row, &table)?;
                if !fragment.is_expression {
                    return None;
                }
                // `!` binds looser than `&` and `|` looser than `&`, so either needs parentheses
                // before the guard below joins it with `&`.
                let compound = matches!(i.kind(), BINARY_EXPR | UNARY_EXPR) && !is_comparison(i);
                Some((fragment.text, compound))
            }
        };
        for (name, value) in assignments {
            names.insert(name.clone()).then_some(())?;
            let written = written_name(&name);
            let value = match &condition {
                None => self.column_value(&value, &table)?,
                Some((test, compound)) => {
                    // A row whose condition is `NA` is not updated, but `Expr.if_else` gives
                    // `NA` there, so an existing column keeps its value only behind a guard. A
                    // new column holds `NA` in that row either way.
                    let Table::Known(known) = &table else {
                        return None;
                    };
                    match known.iter().find(|(column, _)| *column == name) {
                        Some((_, ty)) => {
                            if literal_type(&value)? != ty.as_str() {
                                return None;
                            }
                            let guarded = if *compound {
                                format!("({test})")
                            } else {
                                test.clone()
                            };
                            format!(
                                "Expr.if_else({guarded} & !Expr.is_null({test}), {}, col({}))",
                                node_text(&value),
                                quote(&name)
                            )
                        }
                        None => {
                            let fragment = self.expression(&value, Context::Row, &table)?;
                            if !fragment.is_expression && !is_literal(&value) {
                                return None;
                            }
                            format!("Expr.if_else({test}, {}, NA)", fragment.text)
                        }
                    }
                }
            };
            columns.push(Column {
                name: written,
                value,
            });
        }
        if columns.is_empty() {
            return None;
        }
        let source = self.as_data_frame(target);
        let target = node_text(target);
        Some(format!(
            "{target} <- {source} |> DataFrame.with_columns({})",
            render_columns(&columns)
        ))
    }

    /// The value `:=` assigns, as an argument of `DataFrame.with_columns`: a column expression,
    /// or a constant through `lit()`.
    fn column_value(&self, value: &SyntaxNode, table: &Table) -> Option<String> {
        if is_literal(value) {
            if is_null(value) {
                return None;
            }
            return Some(format!("lit({})", node_text(value)));
        }
        let fragment = self.expression(value, Context::Row, table)?;
        fragment.is_expression.then_some(fragment.text)
    }

    fn rewrite_call(&self, node: &SyntaxNode) -> Option<Rewrite> {
        let callee = node.first_child()?;
        let function = self.data_table_function(&callee)?;
        let list = child_node(node, ARGUMENT_LIST)?;
        let arguments = arguments(&list);
        match function.as_str() {
            "fread" => {
                let mut path = None;
                let mut factors = None;
                for argument in &arguments {
                    let value = argument.value.clone()?;
                    let name = match &argument.tag {
                        None => "file".to_owned(),
                        Some(tag) => tag_name(tag)?,
                    };
                    match name.as_str() {
                        "file" if path.is_none() => path = Some(value),
                        "stringsAsFactors" if factors.is_none() => factors = Some(value),
                        _ => return None,
                    }
                }
                let path = path?;
                // `DataFrame.read_csv` reads only CSV, while `fread` also sniffs other separators
                // and reads literal text, so only a literal path ending in `.csv` qualifies.
                if path.kind() != LITERAL
                    || !string_value(&path)?.to_ascii_lowercase().ends_with(".csv")
                {
                    return None;
                }
                let factors = factors
                    .map(|value| format!(", strings_as_factors = {}", node_text(&value)))
                    .unwrap_or_default();
                Some(Rewrite {
                    title: "Rewrite `fread()` as `DataFrame.read_csv()`",
                    range: node.text_range(),
                    replacement: format!("DataFrame.read_csv({}{factors})", node_text(&path)),
                    rebinds: None,
                })
            }
            "data.table" => {
                for argument in &arguments {
                    let value = argument.value.as_ref()?;
                    match &argument.tag {
                        Some(tag) => {
                            if matches!(
                                tag_name(tag)?.as_str(),
                                "key" | "keep.rownames" | "check.names" | "stringsAsFactors"
                            ) {
                                return None;
                            }
                        }
                        None if value.kind() == NAME && name_text(value) != "..." => {}
                        None => return None,
                    }
                }
                Some(Rewrite {
                    title: "Rewrite `data.table()` as `DataFrame()`",
                    range: callee.text_range(),
                    replacement: "DataFrame".to_owned(),
                    rebinds: None,
                })
            }
            "as.data.table" => match arguments.as_slice() {
                [argument] if argument.tag.is_none() && argument.value.is_some() => Some(Rewrite {
                    title: "Rewrite `as.data.table()` as `as.DataFrame()`",
                    range: callee.text_range(),
                    replacement: "as.DataFrame".to_owned(),
                    rebinds: None,
                }),
                _ => None,
            },
            "setDT" => match arguments.as_slice() {
                [argument] if argument.tag.is_none() && is_top_level_statement(node) => {
                    let value = argument.value.as_ref()?;
                    if value.kind() != NAME {
                        return None;
                    }
                    let name = node_text(value);
                    Some(Rewrite {
                        title: "Rewrite `setDT()` as `as.DataFrame()`",
                        range: node.text_range(),
                        replacement: format!("{name} <- as.DataFrame({name})"),
                        rebinds: Some(name),
                    })
                }
                _ => None,
            },
            "setorder" => {
                if !is_top_level_statement(node) {
                    return None;
                }
                let (first, rest) = arguments.split_first()?;
                let target = first.value.as_ref()?;
                if first.tag.is_some() || target.kind() != NAME || rest.is_empty() {
                    return None;
                }
                let table = self.table(target);
                if table == Table::Other {
                    return None;
                }
                let mut keys = Vec::new();
                let mut descending = Vec::new();
                for argument in rest {
                    if argument.tag.is_some() {
                        return None;
                    }
                    let value = argument.value.as_ref()?;
                    let (name, down) = match value.kind() {
                        NAME => (value.clone(), false),
                        UNARY_EXPR => {
                            let operator = first_token(value)?.kind();
                            let operand = value.first_child()?;
                            match operator {
                                MINUS if operand.kind() == NAME => (operand, true),
                                PLUS if operand.kind() == NAME => (operand, false),
                                _ => return None,
                            }
                        }
                        _ => return None,
                    };
                    let key = name_text(&name);
                    if !self.is_column(&name, &key, &table) {
                        return None;
                    }
                    keys.push(quote(&key));
                    descending.push(down);
                }
                let descending = if descending.iter().all(|down| !down) {
                    String::new()
                } else if descending.iter().all(|down| *down) {
                    ", descending = TRUE".to_owned()
                } else {
                    let flags: Vec<&str> = descending
                        .iter()
                        .map(|down| if *down { "TRUE" } else { "FALSE" })
                        .collect();
                    format!(", descending = c({})", flags.join(", "))
                };
                let source = self.as_data_frame(target);
                let target = node_text(target);
                Some(Rewrite {
                    title: "Rewrite `setorder()` with `DataFrame.sort`",
                    range: node.text_range(),
                    replacement: format!(
                        "{target} <- {source} |> DataFrame.sort({}{descending})",
                        keys.join(", ")
                    ),
                    rebinds: Some(target),
                })
            }
            _ => None,
        }
    }

    /// The data.table function a call head names: `name` when no binding of the item or the
    /// project shadows it, or `data.table::name`.
    fn data_table_function(&self, callee: &SyntaxNode) -> Option<String> {
        const FUNCTIONS: &[&str] = &["fread", "data.table", "as.data.table", "setDT", "setorder"];
        match callee.kind() {
            NAME => {
                let name = name_text(callee);
                (FUNCTIONS.contains(&name.as_str()) && !self.is_variable(callee, &name))
                    .then_some(name)
            }
            NAMESPACE_EXPR => {
                let mut names = callee.children().filter(|child| child.kind() == NAME);
                let (package, name) = (names.next()?, names.next()?);
                (name_text(&package) == "data.table").then(|| name_text(&name))
            }
            _ => None,
        }
    }

    /// An expression inside the brackets as a DataFrame column expression.
    fn expression(&self, node: &SyntaxNode, context: Context, table: &Table) -> Option<Fragment> {
        let plain = |text: String| Fragment {
            text,
            is_expression: false,
            piped: false,
        };
        match node.kind() {
            NAME => {
                let name = name_text(node);
                if QUERY_SYMBOLS.contains(&name.as_str()) {
                    return (context == Context::Aggregate && name == ".N").then(|| Fragment {
                        text: "Expr.len()".to_owned(),
                        is_expression: true,
                        piped: false,
                    });
                }
                if !self.is_column(node, &name, table) {
                    return Some(plain(node_text(node)));
                }
                // Outside an aggregation, a column in aggregate context gives one value per row.
                (context == Context::Row).then(|| Fragment {
                    text: format!("col({})", quote(&name)),
                    is_expression: true,
                    piped: false,
                })
            }
            LITERAL => (!is_null(node)).then(|| plain(node_text(node))),
            PAREN_EXPR => {
                let inner = self.expression(&node.first_child()?, context, table)?;
                Some(Fragment {
                    text: format!("({})", inner.text),
                    is_expression: inner.is_expression,
                    piped: false,
                })
            }
            UNARY_EXPR => {
                let operator = first_token(node)?;
                if !matches!(operator.kind(), MINUS | PLUS | BANG) {
                    return None;
                }
                let operand = node.first_child()?;
                let inner = self.expression(&operand, context, table)?;
                let between =
                    self.source_between(operator.text_range().end(), operand.text_range().start())?;
                Some(Fragment {
                    text: format!("{}{between}{}", operator.text(), parenthesized(&inner)),
                    is_expression: inner.is_expression,
                    piped: false,
                })
            }
            BINARY_EXPR => {
                let operator = first_token(node)?;
                let allowed = match operator.kind() {
                    PLUS | MINUS | STAR | SLASH | CARET | LESS | GREATER | LESS_EQ | GREATER_EQ
                    | EQ2 | BANG_EQ | AMP | PIPE => true,
                    SPECIAL => matches!(operator.text(), "%%" | "%/%"),
                    _ => false,
                };
                if !allowed {
                    return None;
                }
                let mut operands = node.children();
                let (left, right) = (operands.next()?, operands.next()?);
                let left_fragment = self.expression(&left, context, table)?;
                let right_fragment = self.expression(&right, context, table)?;
                let between =
                    self.source_between(trimmed_end(&left), right.text_range().start())?;
                Some(Fragment {
                    text: format!(
                        "{}{between}{}",
                        parenthesized(&left_fragment),
                        parenthesized(&right_fragment)
                    ),
                    is_expression: left_fragment.is_expression || right_fragment.is_expression,
                    piped: false,
                })
            }
            CALL_EXPR => {
                let callee = node.first_child()?;
                if callee.kind() != NAME {
                    return None;
                }
                let function = name_text(&callee);
                if self.is_variable(&callee, &function) {
                    return None;
                }
                let arguments = arguments(&child_node(node, ARGUMENT_LIST)?);
                let mut operand = None;
                let mut removes_missing = false;
                for argument in &arguments {
                    let value = argument.value.as_ref()?;
                    match &argument.tag {
                        None if operand.is_none() => operand = Some(value.clone()),
                        // DataFrame aggregations skip missing values, as `na.rm = TRUE` does.
                        Some(tag)
                            if tag_name(tag)? == "na.rm"
                                && context == Context::Aggregate
                                && node_text(value) == "TRUE" =>
                        {
                            removes_missing = true;
                        }
                        _ => return None,
                    }
                }
                let operand = operand?;
                let inner = self.expression(&operand, Context::Row, table)?;
                if !inner.is_expression {
                    return None;
                }
                match context {
                    Context::Row if function == "is.na" && arguments.len() == 1 => Some(Fragment {
                        text: format!("Expr.is_null({})", inner.text),
                        is_expression: true,
                        piped: false,
                    }),
                    Context::Aggregate => {
                        let (_, verb) = AGGREGATIONS.iter().find(|(name, _)| *name == function)?;
                        if removes_missing && matches!(function.as_str(), "first" | "last") {
                            return None;
                        }
                        let receiver =
                            if inner.piped || matches!(operand.kind(), BINARY_EXPR | UNARY_EXPR) {
                                format!("({})", inner.text)
                            } else {
                                inner.text
                            };
                        Some(Fragment {
                            text: format!("{receiver} |> {verb}()"),
                            is_expression: true,
                            piped: true,
                        })
                    }
                    Context::Row => None,
                }
            }
            _ => None,
        }
    }

    /// Whether a bare name in a query reads a column of `table`.
    fn is_column(&self, node: &SyntaxNode, name: &str, table: &Table) -> bool {
        match table {
            Table::Other => false,
            Table::Known(columns) => columns.iter().any(|(column, _)| column == name),
            Table::Unknown => {
                !BASE_CONSTANTS.contains(&name)
                    && !QUERY_SYMBOLS.contains(&name)
                    && !self.is_variable(node, name)
            }
        }
    }

    /// Whether a name resolves to a binding of its function or of the project.
    fn is_variable(&self, node: &SyntaxNode, name: &str) -> bool {
        let resolved = self.expression_id(node).is_some_and(|id| {
            self.naming
                .is_some_and(|naming| naming.resolutions.contains_key(&id))
        });
        resolved || (self.is_project_name)(Symbol::new(name))
    }

    fn table(&self, node: &SyntaxNode) -> Table {
        let Some(ty) = self
            .expression_id(node)
            .and_then(|id| self.check?.expression_types.get(&id))
        else {
            return Table::Other;
        };
        match ty.kind(self.db) {
            TyKind::Frame(FrameKind::DataFrame | FrameKind::DataTable, Some(columns)) => {
                let mut renderer = TypeRenderer::default();
                Table::Known(
                    columns
                        .iter()
                        .map(|column| {
                            (
                                column.name.text(self.db).to_owned(),
                                renderer.render(self.db, column.ty),
                            )
                        })
                        .collect(),
                )
            }
            TyKind::Frame(FrameKind::DataFrame | FrameKind::DataTable, None) => Table::Unknown,
            TyKind::Named(name, _) if name.text(self.db) == "data.table" => Table::Unknown,
            _ => Table::Other,
        }
    }

    fn expression_id(&self, node: &SyntaxNode) -> Option<ExprId> {
        let range = node.text_range().checked_sub(self.offset)?;
        self.expressions.get(&range).copied()
    }

    /// The source text between two offsets, such as the spacing around an operator.
    fn source_between(&self, start: TextSize, end: TextSize) -> Option<&str> {
        self.text.get(usize::from(start)..usize::from(end))
    }
}

/// An argument as written: its tag, if any, and its value, if any.
struct Argument {
    tag: Option<SyntaxNode>,
    value: Option<SyntaxNode>,
}

fn arguments(list: &SyntaxNode) -> Vec<Argument> {
    list.children()
        .filter(|child| child.kind() == ARGUMENT)
        .map(|argument| {
            let named = argument
                .children_with_tokens()
                .any(|element| element.kind() == EQ);
            let mut nodes = argument.children();
            if named {
                let tag = nodes.next();
                Argument {
                    tag,
                    value: nodes.next(),
                }
            } else {
                Argument {
                    tag: None,
                    value: nodes.next(),
                }
            }
        })
        .collect()
}

/// The arguments of `.(...)` or `list(...)`, the spellings of a list of columns.
fn list_items(node: &SyntaxNode) -> Option<Vec<(Option<SyntaxNode>, SyntaxNode)>> {
    if node.kind() != CALL_EXPR {
        return None;
    }
    let callee = node.first_child()?;
    if callee.kind() != NAME || !matches!(name_text(&callee).as_str(), "." | "list") {
        return None;
    }
    arguments(&child_node(node, ARGUMENT_LIST)?)
        .into_iter()
        .map(|argument| Some((argument.tag, argument.value?)))
        .collect()
}

/// The column names a string or `c()` of strings spells, as `on` and `by` take them.
fn string_list(node: &SyntaxNode) -> Option<Vec<String>> {
    match node.kind() {
        LITERAL => Some(vec![string_value(node)?]),
        CALL_EXPR => {
            let callee = node.first_child()?;
            if callee.kind() != NAME || name_text(&callee) != "c" {
                return None;
            }
            arguments(&child_node(node, ARGUMENT_LIST)?)
                .into_iter()
                .map(|argument| {
                    if argument.tag.is_some() {
                        return None;
                    }
                    string_value(argument.value.as_ref()?)
                })
                .collect()
        }
        _ => None,
    }
}

/// The columns `:=` assigns and their values: `name := value`, `"name" := value`,
/// `c("a", "b") := list(x, y)` and `` `:=`(a = x, b = y) ``.
fn update_assignments(j: &SyntaxNode) -> Option<Vec<(String, SyntaxNode)>> {
    if j.kind() == CALL_EXPR {
        return list_arguments_named(j);
    }
    let mut operands = j.children();
    let (left, right) = (operands.next()?, operands.next()?);
    if left.kind() == NAME {
        return Some(vec![(name_text(&left), right)]);
    }
    let names = string_list(&left)?;
    match names.as_slice() {
        [name] if list_items(&right).is_none() => Some(vec![(name.clone(), right)]),
        _ => {
            let values = list_items(&right)?;
            if values.len() != names.len() || values.iter().any(|(tag, _)| tag.is_some()) {
                return None;
            }
            Some(
                names
                    .into_iter()
                    .zip(values.into_iter().map(|(_, value)| value))
                    .collect(),
            )
        }
    }
}

fn list_arguments_named(call: &SyntaxNode) -> Option<Vec<(String, SyntaxNode)>> {
    arguments(&child_node(call, ARGUMENT_LIST)?)
        .into_iter()
        .map(|argument| Some((tag_name(&argument.tag?)?, argument.value?)))
        .collect()
}

/// `j` is `name := value` or `` `:=`(...) ``.
fn is_update(j: &SyntaxNode) -> bool {
    match j.kind() {
        BINARY_EXPR => first_token(j).is_some_and(|operator| operator.kind() == COLON_EQ),
        CALL_EXPR => j
            .first_child()
            .is_some_and(|callee| callee.kind() == NAME && name_text(&callee) == ":="),
        _ => false,
    }
}

/// A column's name in the output and as written: its tag, or the name data.table gives it.
fn output_name(
    tag: Option<&SyntaxNode>,
    value: &SyntaxNode,
    position: usize,
) -> Option<(String, String)> {
    if let Some(tag) = tag {
        let name = tag_name(tag)?;
        let written = written_name(&name);
        return Some((name, written));
    }
    let name = match value.kind() {
        NAME if name_text(value) == ".N" => "N".to_owned(),
        NAME => name_text(value),
        _ => format!("V{}", position + 1),
    };
    Some((name.clone(), written_name(&name)))
}

fn render_columns(columns: &[Column]) -> String {
    let rendered: Vec<String> = columns
        .iter()
        .map(|column| format!("{} = {}", column.name, column.value))
        .collect();
    rendered.join(", ")
}

/// The operands of top-level `&`, left to right.
fn split_conjunction(node: &SyntaxNode, conjuncts: &mut Vec<SyntaxNode>) {
    if node.kind() == BINARY_EXPR
        && first_token(node).is_some_and(|operator| operator.kind() == AMP)
    {
        let mut operands = node.children();
        if let (Some(left), Some(right)) = (operands.next(), operands.next()) {
            split_conjunction(&left, conjuncts);
            split_conjunction(&right, conjuncts);
            return;
        }
    }
    conjuncts.push(node.clone());
}

/// An expression that yields `TRUE` or `FALSE` per row: a comparison, a logical combination of
/// them, or `is.na()`. A bare name or a number in `i` selects rows or joins instead.
fn is_logical(node: &SyntaxNode) -> bool {
    match node.kind() {
        BINARY_EXPR => {
            is_comparison(node)
                || first_token(node).is_some_and(|operator| {
                    matches!(operator.kind(), AMP | PIPE)
                        && node.children().all(|operand| is_logical(&operand))
                })
        }
        UNARY_EXPR => {
            first_token(node).is_some_and(|operator| operator.kind() == BANG)
                && node
                    .first_child()
                    .is_some_and(|operand| is_logical(&operand))
        }
        PAREN_EXPR => node.first_child().is_some_and(|inner| is_logical(&inner)),
        CALL_EXPR => node
            .first_child()
            .is_some_and(|callee| callee.kind() == NAME && name_text(&callee) == "is.na"),
        _ => false,
    }
}

fn is_comparison(node: &SyntaxNode) -> bool {
    node.kind() == BINARY_EXPR
        && first_token(node).is_some_and(|operator| {
            matches!(
                operator.kind(),
                LESS | GREATER | LESS_EQ | GREATER_EQ | EQ2 | BANG_EQ
            )
        })
}

fn is_literal(node: &SyntaxNode) -> bool {
    match node.kind() {
        LITERAL => true,
        UNARY_EXPR => {
            first_token(node).is_some_and(|operator| operator.kind() == MINUS)
                && node.first_child().is_some_and(|operand| {
                    operand.kind() == LITERAL
                        && first_token(&operand)
                            .is_some_and(|token| matches!(token.kind(), INTEGER | DOUBLE))
                })
        }
        _ => false,
    }
}

fn is_null(node: &SyntaxNode) -> bool {
    first_token(node).is_some_and(|token| token.kind() == NULL_KW)
}

/// The column type a literal has, rendered as the checker renders a column.
fn literal_type(node: &SyntaxNode) -> Option<&'static str> {
    let token = match node.kind() {
        LITERAL => first_token(node)?,
        UNARY_EXPR if is_literal(node) => first_token(&node.first_child()?)?,
        _ => return None,
    };
    Some(match token.kind() {
        INTEGER => "integer[]",
        DOUBLE => "double[]",
        STRING => "character[]",
        TRUE_KW | FALSE_KW => "logical[]",
        _ => return None,
    })
}

/// A statement whose value nothing reads, outside any function.
fn is_top_level_statement(node: &SyntaxNode) -> bool {
    node.parent()
        .is_some_and(|parent| matches!(parent.kind(), SOURCE_FILE | BRACE_EXPR))
        && !node
            .ancestors()
            .any(|ancestor| ancestor.kind() == FUNCTION_DEF)
}

/// Whether a `|>` chain replacing `node` must be parenthesized to keep its place in the
/// enclosing expression, as the operand of `$`, `[` or an arithmetic operator.
fn needs_parentheses(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        SOURCE_FILE | BRACE_EXPR | PAREN_EXPR | ARGUMENT | IF_EXPR | FOR_EXPR | WHILE_EXPR
        | REPEAT_EXPR | FUNCTION_DEF => false,
        BINARY_EXPR => {
            let is_first = parent.first_child().as_ref() == Some(node);
            match first_token(&parent).map(|operator| operator.kind()) {
                Some(LESS_MINUS | LESS2_MINUS | EQ) => is_first,
                Some(MINUS_GREATER | MINUS_GREATER2 | PIPE_GREATER) => !is_first,
                _ => true,
            }
        }
        _ => true,
    }
}

fn parenthesized(fragment: &Fragment) -> String {
    if fragment.piped {
        format!("({})", fragment.text)
    } else {
        fragment.text.clone()
    }
}

/// A node's first significant token of its own: the operator of a binary or unary expression,
/// the bracket of an argument list, the token of a literal.
fn first_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| !token.kind().is_trivia())
}

fn child_node(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children().find(|child| child.kind() == kind)
}

/// A node's text without the trailing whitespace the parser attaches to an operand that an
/// operator follows.
fn node_text(node: &SyntaxNode) -> String {
    let mut text = node.text().to_string();
    text.truncate(text.trim_end().len());
    text
}

fn trimmed_end(node: &SyntaxNode) -> TextSize {
    node.text_range().start() + TextSize::of(node_text(node).as_str())
}

/// A name's spelling without backticks.
fn name_text(node: &SyntaxNode) -> String {
    let text = node_text(node);
    match text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
    {
        Some(inner) => inner.to_owned(),
        None => text,
    }
}

/// An argument tag's name: a name or a string.
fn tag_name(tag: &SyntaxNode) -> Option<String> {
    match tag.kind() {
        NAME => Some(name_text(tag)),
        LITERAL => string_value(tag),
        _ => None,
    }
}

/// A plain string literal's value. One with escapes is left alone rather than decoded.
fn string_value(node: &SyntaxNode) -> Option<String> {
    let token = first_token(node)?;
    if token.kind() != STRING {
        return None;
    }
    let text = token.text();
    let inner = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            text.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })?;
    (!inner.contains('\\')).then(|| inner.to_owned())
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A name as an argument tag: bare when R reads it as a name, backticked otherwise.
fn written_name(name: &str) -> String {
    if is_syntactic_name(name) {
        name.to_owned()
    } else {
        format!("`{}`", name.replace('`', "\\`"))
    }
}
