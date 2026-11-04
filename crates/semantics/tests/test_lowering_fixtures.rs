//! Pins the HIR shape, which typed output cannot tell apart from a wrong lowering that
//! happens to type the same.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use {
    semantics::{
        DocumentKind,
        hir::{Argument, ExprId, ExpressionKind, LiteralKind, Module, PatId, PatternKind, Rest},
        item_hir, item_tree,
        testing::with_fixture_project,
    },
    std::path::PathBuf,
    syntax::testing::run_fixture_suite,
};

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
    let kind = module.kind(id);
    let label = match kind {
        ExpressionKind::Missing => "Missing".to_owned(),
        ExpressionKind::Literal(literal) => format!("Literal {}", literal_text(module, literal)),
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
            name.map(|name| module.name(name)).as_deref().unwrap_or("?")
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
            let spelled: Vec<String> = module[parameters]
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
            let variable = variable.map(|variable| module.name(variable));
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
            let names: Vec<&str> = module[fields]
                .iter()
                .map(|field| field.name.as_str())
                .collect();
            format!("Record({})", names.join(", "))
        }
        ExpressionKind::RecordUpdate { fields, .. } => {
            let names: Vec<&str> = module[fields]
                .iter()
                .map(|field| field.name.as_str())
                .collect();
            format!("RecordUpdate({})", names.join(", "))
        }
        ExpressionKind::Tag { name, .. } => format!("Tag :{}", module.name(*name)),
        ExpressionKind::Match { .. } => "Match".to_owned(),
        ExpressionKind::FormatString(_) => "FormatString".to_owned(),
        ExpressionKind::Try(_) => "Try".to_owned(),
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
            render_arguments(output, module, &module[arguments], depth + 1);
        }
        // Parameter defaults are spelled in the label, not rendered as children.
        ExpressionKind::Function { body, .. } => {
            render_expression(output, module, *body, depth + 1);
        }
        ExpressionKind::Match { scrutinee, arms } => {
            render_expression(output, module, *scrutinee, depth + 1);
            let indent = "  ".repeat(depth + 1);
            for arm in &module[arms] {
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
            module.for_each_child(id, |child| {
                render_expression(output, module, child, depth + 1)
            });
        }
    }
}

fn render_pattern(output: &mut String, module: &Module, id: PatId, depth: usize) {
    let indent = "  ".repeat(depth);
    match &module.pattern(id).kind {
        PatternKind::Missing => output.push_str(&format!("{indent}pattern Missing\n")),
        PatternKind::Wildcard => output.push_str(&format!("{indent}pattern _\n")),
        PatternKind::Bind(name) => {
            let ExpressionKind::NameRef(name) = module.kind(*name) else {
                panic!("a binding leaf is a name");
            };
            output.push_str(&format!("{indent}pattern bind {name}\n"));
        }
        PatternKind::Literal { literal, negative } => output.push_str(&format!(
            "{indent}pattern literal {}{}\n",
            if *negative { "-" } else { "" },
            literal_text(module, literal)
        )),
        PatternKind::Tag { name, payload, .. } => {
            let name = module.name(*name);
            output.push_str(&format!("{indent}pattern tag :{name}\n"));
            if let Some(payload) = payload {
                render_pattern(output, module, *payload, depth + 1);
            }
        }
        PatternKind::Tuple(items) => {
            output.push_str(&format!("{indent}pattern tuple\n"));
            for item in &module[items] {
                render_pattern(output, module, *item, depth + 1);
            }
        }
        PatternKind::Record { fields, rest } => {
            output.push_str(&format!(
                "{indent}pattern Record{}\n",
                rest.map(|rest| format!(" {}", rest_text(module, rest)))
                    .unwrap_or_default()
            ));
            for field in &module[fields] {
                output.push_str(&format!("{indent}  field {} =\n", field.name));
                render_pattern(output, module, field.pattern, depth + 2);
            }
        }
        PatternKind::List { items, rest } => {
            output.push_str(&format!("{indent}pattern list\n"));
            let items = &module[items];
            let position = rest.map_or(items.len(), |rest| rest.position as usize);
            for (index, item) in items.iter().enumerate() {
                if index == position
                    && let Some(rest) = rest
                {
                    output.push_str(&format!("{indent}  {}\n", rest_text(module, rest.rest)));
                }
                render_pattern(output, module, *item, depth + 1);
            }
            if let Some(rest) = rest.filter(|rest| rest.position as usize >= items.len()) {
                output.push_str(&format!("{indent}  {}\n", rest_text(module, rest.rest)));
            }
        }
        PatternKind::As { pattern, binding } => {
            let ExpressionKind::NameRef(name) = module.kind(*binding) else {
                panic!("an `as` binding is a name");
            };
            output.push_str(&format!("{indent}pattern as {name}\n"));
            render_pattern(output, module, *pattern, depth + 1);
        }
        PatternKind::Or(alternatives) => {
            output.push_str(&format!("{indent}pattern or\n"));
            for alternative in &module[alternatives] {
                render_pattern(output, module, *alternative, depth + 1);
            }
        }
    }
}

fn render_arguments(output: &mut String, module: &Module, arguments: &[Argument], depth: usize) {
    let indent = "  ".repeat(depth);
    for argument in arguments {
        match (module.argument_name(argument), argument.value) {
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

fn rest_text(module: &Module, rest: Rest) -> String {
    match rest.binding.map(|binding| module.kind(binding)) {
        Some(ExpressionKind::NameRef(name)) => format!("..{name}"),
        _ => "..".to_owned(),
    }
}

/// Renders a literal with its text resolved, as `Integer("1L")`.
fn literal_text(module: &Module, literal: &LiteralKind) -> String {
    match literal {
        LiteralKind::Integer(text) => format!("Integer({:?})", &module[text]),
        LiteralKind::Double(text) => format!("Double({:?})", &module[text]),
        LiteralKind::Complex(text) => format!("Complex({:?})", &module[text]),
        LiteralKind::String(text) => format!("String({:?})", &module[text]),
        LiteralKind::Suffixed { number, ty } => format!("Suffixed({:?}, {ty:?})", &module[number]),
        other => format!("{other:?}"),
    }
}
