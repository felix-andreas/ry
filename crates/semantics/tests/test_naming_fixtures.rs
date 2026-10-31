//! Name-resolution fixtures: the only suite that sees a read resolving to the wrong binding
//! while still producing the same type.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use semantics::hir::{ExprId, ExpressionKind, Module, Symbol};
use semantics::naming::{BindingKind, ItemNaming};
use semantics::testing::with_fixture_project;
use semantics::{DocumentKind, item_hir, item_naming, item_tree};
use std::collections::BTreeSet;
use std::path::PathBuf;
use syntax::TextRange;
use syntax::testing::run_fixture_suite;

#[test]
fn naming_fixtures() {
    let suite = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/naming");
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
                let (Some(naming), Some(module)) =
                    (item_naming(db, item).as_ref(), item_hir(db, item).as_ref())
                else {
                    continue;
                };
                let header = match item.name(db).clone() {
                    Some(name) => name,
                    None => format!("<{:?}>", item.kind(db)).to_lowercase(),
                };
                output.push_str(&format!("# {header}\n"));
                render_item(&mut output, naming, module);
            }
            output
        },
    )
}

fn render_item(output: &mut String, naming: &ItemNaming, module: &Module) {
    for (id, binding) in &naming.bindings {
        output.push_str(&format!(
            "  b{} {} {} {}\n",
            id.0,
            binding.name,
            binding_kind(binding.kind),
            span(binding.range),
        ));
    }

    // Writes and reads must render differently: two `-> b0` lines would not say which is which.
    let targets = assignment_targets(module);

    let mut reads: Vec<(u32, String)> = Vec::new();
    for (&expression, &binding) in &naming.resolutions {
        let mut line = format!(
            "  {} {} {} b{}",
            span(module.expression(expression).range),
            name_at(module, expression),
            if targets.contains(&expression) {
                "<-"
            } else {
                "->"
            },
            binding.0
        );
        if naming.maybe_undefined.contains(&expression) {
            line.push_str(" maybe-undefined");
        }
        if naming.captured_slots.contains(&binding) {
            line.push_str(" captured");
        }
        reads.push((start_of(module, expression), line));
    }
    for (&expression, name) in &naming.non_locals {
        let mut line = format!(
            "  {} {name} -> non-local",
            span(module.expression(expression).range)
        );
        if naming.deferred_non_locals.contains(&expression) {
            line.push_str(" deferred");
        }
        reads.push((start_of(module, expression), line));
    }
    for (&expression, name) in &naming.quiet_reads {
        let mut line = format!(
            "  {} {name} -> quiet",
            span(module.expression(expression).range)
        );
        if naming.deferred_quiet_reads.contains(&expression) {
            line.push_str(" deferred");
        }
        reads.push((start_of(module, expression), line));
    }
    for (&expression, read) in &naming.namespace_reads {
        let separator = if read.internal { ":::" } else { "::" };
        let name = read.name.map_or_else(|| "?".to_owned(), |name| name.to_string());
        reads.push((
            start_of(module, expression),
            format!(
                "  {} {}{separator}{name} -> namespace",
                span(module.expression(expression).range),
                read.package
            ),
        ));
    }
    reads.sort();
    for (_, line) in reads {
        output.push_str(&format!("{line}\n"));
    }

    render_set(output, "operator reads", naming.quiet_operator_reads.iter());
    render_set(output, "super-globals", naming.super_globals.iter());
    render_set(output, "used top-level", naming.used_top_level_names.iter());
    for unused in &naming.unused_assignments {
        output.push_str(&format!(
            "  unused {} {}\n",
            unused.name,
            span(unused.range)
        ));
    }
    for unused in &naming.unused_parameters {
        output.push_str(&format!(
            "  unused-parameter {} {}\n",
            unused.name,
            span(unused.range)
        ));
    }
    for target in &naming.invalid_assignment_targets {
        output.push_str(&format!("  invalid-target {}\n", span(target.range)));
    }
    for &expression in &naming.masked_subsets {
        output.push_str(&format!(
            "  masked-subset {}\n",
            span(module.expression(expression).range)
        ));
    }
}

fn render_set<'a>(output: &mut String, label: &str, values: impl Iterator<Item = &'a Symbol>) {
    let joined: Vec<&str> = values.map(|value| value.as_str()).collect();
    if !joined.is_empty() {
        output.push_str(&format!("  {label}: {}\n", joined.join(", ")));
    }
}

fn assignment_targets(module: &Module) -> BTreeSet<ExprId> {
    module
        .expressions
        .iter()
        .filter_map(|expression| match expression.kind {
            ExpressionKind::Assign { target, .. } => Some(target),
            _ => None,
        })
        .collect()
}

/// A quoted target (`"x" <- 1`) lowers to a string literal, so it renders as its kind.
fn name_at(module: &Module, expression: ExprId) -> String {
    match &module.expression(expression).kind {
        ExpressionKind::NameRef(name) => name.to_string(),
        other => format!("{other:?}")
            .split(['(', ' '])
            .next()
            .unwrap_or("?")
            .to_owned(),
    }
}

fn start_of(module: &Module, expression: ExprId) -> u32 {
    module.expression(expression).range.start().into()
}

fn span(range: TextRange) -> String {
    format!("{}..{}", u32::from(range.start()), u32::from(range.end()))
}

fn binding_kind(kind: BindingKind) -> &'static str {
    match kind {
        BindingKind::TopLevel => "top-level",
        BindingKind::Local => "local",
        BindingKind::Parameter => "parameter",
        BindingKind::ForVariable => "for-variable",
    }
}
