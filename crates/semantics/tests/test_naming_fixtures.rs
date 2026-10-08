//! Name-resolution fixtures: which binding every name in an item resolves to.
//!
//! This is the only suite that tests naming *directly*. Everywhere else naming
//! is tested through its consequences, which are a type that comes out right
//! and a diagnostic that does or does not fire. Those consequences cannot see a
//! read that resolves to the **wrong** binding while still producing the same
//! type. That failure is silent in every other suite, and it is a real one. The
//! rule for which writer an immediate read sees is the nearest earlier one,
//! rather than the first or the last, and it is invisible unless the bindings
//! differ in type.
//!
//! The rendering is flat and source-ordered rather than a tree of the HIR. The
//! fact under test is the resolution, not the shape, and a tree renderer would
//! restate the lowering, which would churn this suite on every HIR change
//! while adding nothing about names. Sections appear only when they have content, so an
//! ordinary case stays short.
//!
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use semantics::hir::{ExprId, Module};
use semantics::naming::{BindingKind, ItemNaming};
use semantics::{
    DocumentKind, ItemKind, ProjectFiles, RootDatabase, SourceFile, item_hir, item_naming,
    item_tree,
};
use std::fmt::Write as _;
use std::path::PathBuf;

#[test]
fn naming_fixtures() {
    let suite = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/naming");
    syntax::testing::run_fixture_suite(&suite, &render);
}

fn render(source: &str) -> String {
    let db = RootDatabase::default();
    semantics::stubs::install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.to_owned(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);

    let mut output = String::new();
    for &item in item_tree(&db, file) {
        let (Some(naming), Some(module)) = (
            item_naming(&db, item).as_ref(),
            item_hir(&db, item).as_ref(),
        ) else {
            continue;
        };
        let header = match item.name(&db).clone() {
            Some(name) => name,
            None => format!("<{}>", item_kind(*item.kind(&db))),
        };
        let _ = writeln!(output, "# {header}");
        render_item(&mut output, naming, module);
    }
    output
}

/// One item's naming facts. Bindings first, because every resolution below
/// points at one; then reads in source order; then the sets that are empty in
/// an ordinary case.
fn render_item(output: &mut String, naming: &ItemNaming, module: &Module) {
    for binding in naming.bindings.values() {
        let _ = writeln!(
            output,
            "  b{} {} {} {}",
            binding.id.0,
            binding.name,
            binding_kind(binding.kind),
            span(binding.range),
        );
    }

    // A resolution sits on a read or on an assignment target, and telling them
    // apart is the point of the suite: "this write and that read share a slot"
    // is the fact, and a rendering that shows only `-> b0` twice does not say
    // which direction each occurrence goes.
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
        let name = read.name.clone().unwrap_or_else(|| "?".to_owned());
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
        let _ = writeln!(output, "{line}");
    }

    render_set(output, "operator reads", naming.quiet_operator_reads.iter());
    render_set(output, "super-globals", naming.super_globals.iter());
    render_set(output, "used top-level", naming.used_top_level_names.iter());
    for unused in &naming.unused_assignments {
        let _ = writeln!(output, "  unused {} {}", unused.name, span(unused.range));
    }
    for unused in &naming.unused_parameters {
        let _ = writeln!(
            output,
            "  unused-parameter {} {}",
            unused.name,
            span(unused.range)
        );
    }
    for target in &naming.invalid_assignment_targets {
        let _ = writeln!(output, "  invalid-target {}", span(target.range));
    }
    for &expression in &naming.masked_subsets {
        let _ = writeln!(
            output,
            "  masked-subset {}",
            span(module.expression(expression).range)
        );
    }
}

fn render_set<'a>(output: &mut String, label: &str, values: impl Iterator<Item = &'a String>) {
    let joined: Vec<&str> = values.map(String::as_str).collect();
    if !joined.is_empty() {
        let _ = writeln!(output, "  {label}: {}", joined.join(", "));
    }
}

/// Every expression that is the target of an assignment, so a resolution on one
/// renders as a write rather than a read.
fn assignment_targets(module: &Module) -> std::collections::BTreeSet<ExprId> {
    use semantics::hir::ExpressionKind;
    let mut targets = std::collections::BTreeSet::new();
    for expression in &module.expressions {
        if let ExpressionKind::Assign { target, .. } = &expression.kind {
            targets.insert(*target);
        }
    }
    targets
}

/// The name an expression spells, for the resolution lines. A resolution can
/// sit on an assignment target as well as a read, and a target is usually a
/// `NameRef`. A quoted target is the exception: `"x" <- 1` binds `x`, but
/// lowering keeps the string literal, so the occurrence renders as its
/// expression kind while the binding line above it carries the stripped name.
fn name_at(module: &Module, expression: ExprId) -> String {
    match &module.expression(expression).kind {
        semantics::hir::ExpressionKind::NameRef(name) => name.clone(),
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

fn span(range: syntax::TextRange) -> String {
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

fn item_kind(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Function => "function",
        ItemKind::Value => "value",
        ItemKind::Statement => "statement",
    }
}
