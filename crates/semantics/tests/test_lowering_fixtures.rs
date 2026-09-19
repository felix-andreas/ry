//! Lowering fixtures: the HIR an item lowers to.
//!
//! Lowering is where R's surface syntax becomes the shape the checker reasons
//! about, and several of those steps are real rewrites rather than a change of
//! representation. A native pipe becomes a call with the left side spliced in
//! as the first argument, a replacement form becomes a read of the base
//! followed by a write back to it, and `local({…})` becomes its own
//! construct. Every
//! one of those is currently tested only through whatever type falls out the far
//! end, which cannot distinguish "lowered to the wrong shape but coincidentally
//! typed the same" from "lowered correctly".
//!
//! This is the analogue of the parse-tree dump one layer down: the syntax suite
//! pins what the parser built, and this pins what lowering made of it. Ranges
//! are item-relative and printed only where a case needs them, so the tree stays
//! readable.
//!
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use semantics::hir::{Argument, ExprId, ExpressionKind, Module, Parameter};
use semantics::{
    DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, item_hir, item_tree,
};
use std::fmt::Write as _;
use std::path::PathBuf;

#[test]
fn lowering_fixtures() {
    let suite = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/lowering");
    syntax::testing::run_fixture_suite(&suite, &render);
}

fn render(source: &str) -> String {
    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);

    let mut output = String::new();
    for &item in item_tree(&db, file) {
        let Some(module) = item_hir(&db, item).as_ref() else {
            continue;
        };
        let header = match item.name(&db).clone() {
            Some(name) => name,
            None => format!("<{}>", item_kind(*item.kind(&db))),
        };
        let _ = writeln!(output, "# {header}");
        match module.root {
            Some(root) => render_expression(&mut output, module, root, 1),
            None => {
                let _ = writeln!(output, "  <no root>");
            }
        }
    }
    output
}

fn render_expression(output: &mut String, module: &Module, id: ExprId, depth: usize) {
    let indent = "  ".repeat(depth);
    let expression = module.expression(id);
    match &expression.kind {
        ExpressionKind::Missing => {
            let _ = writeln!(output, "{indent}Missing");
        }
        ExpressionKind::Literal(literal) => {
            let _ = writeln!(output, "{indent}Literal {literal:?}");
        }
        ExpressionKind::NameRef(name) => {
            let _ = writeln!(output, "{indent}NameRef {name}");
        }
        ExpressionKind::Assign {
            spelling,
            target,
            value,
        } => {
            let _ = writeln!(output, "{indent}Assign {spelling:?}");
            render_expression(output, module, *target, depth + 1);
            render_expression(output, module, *value, depth + 1);
        }
        ExpressionKind::Unary { operator, operand } => {
            let _ = writeln!(output, "{indent}Unary {operator:?}");
            render_expression(output, module, *operand, depth + 1);
        }
        ExpressionKind::Binary {
            operator,
            special_name,
            lhs,
            rhs,
        } => {
            match special_name {
                Some(name) => {
                    let _ = writeln!(output, "{indent}Binary {operator:?} {name}");
                }
                None => {
                    let _ = writeln!(output, "{indent}Binary {operator:?}");
                }
            }
            render_expression(output, module, *lhs, depth + 1);
            render_expression(output, module, *rhs, depth + 1);
        }
        ExpressionKind::Call { callee, arguments } => {
            let _ = writeln!(output, "{indent}Call");
            render_expression(output, module, *callee, depth + 1);
            render_arguments(output, module, arguments, depth + 1);
        }
        ExpressionKind::Index {
            double,
            target,
            arguments,
        } => {
            let bracket = if *double { "[[" } else { "[" };
            let _ = writeln!(output, "{indent}Index {bracket}");
            render_expression(output, module, *target, depth + 1);
            render_arguments(output, module, arguments, depth + 1);
        }
        ExpressionKind::Field {
            at, target, name, ..
        } => {
            let accessor = if *at { "@" } else { "$" };
            let spelled = name.clone().unwrap_or_else(|| "?".to_owned());
            let _ = writeln!(output, "{indent}Field {accessor}{spelled}");
            render_expression(output, module, *target, depth + 1);
        }
        ExpressionKind::Namespace {
            internal,
            package,
            name,
        } => {
            let separator = if *internal { ":::" } else { "::" };
            let _ = writeln!(
                output,
                "{indent}Namespace {}{separator}{}",
                package.clone().unwrap_or_else(|| "?".to_owned()),
                name.clone().unwrap_or_else(|| "?".to_owned())
            );
        }
        ExpressionKind::Function { parameters, body } => {
            let _ = writeln!(output, "{indent}Function({})", spell(parameters));
            render_expression(output, module, *body, depth + 1);
        }
        ExpressionKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            let _ = writeln!(output, "{indent}If");
            render_expression(output, module, *condition, depth + 1);
            render_expression(output, module, *then_branch, depth + 1);
            if let Some(otherwise) = else_branch {
                render_expression(output, module, *otherwise, depth + 1);
            }
        }
        ExpressionKind::For {
            variable,
            sequence,
            body,
            ..
        } => {
            let _ = writeln!(
                output,
                "{indent}For {}",
                variable.clone().unwrap_or_else(|| "?".to_owned())
            );
            render_expression(output, module, *sequence, depth + 1);
            render_expression(output, module, *body, depth + 1);
        }
        ExpressionKind::While { condition, body } => {
            let _ = writeln!(output, "{indent}While");
            render_expression(output, module, *condition, depth + 1);
            render_expression(output, module, *body, depth + 1);
        }
        ExpressionKind::Repeat { body } => {
            let _ = writeln!(output, "{indent}Repeat");
            render_expression(output, module, *body, depth + 1);
        }
        ExpressionKind::Local { body } => {
            let _ = writeln!(output, "{indent}Local");
            render_expression(output, module, *body, depth + 1);
        }
        ExpressionKind::Block {
            statements,
            trailing_semicolon,
        } => {
            let _ = writeln!(
                output,
                "{indent}Block{}",
                if *trailing_semicolon {
                    " trailing-semicolon"
                } else {
                    ""
                }
            );
            for &statement in statements {
                render_expression(output, module, statement, depth + 1);
            }
        }
        ExpressionKind::Paren(inner) => {
            let _ = writeln!(output, "{indent}Paren");
            render_expression(output, module, *inner, depth + 1);
        }
        ExpressionKind::Break => {
            let _ = writeln!(output, "{indent}Break");
        }
        ExpressionKind::Next => {
            let _ = writeln!(output, "{indent}Next");
        }
    }
}

/// An argument renders its name inline, because argument *matching* is what a
/// call's lowering has to get right and a bare list of values does not show it.
fn render_arguments(output: &mut String, module: &Module, arguments: &[Argument], depth: usize) {
    let indent = "  ".repeat(depth);
    for argument in arguments {
        match (&argument.name, argument.value) {
            (Some(name), Some(value)) => {
                let _ = writeln!(output, "{indent}arg {name} =");
                render_expression(output, module, value, depth + 1);
            }
            (Some(name), None) => {
                let _ = writeln!(output, "{indent}arg {name} = <empty>");
            }
            (None, Some(value)) => render_expression(output, module, value, depth),
            (None, None) => {
                let _ = writeln!(output, "{indent}<empty>");
            }
        }
    }
}

fn spell(parameters: &[Parameter]) -> String {
    parameters
        .iter()
        .map(|parameter| match parameter.default {
            Some(_) => format!("{} = …", parameter.name),
            None => parameter.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn item_kind(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Function => "function",
        ItemKind::Value => "value",
        ItemKind::Statement => "statement",
    }
}
