//! Pins the HIR shape, which typed output cannot tell apart from a wrong lowering that
//! happens to type the same.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use semantics::hir::{Argument, ExprId, ExpressionKind, Module, PatId, PatternKind};
use semantics::testing::with_fixture_project;
use semantics::{DocumentKind, item_hir, item_tree};
use std::path::PathBuf;
use syntax::testing::run_fixture_suite;

#[test]
fn lowering_fixtures() {
    let suite = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/lowering");
    run_fixture_suite(&suite, &render);
}

fn render(source: &str) -> String {
    with_fixture_project(
        vec![(source.to_owned(), DocumentKind::Package)],
        |db, files| {
            let &[file] = files else {
                panic!("a fixture case is one file");
            };
            let mut output = String::new();
            for &item in item_tree(db, file) {
                let Some(module) = item_hir(db, item).as_ref() else {
                    continue;
                };
                let header = match item.name(db).clone() {
                    Some(name) => name,
                    None => format!("<{:?}>", item.kind(db)).to_lowercase(),
                };
                output.push_str(&format!("# {header}\n"));
                match module.root {
                    Some(root) => render_expression(&mut output, module, root, 1),
                    None => output.push_str("  <no root>\n"),
                }
            }
            output
        },
    )
}

fn render_expression(output: &mut String, module: &Module, id: ExprId, depth: usize) {
    let kind = &module.expression(id).kind;
    let label = match kind {
        ExpressionKind::Missing => "Missing".to_owned(),
        ExpressionKind::Literal(literal) => format!("Literal {literal:?}"),
        ExpressionKind::NameRef(name) => format!("NameRef {name}"),
        ExpressionKind::Assign { spelling, .. } => format!("Assign {spelling:?}"),
        ExpressionKind::Unary { operator, .. } => format!("Unary {operator:?}"),
        ExpressionKind::Binary {
            operator,
            special_name: Some(name),
            ..
        } => format!("Binary {operator:?} {name}"),
        ExpressionKind::Binary { operator, .. } => format!("Binary {operator:?}"),
        ExpressionKind::Call { .. } => "Call".to_owned(),
        ExpressionKind::Index { double, .. } => {
            format!("Index {}", if *double { "[[" } else { "[" })
        }
        ExpressionKind::Field { at, name, .. } => format!(
            "Field {}{}",
            if *at { "@" } else { "$" },
            name.as_deref().unwrap_or("?")
        ),
        ExpressionKind::Namespace {
            internal,
            package,
            name,
        } => format!(
            "Namespace {}{}{}",
            package.as_deref().unwrap_or("?"),
            if *internal { ":::" } else { "::" },
            name.as_deref().unwrap_or("?")
        ),
        ExpressionKind::Function { parameters, .. } => {
            let spelled: Vec<String> = parameters
                .iter()
                .map(|parameter| match parameter.default {
                    Some(_) => format!("{} = …", parameter.name),
                    None => parameter.name.to_string(),
                })
                .collect();
            format!("Function({})", spelled.join(", "))
        }
        ExpressionKind::If { .. } => "If".to_owned(),
        ExpressionKind::For { variable, .. } => {
            format!("For {}", variable.as_deref().unwrap_or("?"))
        }
        ExpressionKind::While { .. } => "While".to_owned(),
        ExpressionKind::Repeat { .. } => "Repeat".to_owned(),
        ExpressionKind::Local { .. } => "Local".to_owned(),
        ExpressionKind::Block { .. } => "Block".to_owned(),
        ExpressionKind::Paren(_) => "Paren".to_owned(),
        ExpressionKind::Break => "Break".to_owned(),
        ExpressionKind::Next => "Next".to_owned(),
        ExpressionKind::Tuple(_) => "Tuple".to_owned(),
        ExpressionKind::Array(_) => "Array".to_owned(),
        ExpressionKind::Record(fields) => {
            let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
            format!("Record({})", names.join(", "))
        }
        ExpressionKind::Tag { name, .. } => format!("Tag :{name}"),
        ExpressionKind::Match { .. } => "Match".to_owned(),
    };
    output.push_str(&format!("{}{label}\n", "  ".repeat(depth)));
    match kind {
        ExpressionKind::Call {
            callee: head,
            arguments,
        }
        | ExpressionKind::Index {
            target: head,
            arguments,
            ..
        } => {
            render_expression(output, module, *head, depth + 1);
            render_arguments(output, module, arguments, depth + 1);
        }
        // Parameter defaults are spelled in the label, not rendered as children.
        ExpressionKind::Function { body, .. } => {
            render_expression(output, module, *body, depth + 1);
        }
        ExpressionKind::Match { scrutinee, arms } => {
            render_expression(output, module, *scrutinee, depth + 1);
            let indent = "  ".repeat(depth + 1);
            for arm in arms {
                output.push_str(&format!("{indent}arm\n"));
                render_pattern(output, module, arm.pattern, depth + 2);
                if let Some(guard) = arm.guard {
                    output.push_str(&format!("{indent}  if\n"));
                    render_expression(output, module, guard, depth + 3);
                }
                render_expression(output, module, arm.body, depth + 2);
            }
        }
        _ => {
            for child in kind.child_ids() {
                render_expression(output, module, child, depth + 1);
            }
        }
    }
}

fn render_pattern(output: &mut String, module: &Module, id: PatId, depth: usize) {
    let indent = "  ".repeat(depth);
    match &module.pattern(id).kind {
        PatternKind::Missing => output.push_str(&format!("{indent}pattern Missing\n")),
        PatternKind::Wildcard => output.push_str(&format!("{indent}pattern _\n")),
        PatternKind::Bind(name) => {
            let ExpressionKind::NameRef(name) = &module.expression(*name).kind else {
                panic!("a binding leaf is a name");
            };
            output.push_str(&format!("{indent}pattern bind {name}\n"));
        }
        PatternKind::Literal { literal, negative } => output.push_str(&format!(
            "{indent}pattern literal {}{literal:?}\n",
            if *negative { "-" } else { "" }
        )),
        PatternKind::Tag { name, payload, .. } => {
            output.push_str(&format!("{indent}pattern tag :{name}\n"));
            if let Some(payload) = payload {
                render_pattern(output, module, *payload, depth + 1);
            }
        }
        PatternKind::Tuple(items) => {
            output.push_str(&format!("{indent}pattern tuple\n"));
            for item in items {
                render_pattern(output, module, *item, depth + 1);
            }
        }
        PatternKind::Record { fields, rest } => {
            output.push_str(&format!(
                "{indent}pattern Record{}\n",
                if *rest { " .." } else { "" }
            ));
            for field in fields {
                output.push_str(&format!("{indent}  field {} =\n", field.name));
                render_pattern(output, module, field.pattern, depth + 2);
            }
        }
        PatternKind::Or(alternatives) => {
            output.push_str(&format!("{indent}pattern or\n"));
            for alternative in alternatives {
                render_pattern(output, module, *alternative, depth + 1);
            }
        }
    }
}

fn render_arguments(output: &mut String, module: &Module, arguments: &[Argument], depth: usize) {
    let indent = "  ".repeat(depth);
    for argument in arguments {
        match (&argument.name, argument.value) {
            (Some(name), Some(value)) => {
                output.push_str(&format!("{indent}arg {name} =\n"));
                render_expression(output, module, value, depth + 1);
            }
            (Some(name), None) => output.push_str(&format!("{indent}arg {name} = <empty>\n")),
            (None, Some(value)) => render_expression(output, module, value, depth),
            (None, None) => output.push_str(&format!("{indent}<empty>\n")),
        }
    }
}
