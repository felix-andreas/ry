//! Reload fixtures: the plan that takes a loaded package from its `before` sources to its
//! `after` sources. A `#~~~~ before <path>` or `#~~~~ after <path>` file exists on one side
//! only, so a path on both sides is an edit, on one side an addition or a deletion, and a
//! plain `#~~~~ <path>` file is the same on both sides. `R/` files run in the order
//! `DESCRIPTION`'s collation field and R's file order give them, and a `data/` file is a dataset
//! whose fingerprint is its text. A case whose files are all plain renders the plan of a fresh
//! load instead. A reload also lists the definitions that reach a touched name through their
//! calls, which `ry test --watch` reruns the tests of first.
//! `RY_BLESS=1` accepts new output; `FIXTURE_FILTER=group__case` runs one case.

use {
    loader::{Datasets, Loader, PackageInputs, Plan, SysdataChange, collate_code_files},
    semantics::{load_index::fnv1a, metadata::parse_description_collate},
    std::path::{Path, PathBuf},
    syntax::testing::{run_fixture_suite, split_files},
};

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
        let mut code = Vec::new();
        let mut datasets = Vec::new();
        for (path, text) in &files {
            let path = match path.split_once(' ') {
                Some((side, path)) if side == wanted => path,
                Some(_) => continue,
                None => path.as_str(),
            };
            match path {
                "DESCRIPTION" => inputs.description = text.clone(),
                "NAMESPACE" => inputs.namespace = text.clone(),
                _ if path.starts_with("data/") => datasets.push((path.to_owned(), text.clone())),
                _ => code.push((path.to_owned(), text.clone())),
            }
        }
        let collate = parse_description_collate(&inputs.description);
        let relative = code
            .iter()
            .map(|(path, _)| path.strip_prefix("R/").unwrap_or(path).to_owned())
            .collect();
        for path in collate_code_files(relative, &collate) {
            let path = format!("R/{path}");
            let text = code.iter().find(|(candidate, _)| *candidate == path);
            inputs
                .files
                .push((path, text.map(|(_, text)| text.clone())));
        }
        datasets.sort();
        if !datasets.is_empty() {
            inputs.data = Some(Datasets {
                fingerprint: fnv1a(format!("{datasets:?}").as_bytes()),
                files: datasets.into_iter().map(|(path, _)| path).collect(),
            });
        }
        inputs
    };
    let root = Path::new("/fixture");
    let mut loader = Loader::default();
    if files.iter().all(|(path, _)| !path.contains(' ')) {
        let inputs = side("after");
        let texts = inputs.files.clone();
        let plan = loader
            .plan_inputs(root, inputs, true)
            .expect("the fresh load plans");
        return render_plan(&plan, &texts);
    }
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
    let mut output = render_plan(&plan, &after_texts);
    if let Some(touched) = plan.touched_names() {
        loader.commit(root);
        let callers: Vec<String> = loader
            .callers(root, &touched)
            .into_iter()
            .filter(|name| !touched.contains(name))
            .collect();
        if !callers.is_empty() {
            output.push_str(&format!("affects callers {}\n", callers.join(", ")));
        }
    }
    output
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
    for (position, code) in plan.run.iter().enumerate() {
        let held = plan
            .deferred_s4
            .as_ref()
            .is_some_and(|deferred| deferred.run.contains(&position));
        let verb = if held { "hold back" } else { "run" };
        let text = text_of(&code.path);
        let snippet: String = text
            .chars()
            .skip(code.start as usize)
            .take((code.end - code.start) as usize)
            .collect();
        let first_line = snippet.lines().next().unwrap_or_default();
        output.push_str(&format!(
            "{verb} {}:{} {first_line}\n",
            code.path, code.line
        ));
    }
    if let Some(deferred) = &plan.deferred_s4 {
        output.push_str(&format!("placeholders {}\n", deferred.names.join(" ")));
        if !deferred.foreign_generics.is_empty() {
            output.push_str(&format!(
                "forced by other packages' generics {}\n",
                deferred.foreign_generics.join(" ")
            ));
        }
        if deferred.cache_key.is_none() {
            output.push_str("metadata not cached: a definition reads other code\n");
        }
    }
    if plan.sysdata != SysdataChange::Keep {
        output.push_str(&format!("sysdata {:?}\n", plan.sysdata));
    }
    match plan.data.as_deref() {
        Some([]) => output.push_str("clear datasets\n"),
        Some(files) => output.push_str(&format!("load datasets {}\n", files.join(" "))),
        None => {}
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
    if plan.keeps_names && !plan.is_noop() {
        output.push_str("keeps names\n");
    }
    if plan.run_on_load {
        output.push_str("run .onLoad\n");
    }
    for warning in &plan.warnings {
        output.push_str(&format!("warning {warning}\n"));
    }
    output
}
