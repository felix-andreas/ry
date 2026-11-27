//! The preflight spells each type of a signature canonically
//! (`semantics::canonical_annotation`), and the typed core reads that text to compile a function
//! for its declared types ([`typed::ahead::annotated_type`]). These tests pin the printer and the
//! reader against each other over the types a parameter can declare.

use {
    interp::testing::with_interpreter,
    semantics::{
        DocumentKind, ProjectFiles, RootDatabase, SourceFile, TypeDefinitions,
        canonical_annotation, stubs::install_shipped_stubs,
    },
    syntax::{SyntaxKind, annotation::signature},
    typed::{
        ahead::annotated_type,
        ir::{Item, Type},
        shapes,
    },
};

const DEFINITIONS: &str = "\
alias Region = :North | :South
alias Point = Record{x: F64, y: F64}
alias Pair = Tuple{I64, Str}
alias Counts = Dict<Str, I64>
alias Ids = List<I64>
alias Prices = double[]
alias Costs = Prices
alias Boxed<T> = Record{value: T}
alias Sales = DataFrame{price: double[]}
alias Step = fn(I64) -> I64
alias Rule = Expr
type Tree = :Leaf(I64) | :Node(Tree, Tree)
type Meters = F64
";

/// What the reader gives for a canonical spelling: a type, or the field items of a record or
/// tuple shape in slot order, written out (`Record{Dbl, Str}`, `List<Record{I64}>`).
#[derive(Debug, PartialEq)]
enum Read {
    Exact(Option<Type>),
    Boxed(String),
}

fn read(ty: Option<Type>) -> Read {
    match ty {
        Some(Type::Record(shape)) => Read::Boxed(written_item(Item::Record(shape))),
        Some(Type::Vector {
            item: item @ Item::Record(_),
            array: true,
        }) => Read::Boxed(format!("List<{}>", written_item(item))),
        other => Read::Exact(other),
    }
}

fn written_item(item: Item) -> String {
    match item {
        Item::Record(shape) => {
            let items: Vec<String> = shapes::fields(shape)
                .1
                .into_iter()
                .map(written_item)
                .collect();
            format!("Record{{{}}}", items.join(", "))
        }
        Item::Array(array) => format!("List<{}>", written_item(shapes::array_item(array))),
        other => format!("{other:?}"),
    }
}

/// Each written parameter type, its canonical spelling (`None` when it stays as written), and
/// the values the reader expects from R for that spelling (`Exact(None)` when none arrive from
/// R).
#[test]
fn canonical_spellings_read_back() {
    let vector = |item, array| Read::Exact(Some(Type::Vector { item, array }));
    let exact = |ty| Read::Exact(Some(ty));
    let boxed = |written: &str| Read::Boxed(written.to_string());
    let none = || Read::Exact(None);
    let tag = || exact(Type::Tag(shapes::ANY));
    let cases: Vec<(&str, Option<&str>, Read)> = vec![
        ("I64", Some("I64"), exact(Type::I64)),
        ("F64", Some("F64"), exact(Type::Dbl)),
        ("Bool", Some("Bool"), exact(Type::Lgl)),
        ("Str", Some("Str"), exact(Type::Str)),
        ("integer", Some("integer"), exact(Type::Int)),
        ("double", Some("double"), exact(Type::Dbl)),
        ("logical", Some("logical"), exact(Type::Lgl)),
        ("character", Some("character"), exact(Type::Str)),
        ("double[]", Some("double[]"), vector(Item::Dbl, false)),
        ("integer[]", Some("integer[]"), vector(Item::Int, false)),
        ("character[]", Some("character[]"), vector(Item::Str, false)),
        ("logical[]", Some("logical[]"), vector(Item::Lgl, false)),
        ("List<I64>", Some("List<I64>"), vector(Item::I64, true)),
        ("List<F64>", Some("List<F64>"), vector(Item::Dbl, true)),
        ("List<Str>", Some("List<Str>"), vector(Item::Str, true)),
        ("List<Bool>", Some("List<Bool>"), vector(Item::Lgl, true)),
        ("Ids", Some("List<I64>"), vector(Item::I64, true)),
        ("Prices", Some("double[]"), vector(Item::Dbl, false)),
        ("Costs", Some("double[]"), vector(Item::Dbl, false)),
        ("Region", Some(":North | :South"), tag()),
        (":A | :B(I64)", Some(":A | :B(I64)"), tag()),
        ("Tree", Some(":Leaf(I64) | :Node(Tree, Tree)"), tag()),
        ("Meters", Some("F64"), exact(Type::Dbl)),
        ("List<Meters>", Some("List<F64>"), vector(Item::Dbl, true)),
        ("List< I64 >", Some("List<I64>"), vector(Item::I64, true)),
        (
            "List<Tree>",
            Some("List<:Leaf(I64) | :Node(Tree, Tree)>"),
            none(),
        ),
        (
            "Sales",
            Some("DataFrame{price: double[]}"),
            exact(Type::Table),
        ),
        ("DataFrame", Some("DataFrame"), exact(Type::Table)),
        ("Rule", Some("Expr"), exact(Type::Table)),
        (
            "Point",
            Some("Record{x: F64, y: F64}"),
            boxed("Record{Dbl, Dbl}"),
        ),
        (
            "Boxed<I64>",
            Some("Record{value: I64}"),
            boxed("Record{I64}"),
        ),
        (
            "Record{id: I64, region: Region}",
            Some("Record{id: I64, region: :North | :South}"),
            none(),
        ),
        (
            "List<Point>",
            Some("List<Record{x: F64, y: F64}>"),
            boxed("List<Record{Dbl, Dbl}>"),
        ),
        ("Pair", Some("Tuple{I64, Str}"), boxed("Record{I64, Str}")),
        (
            "Record{name: Str, at: Point, ids: List<I64>}",
            Some("Record{at: Record{x: F64, y: F64}, ids: List<I64>, name: Str}"),
            boxed("Record{Record{Dbl, Dbl}, List<I64>, Str}"),
        ),
        ("Counts", Some("Dict<Str, I64>"), none()),
        ("Dict<Str, List<F64>>", Some("Dict<Str, List<F64>>"), none()),
        ("Set<I64>", Some("Set<I64>"), none()),
        ("Step", Some("fn(I64) -> I64"), none()),
        ("fn(F64) -> F64", Some("fn(F64) -> F64"), none()),
        // An open record has no closed spelling, so it stays as written.
        ("Record{a: I64, ..}", None, none()),
    ];
    let mut source = format!("# typing: strict\n{DEFINITIONS}");
    for (index, (written, _, _)) in cases.iter().enumerate() {
        source.push_str(&format!("f{index} <- function(x: {written}) NULL\n"));
    }
    let db = RootDatabase::default();
    install_shipped_stubs(&db);
    let file = SourceFile::new(&db, source.clone(), DocumentKind::Package);
    ProjectFiles::new(&db, vec![file]);
    let definitions = TypeDefinitions::of(&db, file);
    let root = syntax::parse(&source).syntax_node();
    let formals: Vec<_> = root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::FUNCTION_DEF)
        .filter_map(|function| {
            signature(&function, true)
                .formals
                .into_iter()
                .next()
                .flatten()
        })
        .collect();
    assert_eq!(formals.len(), cases.len());
    for (node, (written, canonical, expected)) in formals.iter().zip(&cases) {
        assert_eq!(node.text().to_string(), *written);
        let spelled = canonical_annotation(&db, definitions, node);
        assert_eq!(
            spelled.as_deref(),
            *canonical,
            "the canonical spelling of `{written}`"
        );
        let text = spelled.unwrap_or_else(|| written.to_string());
        // SAFETY: the reader interns symbols and layouts on the interpreter, held here.
        let got = with_interpreter(|| read(unsafe { annotated_type(&text) }));
        assert_eq!(got, *expected, "the reading of `{text}`");
    }
}

/// The stop rule of canonical spelling, over the typed benchmark programs: every type their
/// signatures write has a canonical spelling, so none is left for the runtime to read as
/// written.
#[test]
fn benchmark_signatures_all_have_canonical_spellings() {
    let bench = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench");
    let mut programs = Vec::new();
    for directory in [
        "kernels",
        "applications",
        "public/rcp",
        "public/awfy",
        "public/shootout",
    ] {
        let entries = std::fs::read_dir(bench.join(directory)).expect("read a benchmark directory");
        for entry in entries {
            let path = entry.expect("read a benchmark entry").path();
            if path.to_string_lossy().ends_with(".typed.R") {
                programs.push(path);
            }
        }
    }
    programs.sort();
    let (mut written, mut rewritten, mut left) = (0, 0, Vec::new());
    for path in &programs {
        let source = std::fs::read_to_string(path).expect("read a benchmark program");
        let db = RootDatabase::default();
        install_shipped_stubs(&db);
        let file = SourceFile::new(&db, source.clone(), DocumentKind::Package);
        ProjectFiles::new(&db, vec![file]);
        let definitions = TypeDefinitions::of(&db, file);
        let root = syntax::parse(&source).syntax_node();
        for function in root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::FUNCTION_DEF)
        {
            let written_types = signature(&function, true);
            for node in written_types
                .formals
                .into_iter()
                .flatten()
                .chain(written_types.ret)
            {
                if node.text().to_string().contains('\n') {
                    continue;
                }
                written += 1;
                match canonical_annotation(&db, definitions, &node) {
                    Some(text) if text != node.text() => rewritten += 1,
                    Some(_) => {}
                    None => left.push(format!("{}: {}", path.display(), node.text())),
                }
            }
        }
    }
    eprintln!(
        "{} programs: {written} written types, {rewritten} rewritten, {} left as written",
        programs.len(),
        left.len()
    );
    assert!(programs.len() >= 60 && written > 0);
    assert!(
        left.is_empty(),
        "types with no canonical spelling:\n{}",
        left.join("\n")
    );
}
