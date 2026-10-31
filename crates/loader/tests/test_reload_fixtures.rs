//! Reload fixtures: the plan that takes a loaded package from its `before` sources to its
//! `after` sources. A `#~~~~ before <path>` or `#~~~~ after <path>` file exists on one side
//! only, so a path on both sides is an edit, on one side an addition or a deletion, and a
//! plain `#~~~~ <path>` file is the same on both sides.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use loader::{Loader, PackageInputs, Plan, SysdataChange};
use std::path::{Path, PathBuf};
use syntax::testing::{run_fixture_suite, split_files};

#[test]
fn reload_fixtures() {
    let suite = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/reload");
    run_fixture_suite(&suite, &render);
}

fn render(source: &str) -> String {
    let files = split_files(source).expect("a reload case lists its files");
    let side = |wanted: &str| {
        let mut inputs = PackageInputs {
            description: "Package: demo\nVersion: 0.1.0\n".to_owned(),
            ..PackageInputs::default()
        };
        for (path, text) in &files {
            let path = match path.split_once(' ') {
                Some((side, path)) if side == wanted => path,
                Some(_) => continue,
                None => path.as_str(),
            };
            match path {
                "DESCRIPTION" => inputs.description = text.clone(),
                "NAMESPACE" => inputs.namespace = text.clone(),
                _ => inputs.files.push((path.to_owned(), Some(text.clone()))),
            }
        }
        inputs.files.sort_by(|a, b| a.0.cmp(&b.0));
        inputs
    };
    let root = Path::new("/fixture");
    let mut loader = Loader::default();
    let before = side("before");
    loader
        .plan_inputs(root, before, false)
        .expect("the before side plans");
    loader.commit(root);
    let after = side("after");
    let after_texts = after.files.clone();
    let plan = loader
        .plan_inputs(root, after, false)
        .expect("the after side plans");
    render_plan(&plan, &after_texts)
}

fn render_plan(plan: &Plan, texts: &[(String, Option<String>)]) -> String {
    let text_of = |path: &str| {
        texts
            .iter()
            .find(|(candidate, _)| candidate == path)
            .and_then(|(_, text)| text.clone())
            .unwrap_or_default()
    };
    let mut output = String::new();
    if plan.is_noop() {
        output.push_str("nothing to do\n");
    }
    if let Some(imports) = &plan.imports {
        output.push_str(&format!("rebuild imports ({} directives)\n", imports.len()));
    }
    for source in &plan.sources {
        output.push_str(&format!("source {}\n", source.path));
    }
    for path in &plan.dropped_sources {
        output.push_str(&format!("drop source {path}\n"));
    }
    for name in &plan.remove {
        output.push_str(&format!("remove {name}\n"));
    }
    for registration in &plan.unregister {
        output.push_str(&format!("unregister {registration:?}\n"));
    }
    for code in &plan.bind {
        output.push_str(&format!(
            "bind {} from {}:{}:{}\n",
            code.name.as_deref().unwrap_or_default(),
            code.path,
            code.line,
            code.column
        ));
    }
    for code in &plan.run {
        let text = text_of(&code.path);
        let snippet: String = text
            .chars()
            .skip(code.start as usize)
            .take((code.end - code.start) as usize)
            .collect();
        let first_line = snippet.lines().next().unwrap_or_default();
        output.push_str(&format!("run {}:{} {first_line}\n", code.path, code.line));
    }
    if plan.sysdata != SysdataChange::Keep {
        output.push_str(&format!("sysdata {:?}\n", plan.sysdata));
    }
    for method in &plan.s3_unregister {
        output.push_str(&format!(
            "unregister S3 {}.{}\n",
            method.generic, method.class
        ));
    }
    for method in &plan.s3_register {
        output.push_str(&format!(
            "register S3 {}.{} as {}\n",
            method.generic, method.class, method.function
        ));
    }
    if plan.exports_changed {
        output.push_str("sync exports\n");
    }
    if plan.run_on_load {
        output.push_str("run .onLoad\n");
    }
    for warning in &plan.warnings {
        output.push_str(&format!("warning {warning}\n"));
    }
    output
}
