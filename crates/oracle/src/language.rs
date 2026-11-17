//! The language fuzz program: closures built from the constructs whose behavior depends on the
//! call stack, on evaluation order and on dispatch, and called several times.
//!
//! A program holds a few episodes. Each defines one closure `f_<n>` from statements drawn from
//! the [`Construct`]s its seed enables, and calls it one, a few or sixty times through `run_`,
//! which records the visible value, every `log_` entry and every warning of the first and the
//! last call. A statement is a template whose holes are filled independently per occurrence,
//! and a statement may nest another inside a loop, a handler, `local()`, `eval()` or a closure,
//! so constructs meet in combinations nobody listed.
//!
//! Every closure is byte-compiled up front (`cmp_`), because GNU R runs a closure's first calls
//! from the syntax tree and the later ones from bytecode, and the two differ in a few places
//! (the call an error names, the call of a `<<-` warning) that are no engine's bug.

use {
    crate::metamorphic::{Features, Zoo},
    syntax::testing::SplitMix64,
};

/// One family of statements. A seed enables each with probability one half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Construct {
    OnExit,
    Frames,
    Dots,
    Promises,
    Replacement,
    SuperAssignment,
    Conditions,
    S3,
    S4,
    ReferenceClasses,
    Eval,
    Environments,
    Regex,
    ControlFlow,
}

const CONSTRUCTS: [Construct; 14] = [
    Construct::OnExit,
    Construct::Frames,
    Construct::Dots,
    Construct::Promises,
    Construct::Replacement,
    Construct::SuperAssignment,
    Construct::Conditions,
    Construct::S3,
    Construct::S4,
    Construct::ReferenceClasses,
    Construct::Eval,
    Construct::Environments,
    Construct::Regex,
    Construct::ControlFlow,
];

/// The language program for `seed`. The same seed always gives the same text.
pub fn program(seed: u64) -> String {
    let mut rng = SplitMix64(seed ^ 0x6c61_6e67_7561_6765);
    // One seed in eight uses every construct, so that the whole space is also sampled densely.
    let enabled: Vec<Construct> = if rng.chance(1, 8) {
        CONSTRUCTS.to_vec()
    } else {
        let bits = rng.next_u64();
        let mut enabled: Vec<Construct> = CONSTRUCTS
            .into_iter()
            .filter(|construct| bits & (1 << *construct as u8) != 0)
            .collect();
        if enabled.is_empty() {
            enabled.push(CONSTRUCTS[rng.below(CONSTRUCTS.len())]);
        }
        enabled
    };
    let features = Features::swarm(&mut rng);
    let mut writer = Writer {
        zoo: Zoo {
            rng,
            features,
            longest: LONGEST,
        },
        enabled,
        labels: 0,
    };
    let names: Vec<String> = writer
        .enabled
        .iter()
        .map(|construct| format!("{construct:?}"))
        .collect();
    let mut text = format!(
        "# oracle language program, seed {seed}\n# constructs: {}\n# features: {}\n{PREAMBLE}",
        names.join(" "),
        features.names()
    );
    for number in 1..=2 + writer.zoo.rng.below(3) {
        text.push_str(&episode(&mut writer, number));
    }
    text
}

/// No value is longer than this, which is past the 64-element limit where a copy becomes a
/// wrapper and short enough that a log line of it stays readable.
const LONGEST: usize = 70;

/// `log_` records a value as its deparsed text, so a log compares what the value is and not
/// how it prints. It forces the value in its own frame first, so the frames a statement reports
/// do not depend on the functions `log_` calls. `run_` calls `thunk` `times` times and returns
/// the first and the last call's outcome: the value with its visibility, or the error's message
/// and class, and the log, which also holds each warning. `t_` turns an error into a value so
/// that one failing statement does not end the closure. `class_` is a condition's class with
/// one name for `evalError` and `getvarError`: GNU R's AST evaluator raises the first for a
/// missing argument and its compiled code the second, and which of the two runs a promise's
/// code is not ry's to match.
const PREAMBLE: &str = r#"trace_ <- new.env()
log_ <- function(tag, value) {
  value
  trace_$log <- c(trace_$log, paste0(tag, ": ", paste(deparse(value), collapse = " ")))
  invisible(value)
}
cmp_ <- function(f) compiler::cmpfun(f)
class_ <- function(condition) {
  classes <- class(condition)
  classes[classes %in% c("evalError", "getvarError")] <- "evalError or getvarError"
  classes
}
message_ <- function(condition) {
  sub("(TRE pattern compilation error|, reason) '.*'$", "\\1", conditionMessage(condition))
}
t_ <- cmp_(function(expr) tryCatch(expr, error = function(e) paste("error:", message_(e))))
run_ <- cmp_(function(times, thunk) {
  outcomes <- vector("list", times)
  for (i in seq_len(times)) {
    trace_$log <- character()
    value <- withCallingHandlers(
      tryCatch(withVisible(thunk()), error = function(e) list(error = message_(e), class = class_(e))),
      warning = function(w) {
        log_("warning", message_(w))
        invokeRestart("muffleWarning")
      }
    )
    outcomes[[i]] <- list(value = value, log = trace_$log)
  }
  list(first = outcomes[[1]], last = outcomes[[times]], same = identical(outcomes[[1]], outcomes[[times]]))
})
"#;

struct Writer {
    zoo: Zoo,
    enabled: Vec<Construct>,
    /// The number of the last label, which makes each log tag of a program distinct.
    labels: usize,
}

/// One episode: the definitions its constructs need, the closure, and its calls.
fn episode(writer: &mut Writer, number: usize) -> String {
    let count = 1 + writer.zoo.rng.below(3.min(writer.enabled.len()));
    let mut constructs: Vec<Construct> = Vec::new();
    while constructs.len() < count {
        let construct = writer.enabled[writer.zoo.rng.below(writer.enabled.len())];
        if !constructs.contains(&construct) {
            constructs.push(construct);
        }
    }
    let names: Vec<String> = constructs
        .iter()
        .map(|construct| format!("{construct:?}"))
        .collect();
    let mut text = format!("# episode {number}: {}\n", names.join(" "));
    for construct in &constructs {
        text.push_str(&fill(writer, definitions(*construct), number, 0));
    }
    let default = match writer.zoo.rng.below(4) {
        0 => String::new(),
        1 => format!(" = {}", element(&mut writer.zoo)),
        2 => " = { log_(\"default b\", NULL); length(a) }".to_owned(),
        _ => " = a".to_owned(),
    };
    text.push_str(&format!(
        "f_{number} <- cmp_(function(a, b{default}, ...) {{\n  x <- a\n"
    ));
    for construct in &constructs {
        text.push_str(&fill(writer, setup(*construct), number, 0));
    }
    for _ in 0..4 + writer.zoo.rng.below(7) {
        let statement = statement(writer, &constructs, number, 0);
        text.push_str(&format!("  {statement}\n"));
    }
    text.push_str("  list(x, nargs())\n})\n");
    let times = [1, 2, 2, 3, 3, 5, 60][writer.zoo.rng.below(7)];
    let call = call(writer, number);
    text.push_str(&format!(
        "r_{number} <- run_({times}L, cmp_(function() {call}))\nr_{number}\n"
    ));
    text
}

/// One element of a type the seed enables.
fn element(zoo: &mut Zoo) -> &'static str {
    let kind = zoo.kind();
    zoo.element(kind)
}

/// A call of the episode's closure: its arguments, and the form the call is made in, which
/// decides what `sys.call()`, `match.call()` and `parent.frame()` see.
fn call(writer: &mut Writer, number: usize) -> String {
    let mut arguments: Vec<String> = Vec::new();
    if !writer.zoo.rng.chance(1, 12) {
        arguments.push(writer.zoo.value().text);
    }
    match writer.zoo.rng.below(4) {
        0 => arguments.push(format!(
            "b = log_(\"argument b\", {})",
            element(&mut writer.zoo)
        )),
        1 => arguments.push(element(&mut writer.zoo).to_owned()),
        _ => {}
    }
    for _ in 0..writer.zoo.rng.below(4) {
        let value = match writer.zoo.rng.below(3) {
            0 => writer.zoo.value().text,
            _ => element(&mut writer.zoo).to_owned(),
        };
        let name = writer.zoo.name();
        arguments.push(if writer.zoo.rng.chance(1, 2) && name != "\"\"" {
            format!("{name} = {value}")
        } else {
            value
        });
    }
    let arguments = arguments.join(", ");
    match writer.zoo.rng.below(9) {
        0 => format!("do.call(f_{number}, list({arguments}))"),
        1 => format!("do.call(\"f_{number}\", list({arguments}))"),
        2 => format!("local(f_{number}({arguments}))"),
        3 => format!("eval(quote(f_{number}({arguments})))"),
        4 => format!("(function(...) f_{number}(...))({arguments})"),
        5 => format!("tryCatch(f_{number}({arguments}), finally = log_(\"left\", NULL))"),
        _ => format!("f_{number}({arguments})"),
    }
}

/// One statement of a closure body: a template of one of the episode's constructs, sometimes
/// inside a form that changes the frame, the handler stack or the scope it runs in.
fn statement(writer: &mut Writer, constructs: &[Construct], number: usize, depth: usize) -> String {
    let construct = constructs[writer.zoo.rng.below(constructs.len())];
    let pool = statements(construct);
    let template = pool[writer.zoo.rng.below(pool.len())];
    let mut text = fill(writer, template, number, depth);
    if construct == Construct::Regex {
        text = regex_statement(writer);
    }
    if depth >= 2 || !writer.zoo.rng.chance(1, 4) {
        return text;
    }
    let wrappers = [
        "tryCatch({ #BODY# }, error = function(e) log_(\"caught#L#\", conditionMessage(e)), finally = log_(\"finally#L#\", NULL))",
        "for (round in 1:2) { #BODY# }",
        "local({ #BODY# })",
        "eval(quote({ #BODY# }))",
        "(function() { #BODY# })()",
        "if (length(x) > #I#L) { #BODY# } else { #X# }",
        "withCallingHandlers({ #BODY# }, warning = function(w) { log_(\"handled#L#\", conditionMessage(w)); invokeRestart(\"muffleWarning\") })",
        "invisible(lapply(1:2, function(round) { #BODY# }))",
        "suppressWarnings({ #BODY# })",
        "repeat { #BODY#; break }",
        "switch(#I#L, { #BODY# }, { #X# }, NULL)",
        "{ #BODY#; #X# }",
    ];
    let wrapper = wrappers[writer.zoo.rng.below(wrappers.len())];
    fill(writer, wrapper, number, depth + 1).replace("#BODY#", &text)
}

/// `template` with each hole filled: `#E#` the episode number, `#L#` a label that is the same
/// throughout one template, `#I#` a small count, `#V#` a zoo value, `#e#` one element, `#S#` a
/// string, `#N#` a name that is not empty, `#Q#` an expression to quote, `#q#` one without a
/// loop, and `#X#` another statement.
fn fill(writer: &mut Writer, template: &str, number: usize, depth: usize) -> String {
    writer.labels += 1;
    let label = writer.labels;
    let mut text = String::with_capacity(template.len() * 2);
    let mut rest = template;
    while let Some(start) = rest.find('#') {
        text.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('#') else {
            text.push_str(&rest[start..]);
            return text;
        };
        match &after[..end] {
            "E" => text.push_str(&number.to_string()),
            "L" => text.push_str(&format!(" {label}")),
            "I" => text.push_str(&(1 + writer.zoo.rng.below(4)).to_string()),
            "V" => text.push_str(&writer.zoo.value().text),
            "e" => text.push_str(element(&mut writer.zoo)),
            "S" => text.push_str(writer.zoo.string()),
            "N" => {
                let name = writer.zoo.name();
                text.push_str(if name == "\"\"" { "\"n\"" } else { name });
            }
            "Q" => text.push_str(&quoted(writer, 3, true)),
            "q" => text.push_str(&quoted(writer, 3, false)),
            "X" if depth < 2 => {
                let enabled = writer.enabled.clone();
                text.push_str(&statement(writer, &enabled, number, depth + 1));
            }
            "X" => text.push_str("NULL"),
            // The wrapped statement, which the caller puts in.
            "BODY" => text.push_str("#BODY#"),
            other => {
                text.push('#');
                text.push_str(other);
                text.push('#');
            }
        }
        rest = &after[end + 1..];
    }
    text.push_str(rest);
    text
}

/// What an episode defines at top level for `construct`, before its closure.
fn definitions(construct: Construct) -> &'static str {
    match construct {
        Construct::Dots => {
            r#"inner_#E# <- cmp_(function(first, ..., last = "default") list(nargs(), missing(first), ...length(), names(list(...)), last))
"#
        }
        Construct::Replacement => {
            r#"`second_#E#<-` <- cmp_(function(x, value) { log_("second<-", value); x[2] <- value; x })
`field_#E#<-` <- cmp_(function(x, name, value) { x[[name]] <- value; x })
get_#E# <- cmp_(function(x) x)
`get_#E#<-` <- cmp_(function(x, value) value)
"#
        }
        Construct::SuperAssignment => {
            r#"counter_#E# <- 0L
bump_#E# <- cmp_(function(by = 1L) { counter_#E# <<- counter_#E# + by; counter_#E# })
"#
        }
        Construct::S3 => {
            r#"describe_#E# <- cmp_(function(x, ...) UseMethod("describe_#E#"))
describe_#E#.default <- cmp_(function(x, ...) list("default", class(x), .Generic))
describe_#E#.integer <- cmp_(function(x, ...) list("integer", NextMethod()))
describe_#E#.numeric <- cmp_(function(x, ...) list("numeric", NextMethod()))
describe_#E#.character <- cmp_(function(x, extra = "none", ...) list("character", extra, nargs()))
describe_#E#.matrix <- cmp_(function(x, ...) list("matrix", dim(x), NextMethod()))
describe_#E#.function <- cmp_(function(x, ...) "function")
describe_#E#.data.frame <- cmp_(function(x, ...) list("data.frame", NextMethod()))
describe_#E#.factor <- cmp_(function(x, ...) list("factor", .Class, NextMethod()))
describe_#E#.box_#E# <- cmp_(function(x, ...) list("box", .Generic, .Class, NextMethod()))
format.box_#E# <- cmp_(function(x, ...) paste0("<box ", length(unclass(x)), ">"))
print.box_#E# <- cmp_(function(x, ...) { cat(format(x), "\n"); invisible(x) })
`[.box_#E#` <- cmp_(function(x, i) structure(unclass(x)[i], class = "box_#E#"))
`[<-.box_#E#` <- cmp_(function(x, i, value) { log_("box assign", i); y <- unclass(x); y[i] <- value; structure(y, class = "box_#E#") })
`[[.box_#E#` <- cmp_(function(x, i) list("element", i))
`$.box_#E#` <- cmp_(function(x, name) paste("field", name))
length.box_#E# <- cmp_(function(x) 99L)
Ops.box_#E# <- cmp_(function(e1, e2) {
  log_("Ops", list(.Generic, nargs(), missing(e2)))
  if (missing(e2)) get(.Generic)(unlist(unclass(e1))) else get(.Generic)(unlist(unclass(e1)), unlist(unclass(e2)))
})
Math.box_#E# <- cmp_(function(x, ...) { log_("Math", .Generic); get(.Generic)(unlist(unclass(x)), ...) })
Summary.box_#E# <- cmp_(function(..., na.rm = FALSE) { log_("Summary", list(.Generic, na.rm)); 0 })
as.character.box_#E# <- cmp_(function(x, ...) "box")
c.box_#E# <- cmp_(function(...) "combined")
rev.box_#E# <- cmp_(function(x) "reversed")
"#
        }
        Construct::S4 => {
            r#"setClass("Base_#E#", representation(id = "numeric", label = "character"), prototype(label = "none"))
setClass("Derived_#E#", contains = "Base_#E#", representation(extra = "list"),
  validity = function(object) if (length(object@id) > 2L) "id is too long" else TRUE)
setClassUnion("Either_#E#", c("numeric", "character"))
setClass("Holder_#E#", representation(value = "Either_#E#"))
setGeneric("area_#E#", function(shape, ...) standardGeneric("area_#E#"))
setMethod("area_#E#", "Base_#E#", cmp_(function(shape, ...) { log_("area base", shape@id); sum(shape@id) }))
setMethod("area_#E#", "Derived_#E#", cmp_(function(shape, ...) { log_("area derived", NULL); 10 * callNextMethod() }))
setMethod("area_#E#", "numeric", cmp_(function(shape, ...) "numeric"))
setMethod("area_#E#", "character", cmp_(function(shape, ...) "character"))
setMethod("show", "Base_#E#", cmp_(function(object) cat("<Base", object@id, ">\n")))
setMethod("length", "Base_#E#", cmp_(function(x) 42L))
setMethod("+", signature("Base_#E#", "Base_#E#"), cmp_(function(e1, e2) new("Base_#E#", id = e1@id + e2@id)))
setMethod("Arith", signature("Base_#E#", "numeric"), cmp_(function(e1, e2) { log_("Arith", .Generic); new("Base_#E#", id = callGeneric(e1@id, e2)) }))
setMethod("[", "Base_#E#", cmp_(function(x, i, ...) x@id[i]))
setMethod("initialize", "Derived_#E#", function(.Object, ...) {
  .Object <- callNextMethod(.Object, ...)
  if (length(.Object@extra) == 0L) .Object@extra <- list("initialized")
  .Object
})
setGeneric("id_#E#<-", function(object, value) standardGeneric("id_#E#<-"))
setReplaceMethod("id_#E#", "Base_#E#", cmp_(function(object, value) { object@id <- value; validObject(object); object }))
"#
        }
        Construct::ReferenceClasses => {
            r#"Account_#E# <- setRefClass("Account_#E#", fields = list(balance = "numeric", history = "list"), methods = list(
  deposit = function(amount) { balance <<- balance + amount; history[[length(history) + 1L]] <<- amount; invisible(.self) },
  total = function() balance,
  show = function() cat("<Account", balance, ">\n"),
  initialize = function(...) { initFields(...); if (length(balance) == 0L) balance <<- 0; invisible(.self) }
))
Savings_#E# <- setRefClass("Savings_#E#", contains = "Account_#E#", fields = list(rate = "numeric"), methods = list(
  deposit = function(amount) { log_("savings deposit", amount); callSuper(amount * 2) },
  total = function() callSuper() + 1000
))
"#
        }
        _ => "",
    }
}

/// What a closure binds for `construct` before its statements run.
fn setup(construct: Construct) -> &'static str {
    match construct {
        Construct::S3 => "  box <- structure(list(1L, \"two\", 3), class = \"box_#E#\")\n",
        Construct::S4 => "  obj <- new(\"Derived_#E#\", id = c(1, 2))\n",
        Construct::ReferenceClasses => "  account <- Savings_#E#$new(balance = 10, rate = 0.5)\n",
        _ => "",
    }
}

fn statements(construct: Construct) -> &'static [&'static str] {
    match construct {
        Construct::OnExit => ON_EXIT,
        Construct::Frames => FRAMES,
        Construct::Dots => DOTS,
        Construct::Promises => PROMISES,
        Construct::Replacement => REPLACEMENT,
        Construct::SuperAssignment => SUPER_ASSIGNMENT,
        Construct::Conditions => CONDITIONS,
        Construct::S3 => S3,
        Construct::S4 => S4,
        Construct::ReferenceClasses => REFERENCE_CLASSES,
        Construct::Eval => EVAL,
        Construct::Environments => ENVIRONMENTS,
        // A regex statement is composed, not drawn: see `regex_statement`.
        Construct::Regex => &["NULL"],
        Construct::ControlFlow => CONTROL_FLOW,
    }
}

const ON_EXIT: &[&str] = &[
    r#"on.exit(log_("exit#L#", x))"#,
    r#"on.exit(log_("exit added#L#", 1L), add = TRUE)"#,
    r#"on.exit(log_("exit first#L#", 2L), add = TRUE, after = FALSE)"#,
    r#"on.exit()"#,
    r#"log_("sys.on.exit#L#", sys.on.exit())"#,
    r#"(function() { on.exit(log_("inner exit#L#", 0L)); #X#; invisible(NULL) })()"#,
    r#"on.exit(x <- NULL, add = TRUE)"#,
    r#"if (length(x) > #I#L + 3L) { on.exit(log_("early#L#", NULL), add = TRUE); return(log_("returned#L#", x)) }"#,
    r#"on.exit(return(log_("exit return#L#", "replaced")), add = TRUE)"#,
    r#"log_("exit runs on error#L#", tryCatch((function() { on.exit(log_("cleanup#L#", NULL)); stop("boom") })(), error = conditionMessage))"#,
    r#"log_("exit order#L#", (function() { on.exit(log_("one#L#", 1L)); on.exit(log_("two#L#", 2L), add = TRUE); on.exit(log_("zero#L#", 0L), add = TRUE, after = FALSE); "body" })())"#,
    r#"log_("exit sees locals#L#", (function() { v <- 1L; on.exit(log_("local at exit#L#", v)); v <- 2L; v })())"#,
    r#"log_("exit value#L#", (function() { on.exit(return("from exit")); "from body" })())"#,
    r#"log_("exit invisible#L#", withVisible((function() { on.exit(NULL); invisible(1L) })())$visible)"#,
    r#"log_("exit in loop#L#", (function() { for (i in 1:3) on.exit(log_("loop exit#L#", i), add = TRUE); "looped" })())"#,
    r#"on.exit({ #X# }, add = TRUE)"#,
];

const FRAMES: &[&str] = &[
    r#"log_("call#L#", sys.call())"#,
    r#"log_("match#L#", match.call())"#,
    r#"log_("match unexpanded#L#", match.call(expand.dots = FALSE))"#,
    r#"log_("nargs#L#", nargs())"#,
    r#"log_("missing#L#", c(missing(a), missing(b)))"#,
    r#"log_("nframe#L#", sys.nframe())"#,
    r#"log_("formals#L#", names(formals(sys.function())))"#,
    r#"log_("parent is global#L#", identical(parent.frame(), globalenv()))"#,
    r#"log_("callers#L#", vapply(sys.calls(), function(call) class(call[[1]])[1], ""))"#,
    r#"log_("parents#L#", sys.parents())"#,
    r#"log_("recall#L#", (function(n) if (n <= 0L) 0L else n + Recall(n - 1L))(#I#L))"#,
    r#"log_("variables#L#", sort(ls()))"#,
    r#"log_("caller#L#", sys.call(-1))"#,
    r#"log_("call zero#L#", sys.call(0))"#,
    r#"log_("match.arg#L#", t_((function(kind = c("alpha", "beta", "gamma")) match.arg(kind))()))"#,
    r#"log_("match.arg partial#L#", t_((function(kind = c("alpha", "beta", "gamma")) match.arg(kind))("be")))"#,
    r#"log_("match.arg wrong#L#", t_((function(kind = c("alpha", "beta", "gamma")) match.arg(kind))(#S#)))"#,
    r#"log_("match.arg several#L#", t_((function(kind = c("alpha", "beta", "gamma")) match.arg(kind, several.ok = TRUE))(c("g", "al"))))"#,
    r#"log_("frames#L#", length(sys.frames()) == sys.nframe())"#,
    r#"log_("function frame#L#", identical(sys.frame(sys.nframe()), environment()))"#,
    r#"log_("inner call#L#", (function(p, ...) sys.call())(#e#, #N# = #e#))"#,
    r#"log_("inner match#L#", (function(first, second = 2, ...) match.call())(se = #e#, #e#, #e#))"#,
    r#"log_("inner parent#L#", (function() identical(parent.frame(), sys.frame(-1)))())"#,
    r#"log_("partial names#L#", t_((function(value, verbose = FALSE, ...) list(value, verbose, names(list(...))))(val = 1, verb = TRUE, v = 3)))"#,
    r#"log_("argument errors#L#", list(t_((function(p) p)(1, 2)), t_((function(p) p)(q = 1)), t_((function(p, p2) p)(p = 1, p = 2)), t_((function(pa, pb) 1)(p = 1))))"#,
    r#"log_("sys.function#L#", identical(sys.function(), f_#E#))"#,
    r#"log_("eval frame#L#", eval(quote(sys.call())))"#,
    r#"log_("eval frames#L#", eval(quote(sys.nframe())) - sys.nframe())"#,
    r#"log_("eval parent#L#", identical(eval(quote(parent.frame())), parent.frame()))"#,
    r#"log_("do.call frame#L#", do.call(function(...) sys.call(), list(#e#, k = #e#)))"#,
    r#"log_("lapply frame#L#", lapply(1:2, function(i) sys.call())[[2]])"#,
    r#"log_("stack depth#L#", (function() sys.nframe())() - sys.nframe())"#,
    r#"log_("function parent#L#", (function() sys.function(-1))() |> is.function())"#,
    r#"log_("on.exit frame#L#", (function() { on.exit(log_("exit call#L#", sys.call())); NULL })())"#,
];

const DOTS: &[&str] = &[
    r#"log_("dots length#L#", ...length())"#,
    r#"log_("dots names#L#", ...names())"#,
    r#"log_("dot one#L#", t_(..1))"#,
    r#"log_("dot element#L#", t_(...elt(#I#L)))"#,
    r#"log_("dots list#L#", t_(list(...)))"#,
    r#"log_("dots quoted#L#", substitute(list(...)))"#,
    r#"log_("dots forwarded#L#", t_(inner_#E#(...)))"#,
    r#"log_("dots forwarded named#L#", t_(inner_#E#(..., last = #e#)))"#,
    r#"log_("dots do.call#L#", t_(do.call(inner_#E#, list(...))))"#,
    r#"log_("dots combined#L#", t_(c(...)))"#,
    r#"log_("dots nested#L#", (function(...) ...length())(x, ...))"#,
    r#"log_("dots missing#L#", t_((function(p, ...) missing(p))(...)))"#,
    r#"log_("dots unevaluated#L#", names(match.call(expand.dots = FALSE)$...))"#,
    r#"log_("dots twice#L#", t_(identical(list(...), list(...))))"#,
    r#"log_("dots in closure#L#", t_((function() ...length())()))"#,
    r#"log_("dots lazily#L#", (function(...) "untouched")(stop("never forced")))"#,
    r#"log_("dots order#L#", (function(...) { list(...); "forced" })(log_("first dot#L#", 1L), log_("second dot#L#", 2L)))"#,
    r#"log_("dots names kept#L#", (function(...) names(list(...)))(#N# = 1, 2, #N# = 3))"#,
    r#"log_("dots to vapply#L#", t_(vapply(list(...), function(item) class(item)[1], "")))"#,
    r#"log_("dots empty#L#", (function(...) list(...length(), is.null(...names()), length(list(...))))())"#,
    r#"log_("dots missing element#L#", t_((function(...) nargs())(1, , 3)))"#,
    r#"log_("dots alist#L#", t_((function(...) length(as.list(substitute(list(...)))[-1L]))(p, q + 1, )))"#,
    r#"log_("dots sum#L#", t_(sum(...)))"#,
    r#"log_("dots paste#L#", t_(paste(..., sep = "|")))"#,
    r#"log_("dot two missing#L#", t_(..2))"#,
    r#"log_("dots rebound#L#", (function(...) { args <- list(...); args[[1]] <- "changed"; list(args[[1]], ..1) })(#e#))"#,
];

const PROMISES: &[&str] = &[
    r#"force(b)"#,
    r#"log_("b#L#", t_(b))"#,
    r#"delayedAssign("lazy", { log_("lazy forced#L#", NULL); #e# }); #X#; log_("lazy#L#", t_(lazy))"#,
    r#"log_("argument a#L#", substitute(a))"#,
    r#"log_("argument b#L#", substitute(b))"#,
    r#"log_("argument text#L#", deparse(substitute(a))[1])"#,
    r#"log_("promise order#L#", (function(p, q) { q; p; "done" })(log_("p#L#", 1L), log_("q#L#", 2L)))"#,
    r#"log_("unforced#L#", (function(p) 1L)(stop("never")))"#,
    r#"log_("default sees local#L#", (function(p = local_value) { local_value <- #I#L; p })())"#,
    r#"log_("default missing#L#", (function(p = #e#) missing(p))())"#,
    r#"log_("recursive default#L#", t_((function(p = p) p)()))"#,
    r#"log_("forced in lapply#L#", (function() { adders <- lapply(1:3, function(i) function(y) y + i); vapply(adders, function(g) g(10), 0) })())"#,
    r#"log_("captured by local#L#", (function() { fs <- list(); for (i in 1:3) { local({ j <- i; fs[[j]] <<- function() j }) }; vapply(fs, function(g) g(), 0L) })())"#,
    r#"log_("captured loop variable#L#", (function() { fs <- list(); for (i in 1:3) fs[[i]] <- function() i; vapply(fs, function(g) g(), 0L) })())"#,
    r#"log_("promise once#L#", (function(p) { p; p; p })({ log_("evaluated#L#", NULL); #e# }))"#,
    r#"log_("promise sees change#L#", (function() { v <- 1L; g <- function(p) { v <<- 2L; p }; g(v) })())"#,
    r#"log_("promise error twice#L#", (function(p) list(t_(p), t_(p)))(stop("bad argument")))"#,
    r#"log_("missing passed on#L#", (function(p) (function(q) missing(q))(p))())"#,
    r#"log_("missing with default#L#", (function(p = 1) (function(q) missing(q))(p))())"#,
    r#"log_("missing used#L#", t_((function(p) p + 1)()))"#,
    r#"log_("default uses argument#L#", (function(p, q = p * 2) { p <- 10; q })(1))"#,
    r#"log_("substitute in callee#L#", (function(p) substitute(p))(x[[1]] + #e#))"#,
    r#"log_("substitute forced#L#", (function(p) { force(p); substitute(p) })(1 + 2))"#,
    r#"log_("substitute local#L#", (function() { v <- quote(w); substitute(v + 1, list(v = v)) })())"#,
    r#"log_("delayed in loop#L#", (function() { for (i in 1:2) delayedAssign(paste0("d", i), i * 10L); c(d1, d2) })())"#,
    r#"log_("promise in handler#L#", tryCatch((function(p) p)(stop("from promise")), error = function(e) deparse(conditionCall(e))))"#,
    r#"log_("force order#L#", (function(p, q) { force(q); log_("between#L#", NULL); force(p) })(log_("forcing p#L#", 1L), log_("forcing q#L#", 2L)))"#,
    r#"log_("identity of promise value#L#", (function(p) { y <- p; y[1] <- y[1]; identical(p, x) })(x))"#,
];

const REPLACEMENT: &[&str] = &[
    r#"t_(second_#E#(x) <- #e#)"#,
    r#"t_(x[[#I#L]] <- #e#)"#,
    r#"t_(x[#I#L] <- #e#)"#,
    r#"t_(x[[length(x) + 1L]] <- #e#)"#,
    r#"t_(x[c(TRUE, FALSE)] <- #e#)"#,
    r#"t_(names(x)[#I#L] <- #S#)"#,
    r#"t_(attr(x, "tag")[[#N#]] <- #e#)"#,
    r#"t_(levels(x)[#I#L] <- #S#)"#,
    r#"t_(dim(x) <- NULL)"#,
    r#"t_(dimnames(x)[[1]][#I#L] <- #S#)"#,
    r#"t_(field_#E#(x, #N#) <- #e#)"#,
    r#"t_(x[[log_("index#L#", #I#L)]] <- log_("value#L#", #e#))"#,
    r#"t_(names(x)[[log_("name index#L#", 1L)]] <- log_("name value#L#", "n"))"#,
    r#"local({ g <- function(p) p + 1; body(g)[[1]] <- as.name("-"); formals(g)$p <- 5; log_("rebuilt#L#", list(g(), deparse(g))) })"#,
    r#"t_(is.na(x) <- #I#L)"#,
    r#"t_(length(x) <- #I#L)"#,
    r#"t_(class(x) <- "tagged")"#,
    r#"t_(oldClass(x) <- NULL)"#,
    r#"local({ y <- x; t_(y[[#I#L]] <- #e#); log_("alias#L#", identical(x, y)) })"#,
    r#"local({ l <- list(a = list(b = list(c = 1L))); l$a$b$c <- x; l[["a"]][["b"]]$d <- #e#; l$a$b[c("e", "f")] <- list(#e#, NULL); log_("nested#L#", l) })"#,
    r#"t_(get_#E#(x)[[#I#L]] <- #e#)"#,
    r#"t_((function() x[[#I#L]] <<- #e#)())"#,
    r#"local({ e <- new.env(); e$v <- x; t_(e$v[[#I#L]] <- #e#); t_(names(e$v)[1] <- #S#); log_("environment element#L#", list(e$v, identical(e$v, x))) })"#,
    r#"t_(x$new <- #e#)"#,
    r#"t_(x[["new"]][[2]] <- #e#)"#,
    r#"t_(x$new$deep <- #e#)"#,
    r#"t_(substr(x, 1, 2) <- #S#)"#,
    r#"t_(diag(x) <- #e#)"#,
    r#"t_(x[#I#L, ] <- #e#)"#,
    r#"t_(x[, #I#L] <- #e#)"#,
    r#"t_(x[#I#L, #I#L] <- #e#)"#,
    r#"t_(x[is.na(x)] <- #e#)"#,
    r#"t_(x[-1] <- #e#)"#,
    r#"t_(x[#N#] <- #e#)"#,
    r#"t_(x[[#N#]] <- #e#)"#,
    r#"t_(x[c(#N#, #N#)] <- list(#e#, #e#))"#,
    r#"t_(x[[#I#L]][[#I#L]] <- #e#)"#,
    r#"t_(x[#I#L][#I#L] <- #e#)"#,
    r#"t_(names(x) <- NULL)"#,
    r#"t_(names(x)[names(x) == #N#] <- "renamed")"#,
    r#"t_(attributes(x) <- NULL)"#,
    r#"t_(attr(x, "names") <- rev(names(x)))"#,
    r#"t_(mostattributes(x) <- list(names = "n", custom = #e#))"#,
    r#"t_(storage.mode(x) <- "character")"#,
    r#"t_(x[[1]] <- x)"#,
    r#"t_(x[2:3] <- x[1])"#,
    r#"t_(x[] <- #e#)"#,
    r#"t_(x[integer(0)] <- #e#)"#,
    r#"t_(x[NA] <- #e#)"#,
    r#"t_(x[0] <- #e#)"#,
    r#"t_(x[[#I#L]] <- NULL)"#,
    r#"t_(x[100L] <- #e#)"#,
    r#"t_(x[c(-1, -2)] <- #e#)"#,
    r#"t_(x[c(1, 1, 2)] <- c(#e#, #e#, #e#))"#,
    r#"t_(x[matrix(c(1L, 1L), 1)] <- #e#)"#,
    r#"t_(row.names(x)[1] <- "renamed row")"#,
    r#"t_(x[x == #e#] <- #e#)"#,
    r#"t_(levels(x) <- c(levels(x), "added"))"#,
    r#"t_(dim(x) <- c(length(x), 1L))"#,
    r#"t_(body(f_local) <- quote(p))"#,
    r#"log_("assignment value#L#", t_((x[#I#L] <- #e#)))"#,
    r#"log_("chained#L#", t_({ p <- q <- x; p[[1]] <- #e#; identical(q, x) }))"#,
    r#"log_("replacement function value#L#", t_(`second_#E#<-`(x, #e#)))"#,
];

const SUPER_ASSIGNMENT: &[&str] = &[
    r#"log_("bump#L#", bump_#E#(#I#L))"#,
    r#"log_("maker#L#", (function() { count <- 0L; add <- function() { count <<- count + 1L; count }; add(); add(); c(add(), count) })())"#,
    r#"leaked_#E# <<- t_(length(x))"#,
    r#"log_("shadow#L#", (function() { x <- "local"; (function() x <<- "changed")(); x })())"#,
    r#"log_("accumulate#L#", (function() { acc <- NULL; for (i in seq_len(#I#L)) (function() acc <<- c(acc, i * i))(); acc })())"#,
    r#"log_("sapply counter#L#", (function() { n <- 0; invisible(sapply(1:5, function(i) n <<- n + i)); n })())"#,
    r#"log_("memo#L#", (function() { memo <- c(1, 1); fib <- function(n) { if (n <= length(memo)) return(memo[n]); value <- fib(n - 1) + fib(n - 2); memo[n] <<- value; value }; c(fib(20), length(memo)) })())"#,
    r#"log_("skips local#L#", (function() { v <- "outer"; g <- function() { v <- "inner"; v <<- "assigned"; v }; c(g(), v) })())"#,
    r#"log_("element of outer#L#", (function() { v <- c(a = 1, b = 2); (function() v[["b"]] <<- 20)(); v })())"#,
    r#"log_("names of outer#L#", (function() { v <- 1:3; (function() names(v)[2] <<- #N#)(); v })())"#,
    r#"log_("outer list field#L#", (function() { state <- list(n = 0L); step <- function() state$n <<- state$n + 1L; step(); step(); state })())"#,
    r#"log_("two closures#L#", (function() { n <- 0L; up <- function() n <<- n + 1L; down <- function() n <<- n - 1L; up(); up(); down(); n })())"#,
    r#"log_("generator#L#", (function() { make <- function() { i <- 0L; function() { i <<- i + 1L; i } }; g1 <- make(); g2 <- make(); c(g1(), g1(), g2()) })())"#,
    r#"log_("counter value#L#", counter_#E#)"#,
    r#"log_("value of super assignment#L#", (function() { v <- 0; withVisible((function() v <<- 5)()) })())"#,
    r#"x <<- x"#,
    r#"log_("assign in parent#L#", (function() { v <- 1; (function() assign("v", 2, envir = parent.frame()))(); v })())"#,
    r#"log_("replacement in outer#L#", (function() { v <- list(1, 2); (function() attr(v, "tag") <<- #e#)(); attributes(v) })())"#,
];

const CONDITIONS: &[&str] = &[
    r#"log_("caught#L#", tryCatch({ #X#; "completed" }, error = function(e) conditionMessage(e)))"#,
    r#"log_("finally#L#", tryCatch({ #X#; "body" }, error = function(e) "handler", finally = log_("finally ran#L#", NULL)))"#,
    r#"log_("warned#L#", withCallingHandlers({ warning("w"); "after warning" }, warning = function(w) { log_("handling#L#", conditionMessage(w)); invokeRestart("muffleWarning") }))"#,
    r#"log_("warning caught#L#", tryCatch(as.integer(#S#), warning = function(w) class(w)))"#,
    r#"log_("custom#L#", tryCatch(signalCondition(structure(class = c("custom", "condition"), list(message = #S#, call = NULL))), custom = function(c) conditionMessage(c)))"#,
    r#"log_("unhandled signal#L#", signalCondition(simpleCondition("nobody listens"), NULL))"#,
    r#"log_("restart#L#", withRestarts(invokeRestart("retry", #e#), retry = function(v) list("restarted", v)))"#,
    r#"log_("restarts#L#", withRestarts(computeRestarts()[[1]]$name, pick = function() NULL))"#,
    r#"log_("try#L#", class(try(stop(#S#), silent = TRUE)))"#,
    r#"log_("try condition#L#", conditionMessage(attr(try(stop("tried"), silent = TRUE), "condition")))"#,
    r#"log_("rethrown#L#", tryCatch(tryCatch(stop("inner"), error = function(e) stop(paste("outer:", conditionMessage(e)))), error = conditionMessage))"#,
    r#"log_("loop escape#L#", (function() { out <- NULL; for (i in 1:5) { r <- tryCatch({ if (i == 2L) next; if (i == 4L) break; i }, finally = out <- c(out, -i)); out <- c(out, r) }; out })())"#,
    r#"log_("return through#L#", (function() { tryCatch(return("from body"), finally = log_("unwound#L#", NULL)); "not reached" })())"#,
    r#"log_("handler order#L#", withCallingHandlers(tryCatch({ message("note"); "done" }, message = function(m) "exiting"), message = function(m) log_("calling#L#", conditionMessage(m))))"#,
    r#"log_("classed error#L#", tryCatch(stop(structure(class = c("special", "error", "condition"), list(message = "m", call = quote(g(1))))), special = function(e) list(class(e), conditionCall(e))))"#,
    r#"log_("warning as error#L#", local({ old <- options(warn = 2); on.exit(options(old)); tryCatch(as.numeric("x"), error = function(e) conditionMessage(e)) }))"#,
    r#"log_("simple error#L#", tryCatch(stop(simpleError(#S#)), error = function(e) list(conditionMessage(e), is.null(conditionCall(e)))))"#,
    r#"log_("signal class#L#", tryCatch(1L + #e#, error = function(e) class_(e), warning = function(w) class(w)))"#,
    r#"log_("stopifnot#L#", tryCatch(stopifnot("must be long" = length(x) > 1000L), error = conditionMessage))"#,
    r#"log_("stopifnot unnamed#L#", tryCatch(stopifnot(is.numeric(x), length(x) == 1L), error = conditionMessage))"#,
    r#"log_("nested finally#L#", tryCatch({ tryCatch(stop("a"), finally = log_("inner finally#L#", NULL)) }, error = function(e) "outer caught"))"#,
    r#"log_("suppressed#L#", suppressWarnings({ warning("quiet"); "kept" }))"#,
    r#"log_("any condition#L#", tryCatch(message("hello"), condition = function(c) class(c)))"#,
    r#"log_("condition call#L#", tryCatch((function(p) stop("in call"))(1), error = function(e) conditionCall(e)))"#,
    r#"log_("error in handler#L#", tryCatch(tryCatch(stop("a"), error = function(e) stop("b")), error = conditionMessage))"#,
    r#"log_("muffled value#L#", withCallingHandlers(as.integer("3.7x"), warning = function(w) invokeRestart("muffleWarning")))"#,
    r#"log_("signalled#L#", { cond <- simpleCondition("msg", call = NULL); class(cond) <- c("mine", "condition"); withCallingHandlers(signalCondition(cond), mine = function(c) log_("saw#L#", class(c))) })"#,
    r#"log_("stop call.#L#", tryCatch((function() stop("no call", call. = FALSE))(), error = function(e) is.null(conditionCall(e))))"#,
    r#"log_("stop pieces#L#", tryCatch(stop("a", 1L, TRUE, #S#), error = conditionMessage))"#,
    r#"log_("warning pieces#L#", tryCatch(warning("w", 2.5, NULL), warning = conditionMessage))"#,
    r#"log_("warning returns#L#", withCallingHandlers(warning("returned"), warning = function(w) invokeRestart("muffleWarning")))"#,
    r#"log_("warning object#L#", tryCatch(warning(simpleWarning("made", call = quote(h()))), warning = function(w) list(class(w), conditionCall(w))))"#,
    r#"log_("message text#L#", tryCatch(message("a", "b", 1), message = conditionMessage))"#,
    r#"log_("message appended#L#", tryCatch(message("no newline", appendLF = FALSE), message = conditionMessage))"#,
    r#"log_("handlers stack#L#", withCallingHandlers(withCallingHandlers({ warning("deep"); "value" }, warning = function(w) log_("inner handler#L#", 1L)), warning = function(w) { log_("outer handler#L#", 2L); invokeRestart("muffleWarning") }))"#,
    r#"log_("handler not reentered#L#", tryCatch(withCallingHandlers(stop("first"), error = function(e) stop("second")), error = conditionMessage))"#,
    r#"log_("restart with handler#L#", withCallingHandlers(withRestarts({ warning("go"); "not here" }, skip = function() "skipped"), warning = function(w) invokeRestart("skip")))"#,
    r#"log_("try restart#L#", withRestarts(tryInvokeRestart("absent"), present = function() 1))"#,
    r#"log_("conditions of builtins#L#", list(tryCatch(log(-1), warning = conditionMessage), tryCatch(1:3 + 1:2, warning = conditionMessage), tryCatch(sqrt("a"), error = conditionMessage), tryCatch(integer(0)[[1]], error = conditionMessage), tryCatch(list(1)[[3]], error = conditionMessage), tryCatch(NULL$a$b, error = conditionMessage), tryCatch(undefined_variable, error = conditionMessage), tryCatch(undefined_function(), error = conditionMessage)))"#,
    r#"log_("error classes#L#", list(tryCatch(stop("s"), error = class), tryCatch(undefined_variable, error = class), tryCatch(as.integer("x"), warning = class), tryCatch(message("m"), message = class), tryCatch(signalCondition(simpleCondition("c")), condition = class), tryCatch(1[[5]], error = class), tryCatch(packageStartupMessage("p"), message = class)))"#,
    r#"log_("finally order#L#", (function() { order <- NULL; tryCatch({ order <- c(order, "body"); stop("e") }, error = function(e) order <<- c(order, "handler"), finally = order <- c(order, "finally")); order })())"#,
    r#"log_("warning then value#L#", (function() { seen <- NULL; value <- withCallingHandlers({ warning("one"); warning("two"); "end" }, warning = function(w) { seen <<- c(seen, conditionMessage(w)); invokeRestart("muffleWarning") }); list(value, seen) })())"#,
    r#"log_("try value#L#", try(#e#, silent = TRUE))"#,
    r#"log_("inherits which#L#", tryCatch(stop("s"), condition = function(c) inherits(c, c("warning", "error", "simpleError"), which = TRUE)))"#,
    r#"log_("error in finally#L#", tryCatch(tryCatch("fine", finally = stop("from finally")), error = conditionMessage))"#,
    r#"log_("interrupt class#L#", tryCatch(signalCondition(structure(class = c("interrupt", "condition"), list())), interrupt = function(i) "interrupted"))"#,
    r#"log_("conditionCall of primitive#L#", tryCatch(#e# + #S#, error = function(e) list(conditionMessage(e), class(conditionCall(e)))))"#,
    r#"log_("simpleCondition fields#L#", unclass(simpleCondition(#S#, call = quote(f(x)))))"#,
    r#"log_("errorCondition#L#", tryCatch(stop(errorCondition("typed", class = "typedError", data = #e#)), typedError = function(e) list(class(e), e$data)))"#,
    r#"log_("warningCondition#L#", tryCatch(warning(warningCondition("typed", class = "typedWarning")), typedWarning = function(w) class(w)))"#,
    r#"log_("tryCatch no condition#L#", tryCatch(#e#, error = function(e) "never", finally = NULL))"#,
    r#"log_("globalCallingHandlers absent#L#", length(globalCallingHandlers()))"#,
    r#"log_("geterrmessage after try#L#", { try(stop("remembered#L#"), silent = TRUE); geterrmessage() })"#,
];

const S3: &[&str] = &[
    r#"log_("describe#L#", t_(describe_#E#(x)))"#,
    r#"log_("describe value#L#", t_(describe_#E#(#V#)))"#,
    r#"log_("describe extra#L#", t_(describe_#E#(#S#, extra = #e#)))"#,
    r#"log_("box describe#L#", t_(describe_#E#(box)))"#,
    r#"log_("box length#L#", length(box))"#,
    r#"log_("box index#L#", t_(unclass(box[#I#L])))"#,
    r#"log_("box element#L#", t_(box[[#I#L]]))"#,
    r#"log_("box field#L#", box$anything)"#,
    r#"t_(box[#I#L] <- #e#)"#,
    r#"log_("box plus#L#", t_(box + #e#))"#,
    r#"log_("box unary#L#", t_(-box))"#,
    r#"log_("box compared#L#", t_(#e# == box))"#,
    r#"log_("box and box#L#", t_(box * box))"#,
    r#"log_("box math#L#", t_(sqrt(box)))"#,
    r#"log_("box round#L#", t_(round(box, 1)))"#,
    r#"log_("box summary#L#", t_(max(box, 1, na.rm = TRUE)))"#,
    r#"log_("box range#L#", t_(range(box)))"#,
    r#"log_("box format#L#", format(box))"#,
    r#"log_("box printed#L#", capture.output(print(box)))"#,
    r#"log_("box combined#L#", c(box, 1))"#,
    r#"log_("box as text#L#", as.character(box))"#,
    r#"log_("box inherits#L#", list(inherits(box, "box_#E#"), inherits(box, c("a", "box_#E#"), which = TRUE), is.list(box), inherits(box, what = "list")))"#,
    r#"log_("box unclassed#L#", unclass(box))"#,
    r#"log_("box seq_along#L#", seq_along(box))"#,
    r#"log_("box lapply#L#", t_(lapply(box, class)))"#,
    r#"log_("box reversed#L#", t_(rev(box)))"#,
    r#"log_("box identical#L#", identical(box, box))"#,
    r#"log_("box unique#L#", t_(unclass(unique(box))))"#,
    r#"log_("box rep#L#", t_(unclass(rep(box, 2))))"#,
    r#"log_("box is.na#L#", t_(is.na(box)))"#,
    r#"log_("box dim#L#", dim(box))"#,
    r#"log_("box str#L#", capture.output(str(unclass(box))))"#,
    r#"log_("box toString#L#", t_(toString(box)))"#,
    r#"log_("box paste#L#", paste(box))"#,
    r#"log_("box as.list#L#", as.list(box))"#,
    r#"log_("box for#L#", { seen <- NULL; for (item in box) seen <- c(seen, class(item)); seen })"#,
    r#"log_("box vapply#L#", t_(vapply(box, function(item) class(item)[1], "")))"#,
    r#"log_("box length<-#L#", t_({ shorter <- box; length(shorter) <- 2L; unclass(shorter) }))"#,
    r#"log_("box names<-#L#", t_({ named <- box; names(named) <- c("p", "q", "r"); attributes(named) }))"#,
    r#"log_("methods found#L#", c(exists("describe_#E#.box_#E#"), is.function(getS3method("describe_#E#", "box_#E#", optional = TRUE)), is.null(getS3method("describe_#E#", "absent", optional = TRUE))))"#,
    r#"log_("two classes#L#", t_(describe_#E#(structure(1L, class = c("absent", "box_#E#")))))"#,
    r#"log_("dispatch on NULL#L#", t_(describe_#E#(NULL)))"#,
    r#"log_("dispatch on missing#L#", t_(describe_#E#()))"#,
    r#"local({ describe_#E#.nearby <- function(x, ...) "local method"; log_("local method#L#", t_(describe_#E#(structure(1, class = "nearby")))) })"#,
    r#"log_("implicit class#L#", list(class(matrix(1:4, 2)), class(array(1, c(1, 1, 1))), class(1L), class(sum), class(quote(x)), class(quote(x + 1)), class(NULL), oldClass(1L), inherits(1L, "integer"), inherits(matrix(1), "matrix")))"#,
    r#"log_("NextMethod arguments#L#", (function() { gen <- function(x, ...) UseMethod("gen"); gen.default <- function(x, extra = "none", ...) list(unclass(x), extra); gen.child <- function(x, ...) NextMethod(extra = "passed"); gen(structure(1, class = "child")) })())"#,
    r#"log_("NextMethod changed x#L#", (function() { gen <- function(x) UseMethod("gen"); gen.default <- function(x) unclass(x); gen.child <- function(x) { x <- structure(99, class = "child"); NextMethod() }; gen(structure(1, class = "child")) })())"#,
    r#"log_("dispatch after UseMethod#L#", (function() { gen <- function(x) { marker <- "set"; UseMethod("gen"); "never" }; gen.default <- function(x) exists("marker", inherits = FALSE); gen(1) })())"#,
    r#"log_("group generic default#L#", t_(unclass(structure(1:3, class = "plain") + 1L)))"#,
    r#"log_("Ops mismatch#L#", (function() { Ops.left <- function(e1, e2) "left"; Ops.right <- function(e1, e2) "right"; withCallingHandlers(t_(structure(1, class = "left") + structure(2, class = "right")), warning = function(w) { log_("ops warning#L#", conditionMessage(w)); invokeRestart("muffleWarning") }) })())"#,
    r#"log_("print dispatch#L#", capture.output(print(structure(list(1), class = "box_#E#")), if (is.function(box)) "a function" else box))"#,
    r#"log_("format method in paste#L#", t_(paste0("[", format(box), "]")))"#,
    r#"log_("unclass in method#L#", t_(describe_#E#(unclass(box))))"#,
    r#"log_("class<- dispatch#L#", t_({ y <- x; class(y) <- "box_#E#"; length(y) }))"#,
    r#"log_("inherits default#L#", t_(describe_#E#(structure(list(), class = c("box_#E#", "data.frame")))))"#,
    r#"log_("all.equal on box#L#", t_(all.equal(box, box)))"#,
    r#"log_("sort on box#L#", t_(sort(structure(c(3, 1, 2), class = "plain"))))"#,
    r#"log_("seq method#L#", t_(seq(structure(2L, class = "plain"))))"#,
    r#"log_("as.vector on classed#L#", as.vector(structure(1:2, class = "plain", names = c("p", "q"))))"#,
];

const S4: &[&str] = &[
    r#"log_("area#L#", t_(area_#E#(obj)))"#,
    r#"log_("area of value#L#", t_(area_#E#(#V#)))"#,
    r#"log_("is#L#", list(is(obj), is(obj, "Base_#E#"), isVirtualClass("Either_#E#")))"#,
    r#"log_("slots#L#", slotNames(obj))"#,
    r#"log_("shown#L#", capture.output(print(obj)))"#,
    r#"log_("shown bare#L#", capture.output(obj))"#,
    r#"log_("S4 length#L#", length(obj))"#,
    r#"log_("S4 plus#L#", t_((obj + obj)@id))"#,
    r#"log_("S4 arith#L#", t_((obj * 3)@id))"#,
    r#"log_("S4 arith other#L#", t_(obj - "a"))"#,
    r#"log_("S4 index#L#", t_(obj[#I#L]))"#,
    r#"t_(obj@id <- #e#)"#,
    r#"t_(obj@label <- #S#)"#,
    r#"t_(obj@absent <- 1)"#,
    r#"log_("id replaced#L#", t_({ id_#E#(obj) <- c(5, 6); obj@id }))"#,
    r#"log_("id invalid#L#", t_({ id_#E#(obj) <- c(1, 2, 3); obj@id }))"#,
    r#"log_("valid#L#", t_(validObject(obj)))"#,
    r#"log_("new invalid#L#", t_(new("Derived_#E#", id = c(1, 2, 3))))"#,
    r#"log_("new bad slot#L#", t_(new("Base_#E#", nothing = 1)))"#,
    r#"log_("new bad type#L#", t_(new("Base_#E#", id = "text")))"#,
    r#"log_("methods exist#L#", c(existsMethod("area_#E#", "Derived_#E#"), hasMethod("area_#E#", "Derived_#E#"), isGeneric("area_#E#"), existsMethod("area_#E#", "list"), hasMethod("area_#E#", "integer")))"#,
    r#"log_("slot#L#", slot(obj, "label"))"#,
    r#"log_("S4 identical#L#", identical(obj, new("Derived_#E#", id = c(1, 2))))"#,
    r#"log_("extends#L#", c(extends("Derived_#E#", "Base_#E#"), extends("Base_#E#", "Derived_#E#"), extends("numeric", "Either_#E#")))"#,
    r#"log_("S4 type#L#", list(typeof(obj), isS4(obj), is.object(obj), class(obj), isVirtualClass("Base_#E#")))"#,
    r#"log_("S4 attributes#L#", names(attributes(obj)))"#,
    r#"log_("no method#L#", t_(area_#E#(sum)))"#,
    r#"log_("initialized#L#", new("Derived_#E#")@extra)"#,
    r#"log_("union slot#L#", t_(new("Holder_#E#", value = #e#)@value))"#,
    r#"log_("as parent#L#", t_(as(obj, "Base_#E#")@id))"#,
    r#"log_("is parent after as#L#", t_(class(as(obj, "Base_#E#"))))"#,
    r#"log_("as basic#L#", t_(as(#e#, "character")))"#,
    r#"log_("as numeric#L#", t_(as(#S#, "numeric")))"#,
    r#"log_("slot of copy#L#", { other <- obj; other@id <- 9; c(obj@id, other@id) })"#,
    r#"log_("isVirtualClass#L#", t_(new("Either_#E#")))"#,
    r#"log_("is method#L#", is(selectMethod("area_#E#", "Derived_#E#"), "function"))"#,
    r#"log_("inherited method#L#", t_(area_#E#(1L)))"#,
    r#"log_("show method#L#", t_(isVirtualClass("Base_#E#") || is.function(getMethod("show", "Base_#E#"))))"#,
    r#"log_("slots of new#L#", t_({ fresh <- new("Base_#E#"); list(fresh@id, fresh@label) }))"#,
    r#"log_("initialize arguments#L#", t_(new("Derived_#E#", id = 1, extra = list(#e#))@extra))"#,
    r#"log_("unnamed new argument#L#", t_(new("Derived_#E#", new("Base_#E#", id = 7))@id))"#,
    r#"log_("isVirtual and validity#L#", t_(validObject(new("Base_#E#", id = 1:3 + 0))))"#,
    r#"log_("methods::is on basics#L#", list(is(1L, "numeric"), is(1L, "integer"), is("a", "Either_#E#"), is(NULL, "NULL"), is(sum, "function"), is(1, "Either_#E#")))"#,
    r#"log_("slot<-#L#", t_({ slot(obj, "label") <- "set"; obj@label }))"#,
    r#"log_("slot<- unchecked#L#", t_({ slot(obj, "label", check = FALSE) <- 1L; class(obj@label) }))"#,
    r#"log_("isVirtualClass of union member#L#", t_(is(obj@id, "Either_#E#")))"#,
    r#"log_("identical slots#L#", identical(obj@extra, list("initialized")))"#,
    r#"log_("S4 in list#L#", t_(vapply(list(obj, 1, "a"), function(item) class(item)[1], "")))"#,
    r#"log_("S4 str#L#", t_(isVirtualClass("Holder_#E#")))"#,
    r#"log_("callGeneric#L#", t_((obj / 2)@id))"#,
    r#"log_("method on subclass object#L#", t_(area_#E#(new("Base_#E#", id = c(3, 4)))))"#,
];

const REFERENCE_CLASSES: &[&str] = &[
    r#"account$deposit(#I#)"#,
    r#"log_("total#L#", account$total())"#,
    r#"log_("chained#L#", account$deposit(1)$deposit(2)$balance)"#,
    r#"local({ other <- account; other$deposit(5); log_("shared#L#", account$balance) })"#,
    r#"local({ copy <- account$copy(); copy$deposit(100); log_("copied#L#", c(account$balance, copy$balance)) })"#,
    r#"t_(account$balance <- #e#)"#,
    r#"log_("field#L#", account$field("rate"))"#,
    r#"log_("field set#L#", { account$field("rate", 0.25); account$rate })"#,
    r#"log_("class#L#", class(account))"#,
    r#"log_("is#L#", c(is(account, "Account_#E#"), is(account, "envRefClass"), isVirtualClass("Account_#E#")))"#,
    r#"log_("shown#L#", capture.output(print(account)))"#,
    r#"log_("history#L#", length(account$history))"#,
    r#"log_("unknown#L#", t_(account$nothing))"#,
    r#"log_("bad field#L#", t_(Account_#E#$new(balance = "x")))"#,
    r#"log_("bad argument#L#", t_(Account_#E#$new(nothing = 1)))"#,
    r#"log_("fields#L#", Account_#E#$fields())"#,
    r#"log_("identical#L#", c(identical(account, account), identical(account, account$copy())))"#,
    r#"log_("as environment#L#", is.environment(as.environment(account)))"#,
    r#"log_("default#L#", Account_#E#$new()$balance)"#,
    r#"log_("parent method#L#", Account_#E#$new(balance = 3)$deposit(2)$total())"#,
    r#"log_("modified in function#L#", { pay <- function(target) target$deposit(7); pay(account); account$balance })"#,
    r#"log_("modified in lapply#L#", { invisible(lapply(1:3, function(i) account$deposit(i))); length(account$history) })"#,
    r#"log_("self#L#", identical(account$deposit(0), account))"#,
    r#"log_("show on parent#L#", capture.output(Account_#E#$new(balance = 1)))"#,
    r#"log_("generator class#L#", class(Savings_#E#)[1])"#,
    r#"log_("class name#L#", Savings_#E#$className)"#,
    r#"log_("methods listed#L#", all(c("deposit", "total", "copy") %in% Savings_#E#$methods()))"#,
    r#"log_("field via double bracket#L#", t_(account[["balance"]]))"#,
    r#"log_("field of copy unchanged#L#", { before <- account$copy(); account$deposit(1); before$balance == account$balance })"#,
    r#"log_("isVirtualClass#L#", t_(account$rate * account$total()))"#,
];

const EVAL: &[&str] = &[
    r#"log_("quoted#L#", quote(#Q#))"#,
    r#"log_("round trip#L#", { e <- quote(#Q#); list(identical(e, parse(text = deparse(e), keep.source = FALSE)[[1]]), class(e), length(e)) })"#,
    r#"log_("names in#L#", all.names(quote(#Q#)))"#,
    r#"log_("variables in#L#", all.vars(quote(#Q#)))"#,
    r#"log_("evaluated#L#", t_(eval(quote(#q#), list(a = 2, b = 3, x1 = 1:3, f = function(...) ..1, i = 1L))))"#,
    r#"log_("deparsed wide#L#", deparse(quote(#Q#), width.cutoff = 500L))"#,
    r#"log_("deparsed narrow#L#", deparse(quote(#Q#), width.cutoff = 20L))"#,
    r#"log_("deparsed function#L#", deparse(eval(quote(function(p, q = #e#, ...) #Q#))))"#,
    r#"log_("walked#L#", (function(e) if (is.call(e)) lapply(as.list(e), sys.function()) |> length() else class(e))(quote(#Q#)))"#,
    r#"log_("bquote#L#", t_(bquote(.(x) + z)))"#,
    r#"log_("bquote nested#L#", bquote(f(.(1 + 2), g(.(#e#)), ..(list(1, 2))), splice = TRUE))"#,
    r#"log_("substituted#L#", substitute(p + q, list(p = 1L, q = quote(r * 2))))"#,
    r#"log_("eval list#L#", eval(quote(p * 2L), list(p = #I#L)))"#,
    r#"log_("eval enclos#L#", eval(quote(p + bonus), list(p = 1L), enclos = list2env(list(bonus = 10L))))"#,
    r#"log_("eval parent#L#", (function() eval.parent(quote(nargs())))())"#,
    r#"log_("evalq#L#", evalq(x, list(x = "shadow")))"#,
    r#"log_("do.call by name#L#", do.call("paste", list("a", "b", sep = "-")))"#,
    r#"log_("do.call names#L#", do.call(function(...) names(list(...)), list(#N# = 1, 2)))"#,
    r#"log_("do.call quoted#L#", t_(do.call("list", list(quote(undefined_symbol), 1), quote = TRUE)))"#,
    r#"log_("do.call character#L#", t_(do.call(#S#, list(1))))"#,
    r#"log_("call built#L#", { cl <- call("sum", 1, 2, 3); cl[[1]] <- as.name("prod"); list(cl, eval(cl)) })"#,
    r#"log_("as.call#L#", eval(as.call(list(as.name("max"), 1, 5, 3))))"#,
    r#"log_("call edited#L#", { cl <- quote(g(a, b = 2)); cl$b <- NULL; cl[[3]] <- quote(z); cl$new <- 1; cl })"#,
    r#"log_("body#L#", body(function(p) { p + 1 }))"#,
    r#"log_("formals#L#", formals(function(p, q = 2, ...) NULL))"#,
    r#"log_("match.fun#L#", match.fun("+")(1, 2))"#,
    r#"log_("reduced call#L#", Reduce(function(l, r) call("+", l, r), list(1, 2, quote(z))))"#,
    r#"log_("expression#L#", { ex <- expression(a + 1, b); list(length(ex), ex[[1]], class(ex[2])) })"#,
    r#"log_("local eval#L#", local({ p <- #I#L; eval(quote(p + 1L)) }))"#,
    r#"log_("with#L#", with(list(p = 2L, q = 3L), p^q))"#,
    r#"log_("within#L#", within(list(p = 1), { q <- p + 1; rm(p) }))"#,
    r#"log_("classes of language#L#", list(class(quote(x)), class(quote(x + 1)), class(quote((x))), class(quote(if (a) b)), class(quote(function(x) x)), class(quote(x[1])), class(quote(x <- 1)), class(quote({})), typeof(quote(1L)), mode(quote(x$y)), class(quote(for (i in x) i)), class(quote(while (a) b)), class(quote(a = b)), storage.mode(quote(x))))"#,
    r#"log_("as.function#L#", as.function(alist(p = , q = 2, p + q))(1))"#,
    r#"log_("formula#L#", { fm <- y ~ a + b | c; list(length(fm), fm[[2]], all.vars(fm), class(fm[[3]])) })"#,
    r#"log_("parse several#L#", vapply(parse(text = "a; b <- 2\nf(x)", keep.source = FALSE), function(e) class(e), ""))"#,
    r#"log_("parse error#L#", t_(parse(text = #S#, keep.source = FALSE)))"#,
    r#"log_("parse of text#L#", t_(parse(text = deparse(#V#), keep.source = FALSE)[[1]]))"#,
    r#"log_("deparse of value#L#", t_(identical(eval(parse(text = deparse(x), keep.source = FALSE)[[1]]), x)))"#,
    r#"log_("dput of value#L#", t_(capture.output(dput(x))))"#,
    r#"log_("deparse controls#L#", t_(list(deparse(#V#, control = NULL), deparse(#V#, control = "all"), deparse(#V#, control = c("keepNA", "keepInteger", "niceNames", "showAttributes")), deparse(#V#, control = "exact"))))"#,
    r#"log_("deparse nlines#L#", deparse(quote(#Q#), nlines = 1L))"#,
    r#"log_("deparse backtick#L#", list(deparse(as.name("a b")), deparse(as.name("a b"), backtick = TRUE), deparse(quote(`a b` + 1)), deparse(as.name("if")), deparse(quote(`if`(a, b))), deparse(as.name(#N#))))"#,
    r#"log_("call with value#L#", { cl <- as.call(list(as.name("length"), x)); t_(eval(cl)) })"#,
    r#"log_("function in call#L#", eval(as.call(list(function(p) p * 2, 21))))"#,
    r#"log_("sys.call from eval#L#", eval(quote(match.call()), envir = environment()))"#,
    r#"log_("Recall through do.call#L#", do.call(function(n) if (n > 0) Recall(n - 1) else "done", list(3)))"#,
    r#"log_("substitute at depth#L#", (function(p) (function(q) substitute(q))(p + 1))(x))"#,
    r#"log_("quote of constants#L#", list(quote(1L), quote("s"), quote(TRUE), quote(NULL), quote(-1), class(quote(-1)), quote(1e3), quote(0x10), quote(1e-5), quote(100000), quote(1e5L), quote(.5), quote(1i)))"#,
    r#"log_("identical language#L#", list(identical(quote(a + b), quote(a+b)), identical(quote(f(x)), call("f", quote(x))), identical(as.name("x"), quote(x)), identical(quote((a)), quote(a))))"#,
    r#"log_("language length#L#", list(length(quote(f(a, b))), length(quote(x)), length(quote(function(p) p)), length(quote(if (a) b else c)), length(quote(a[b, c])), length(quote({ a; b }))))"#,
    r#"log_("call replaced head#L#", { cl <- quote(f(1, 2)); cl[[1]] <- quote(pkg::g); cl })"#,
    r#"log_("call by index#L#", { cl <- quote(f(a, b = 2, c)); list(cl[-1], cl[c(1, 3)], names(cl), as.list(cl)$b) })"#,
    r#"log_("as.character of language#L#", list(as.character(quote(a + b)), as.character(quote(f(x, 1))), as.character(quote(x)), as.character(expression(a, b + 1))))"#,
    r#"log_("Negate and compose#L#", Negate(is.null)(NULL))"#,
    r#"log_("environment of function#L#", identical(environment(function() NULL), environment()))"#,
    r#"log_("args and arity#L#", list(names(formals(function(p, ...) NULL)), length(formals(sum)), is.primitive(sum), is.primitive(mean), class(args(sum))))"#,
];

const ENVIRONMENTS: &[&str] = &[
    r#"local({ e <- new.env(); assign("k", #e#, envir = e); log_("looked up#L#", list(get("k", e), exists("k", e), exists("zz", e, inherits = FALSE), mget(c("k", "zz"), envir = e, ifnotfound = list("absent")))) })"#,
    r#"local({ e <- new.env(); e$b <- 1; e$a <- 2; lockBinding("a", e); log_("locked#L#", list(t_(e$a <- 3), bindingIsLocked("a", e), sort(ls(e)))); lockEnvironment(e); log_("locked environment#L#", list(t_(e$c <- 1), t_(e$b <- 5), environmentIsLocked(e))) })"#,
    r#"local({ e <- new.env(); n <- 0L; makeActiveBinding("ticker", function() { n <<- n + 1L; n }, e); log_("active#L#", c(e$ticker, e$ticker, get("ticker", e), bindingIsActive("ticker", e))) })"#,
    r#"local({ e <- new.env(); makeActiveBinding("cell", function(v) if (missing(v)) "read" else log_("written#L#", v), e); log_("active read#L#", e$cell); e$cell <- #e#; assign("cell", #e#, e) })"#,
    r#"log_("list2env#L#", { e <- list2env(list(p = 1, q = "two")); list(sort(ls(e)), mget(c("p", "q"), envir = e)) })"#,
    r#"log_("empty parent#L#", { e <- new.env(parent = emptyenv()); list(t_(eval(quote(1 + 1), e)), environmentName(parent.env(e))) })"#,
    r#"log_("environment replaced#L#", { g <- function() secret; environment(g) <- list2env(list(secret = #I#L)); g() })"#,
    r#"log_("removed#L#", { p <- 1; rm(p); exists("p", inherits = FALSE) })"#,
    r#"log_("remove absent#L#", withCallingHandlers(t_(rm("never_bound")), warning = function(w) { log_("rm warning#L#", conditionMessage(w)); invokeRestart("muffleWarning") }))"#,
    r#"log_("local scope#L#", { p <- "outer"; local({ p <- "inner" }); p })"#,
    r#"log_("frame identity#L#", { e <- environment(); identical(e, (function() parent.frame())()) })"#,
    r#"log_("names sorted#L#", ls(list2env(list(b = 1, a = 2, .hidden = 3)), all.names = TRUE))"#,
    r#"log_("names unsorted#L#", sort(names(list2env(list(b = 1, a = 2, .hidden = 3)))))"#,
    r#"log_("value kept#L#", { e <- new.env(); e$only <- x; identical(as.list(e)$only, x) })"#,
    r#"log_("eapply#L#", { e <- list2env(list(p = 1:3)); eapply(e, length) })"#,
    r#"log_("attribute by reference#L#", { e <- new.env(); attr(e, "tag") <- "t"; h <- e; attr(h, "tag") })"#,
    r#"log_("function lookup#L#", { c <- 5; list(c(1, 2), get("c", mode = "function")(3), exists("c", mode = "numeric")) })"#,
    r#"log_("function lookup skips#L#", { sum <- 10; sum(1, 2) + sum })"#,
    r#"log_("delayed in environment#L#", { e <- new.env(); delayedAssign("late", log_("late forced#L#", 7L), assign.env = e); list(exists("late", e), e$late) })"#,
    r#"log_("sizes#L#", list(length(new.env()), length(list2env(list(p = 1, q = 2))), t_(get("x", emptyenv()))))"#,
    r#"log_("environment names#L#", c(environmentName(globalenv()), environmentName(baseenv()), environmentName(emptyenv()), environmentName(topenv()), environmentName(environment(sd)), environmentName(parent.env(globalenv())) != ""))"#,
    r#"log_("non-ASCII binding#L#", { e <- new.env(); assign(#N#, #e#, envir = e); list(ls(e), exists(#N#, e), Encoding(ls(e))) })"#,
    r#"log_("binding by string#L#", { e <- new.env(); e[[#N#]] <- 1L; e[["other"]] <- 2L; sort(ls(e)) })"#,
    r#"log_("dollar on environment#L#", { e <- list2env(list(alpha = 1)); list(e$alpha, e$alp, e[["alpha"]], t_(e[[1]]), t_(e["alpha"])) })"#,
    r#"log_("assign returns#L#", withVisible(assign("p", #e#)))"#,
    r#"log_("get inherits#L#", list(exists("log_"), exists("log_", inherits = FALSE), t_(get("absent_binding")), get0("absent_binding", ifnotfound = "fallback")))"#,
    r#"log_("mget missing#L#", t_(mget(c("x", "absent_binding"))))"#,
    r#"log_("environment equality#L#", { e <- new.env(); list(identical(e, e), identical(e, new.env()), t_(e == e), is.environment(e), is.list(e)) })"#,
    r#"log_("closure environment#L#", { make <- function(v) function() v; g <- make(#e#); list(g(), ls(environment(g)), environmentName(parent.env(environment(g)))) })"#,
    r#"log_("sys.function environment#L#", identical(parent.env(environment()), globalenv()))"#,
    r#"log_("as.environment#L#", list(identical(as.environment(1), globalenv()), is.environment(as.environment(list(p = 1))), t_(as.environment("absent"))))"#,
    r#"log_("local with environment#L#", { e <- list2env(list(v = #I#L)); local(v * 2L, envir = e) })"#,
    r#"log_("nested functions see updates#L#", { v <- 1; g <- function() v; v <- 2; g() })"#,
    r#"log_("rm then lookup#L#", { length <- function(p) "shadowed"; first <- length(1:3); rm(length); list(first, length(1:3)) })"#,
    r#"log_("environment as list#L#", { e <- list2env(list(b = 2, a = 1)); l <- as.list(e); l[order(names(l))] })"#,
    r#"log_("list2env onto#L#", { e <- list2env(list(p = 1)); list2env(list(p = 2, q = 3), envir = e); mget(c("p", "q"), envir = e) })"#,
    r#"log_("exists after assign in loop#L#", { e <- new.env(); for (name in c("p", "q", "p")) assign(name, name, envir = e); length(ls(e)) })"#,
    r#"log_("hash and size#L#", { e <- new.env(hash = TRUE, size = 1L); for (i in 1:50) assign(paste0("v", i), i, envir = e); c(length(e), sum(unlist(mget(ls(e), envir = e)))) })"#,
    r#"log_("unlock#L#", { e <- new.env(); e$p <- 1; lockBinding("p", e); unlockBinding("p", e); e$p <- 2; e$p })"#,
    r#"log_("locked frame variable#L#", { p <- 1; lockBinding("p", environment()); r <- t_(p <- 2); unlockBinding("p", environment()); r })"#,
];

const CONTROL_FLOW: &[&str] = &[
    r#"log_("loop#L#", { out <- NULL; for (i in seq_len(#I#L + 2L)) { if (i %% 2L == 0L) next; if (i > 5L) break; out <- c(out, i) }; out })"#,
    r#"log_("while#L#", { n <- 0L; while (TRUE) { n <- n + 1L; if (n >= #I#L) break }; n })"#,
    r#"log_("repeat#L#", { n <- 10L; repeat { n <- n - 3L; if (n < 0L) break }; n })"#,
    r#"log_("for over x#L#", t_({ n <- 0L; for (item in x) n <- n + 1L; n }))"#,
    r#"log_("for over value#L#", t_({ kinds <- NULL; for (item in #V#) kinds <- c(kinds, class(item)[1]); unique(kinds) }))"#,
    r#"log_("for value#L#", { r <- for (i in 1:3) i; is.null(r) })"#,
    r#"log_("for variable after#L#", { item <- "before"; for (item in #V#) NULL; t_(item) })"#,
    r#"log_("loop changes sequence#L#", { s <- 1:3; out <- NULL; for (i in s) { s <- c(s, 9L); out <- c(out, i) }; list(out, length(s)) })"#,
    r#"log_("loop changes variable#L#", { out <- NULL; for (i in 1:3) { out <- c(out, i); i <- 10L }; list(out, i) })"#,
    r#"log_("switch#L#", vapply(list("a", "b", "c", "z"), function(k) switch(k, a = , b = "ab", c = "c", "other"), ""))"#,
    r#"log_("switch numeric#L#", list(switch(2L, "one", "two"), is.null(switch(4L, "one")), t_(switch("q", a = 1)), is.null(switch("b", a = 1, b = )), t_(switch(NULL, 1)), t_(switch(c(1, 2), 1)), switch(2.9, "one", "two")))"#,
    r#"log_("switch on value#L#", t_(switch(#e#, a = "first", b = "second", "third")))"#,
    r#"log_("switch on string#L#", t_(switch(#S#, a = "A", "1" = "one", "NA" = "na text", "fallback")))"#,
    r#"log_("visibility#L#", list(withVisible(invisible(1))$visible, withVisible((invisible(1)))$visible, withVisible(p <- 2)$visible, withVisible(if (FALSE) 1)$visible, withVisible(for (i in 1) i)$visible, withVisible(NULL)$visible, withVisible((function() invisible(3))())$visible, withVisible(assign("q", 1))$visible, withVisible((function() p <- 1)())$visible, withVisible(switch("z", a = 1))$visible, withVisible(repeat break)$visible, withVisible(while (FALSE) 1)$visible, withVisible(f <- function() 1)$visible, withVisible(invisible())$visible, withVisible({ invisible(1); 2 })$visible, withVisible(if (TRUE) invisible(1))$visible, withVisible(identity(invisible(1)))$visible, withVisible(suppressWarnings(invisible(1)))$visible, withVisible(tryCatch(invisible(1), error = identity))$visible, withVisible(local(invisible(1)))$visible, withVisible(eval(quote(invisible(1))))$visible, withVisible(do.call(invisible, list(1)))$visible, withVisible(x[[1]] <- x[[1]])$visible, withVisible(library)$visible, withVisible(set.seed(1))$visible, withVisible(print)$visible))"#,
    r#"log_("recursion#L#", (function(n) if (n <= 1L) 1 else n * sys.function()(n - 1L))(#I#L + 3L))"#,
    r#"log_("deep recursion#L#", (function(n) if (n == 0L) 0L else 1L + Recall(n - 1L))(300L))"#,
    r#"log_("vapply#L#", t_(vapply(x, function(item) length(item), 0L)))"#,
    r#"log_("mapply#L#", mapply(function(p, q) p + q, 1:3, c(a = 10, b = 20, c = 30)))"#,
    r#"log_("mapply names#L#", mapply(function(p, q) paste(p, q), c(#N#, "k"), 1:2))"#,
    r#"log_("Map#L#", Map(function(p, q) paste(p, q), c(first = "a", second = "b"), 1:2))"#,
    r#"log_("Reduce#L#", Reduce(`+`, 1:5, accumulate = TRUE, right = TRUE))"#,
    r#"log_("Reduce init#L#", Reduce(function(l, r) paste0(l, r), list("a", "b", "c"), accumulate = TRUE, "z"))"#,
    r#"log_("Filter#L#", Filter(function(v) v > 1, c(a = 1, b = 2, c = 3)))"#,
    r#"log_("Position and Find#L#", list(Position(function(v) v > 1, c(1, 2, 3)), Find(function(v) v > 5, c(1, 2, 3)), Position(function(v) v > 1, c(1, 2, 3), right = TRUE)))"#,
    r#"log_("sapply shapes#L#", list(sapply(1:3, function(i) c(i, i^2)), sapply(list(), length), sapply(c(a = 1), function(v) NULL), sapply(1:2, function(i) list(i)), sapply(c("p", "q"), toupper), sapply(1:2, function(i) if (i == 1L) 1L else "s"), sapply(1:3, function(i) letters[i], USE.NAMES = FALSE), sapply(c("p", "q"), nchar, simplify = FALSE)))"#,
    r#"log_("outer#L#", outer(1:2, 1:3, function(p, q) p * 10L + q))"#,
    r#"log_("apply#L#", t_(apply(x, 1, function(row) length(row))))"#,
    r#"log_("apply over value#L#", t_(apply(#V#, 2, function(column) class(column))))"#,
    r#"log_("rapply#L#", rapply(list(1, "a", list(2, "b")), function(v) v * 10, classes = "numeric", how = "unlist"))"#,
    r#"log_("condition NA#L#", t_(if (NA) 1))"#,
    r#"log_("condition long#L#", t_(if (c(TRUE, FALSE)) 1))"#,
    r#"log_("condition text#L#", t_(if ("T") "yes"))"#,
    r#"log_("condition of x#L#", t_(if (x) "truthy" else "falsy"))"#,
    r#"log_("condition of value#L#", t_(if (#e#) "truthy" else "falsy"))"#,
    r#"log_("while of x#L#", t_(while (x) break))"#,
    r#"log_("logic#L#", list(NA && FALSE, NA || TRUE, t_(NULL && TRUE), t_("a" || TRUE), t_(c(TRUE, TRUE) && TRUE), isTRUE(c(TRUE, TRUE)), isFALSE(NA), xor(TRUE, NA), TRUE && #e# == #e#))"#,
    r#"log_("index loops#L#", { out <- 0L; for (i in seq_along(x)) out <- out + i; for (j in seq_len(length(x))) out <- out - j; out })"#,
    r#"log_("mutual recursion#L#", (function() { g <- function(n) if (n > 0L) h(n - 1L) else "bottom"; h <- function(n) g(n); g(5L) })())"#,
    r#"log_("return from loop#L#", (function() { for (i in 1:10) if (i == #I#L) return(i); "none" })())"#,
    r#"log_("return in sapply#L#", sapply(1:3, function(i) { repeat { if (i > 1L) return(i * 2L); break }; -1L }))"#,
    r#"log_("ifelse#L#", t_(ifelse(x > 1, "big", "small")))"#,
    r#"log_("ifelse shapes#L#", list(ifelse(c(a = TRUE, b = FALSE), 1, "n"), ifelse(NA, 1, 2), ifelse(logical(0), 1, 2), ifelse(matrix(c(TRUE, FALSE), 1), 1:2, 3:4), ifelse(#e#, "yes", "no")))"#,
    r#"log_("pipe#L#", c(1, 4, 9) |> sqrt() |> sum())"#,
    r#"log_("pipe placeholder#L#", list(a = 1:3) |> _$a |> rev())"#,
    r#"log_("lambda#L#", (\(p, q = 2) p^q)(3))"#,
    r#"log_("vapply mismatch#L#", t_(vapply(1:2, function(i) "s", 0)))"#,
    r#"log_("vapply names#L#", vapply(c(#N#, "k"), function(name) nchar(name), 0L))"#,
    r#"log_("lapply over value#L#", t_(lapply(#V#, function(item) typeof(item))))"#,
    r#"log_("seq_along and seq_len#L#", list(seq_along(x), seq_len(length(x)), t_(seq_len(-1)), rev(seq_along(x))))"#,
    r#"log_("break in nested#L#", { out <- NULL; for (i in 1:3) { for (j in 1:3) { if (j == 2L) break; out <- c(out, i * 10L + j) }; if (i == 2L) next; out <- c(out, -i) }; out })"#,
    r#"log_("next in repeat#L#", { n <- 0L; total <- 0L; repeat { n <- n + 1L; if (n > 6L) break; if (n %% 2L == 0L) next; total <- total + n }; total })"#,
    r#"log_("while with next#L#", { n <- 0L; kept <- NULL; while (n < 6L) { n <- n + 1L; if (n == 3L) next; kept <- c(kept, n) }; kept })"#,
    r#"log_("for over list#L#", { classes <- NULL; for (item in list(1L, "a", NULL, c(1, 2), sum)) classes <- c(classes, class(item)[1]); classes })"#,
    r#"log_("for over factor and date#L#", list({ out <- NULL; for (level in factor(c("b", "a"))) out <- c(out, level); out }, { out <- NULL; for (day in as.Date("2024-01-01") + 0:1) out <- c(out, class(day)); out }))"#,
    r#"log_("do.call in loop#L#", { total <- 0; for (i in 1:3) total <- do.call(`+`, list(total, i)); total })"#,
    r#"log_("function value of if#L#", (function(flag) if (flag) "yes")(FALSE) |> is.null())"#,
    r#"log_("braces#L#", list({}, { 1; 2 }, (function() {})(), is.null({ NULL })))"#,
    r#"log_("growing in loop#L#", { v <- NULL; for (i in 1:#I#) { v[i] <- i * 2; v[[i]] <- v[[i]] + 1 }; l <- list(); for (i in 1:3) l[[paste0("k", i)]] <- i; list(v, l) })"#,
    r#"log_("sequence edges#L#", list(1:0, seq_len(0), 5:5, t_(1:NA), -1:1, 2.5:4, seq(2, 10, by = 3), seq(1, 0, length.out = 3), rev(integer(0)), 0:-2))"#,
];

/// A statement that calls one text function on strings that hold non-ASCII, combining and
/// odd characters.
fn regex_statement(writer: &mut Writer) -> String {
    const PATTERNS: [&str; 30] = [
        r#""a""#,
        r#""[[:alpha:]]+""#,
        r#""\\w+""#,
        r#"".""#,
        r#""""#,
        r#""^""#,
        r#""$""#,
        r#""\\b""#,
        r#""(a)(b)?""#,
        r#""[\u00e4\u00f6\u00fc]""#,
        r#""\\d+""#,
        r#""\\s""#,
        r#""(?i)\u00c9""#,
        r#""[[:space:]]+""#,
        r#""a|""#,
        r#""(.)\\1""#,
        r#""[^a-z]""#,
        r#""\\p{L}+""#,
        r#""x*""#,
        r#""\u00e9""#,
        r#""\\.""#,
        r#""[""#,
        r#""(""#,
        r#""[[:upper:]]""#,
        r#""^.{2}""#,
        r#""\u65e5.""#,
        r#""e\u0301""#,
        r#""[[:punct:]]""#,
        r#""\\S+$""#,
        r#""(?<=a).""#,
    ];
    const SUBJECTS: [&str; 16] = [
        r#""stra\u00dfe \u00c4rger""#,
        r#""\u65e5\u672c\u8a9e text""#,
        r#""e\u0301 caf\u00e9""#,
        r#""a\tb\nc""#,
        "NA",
        r#""""#,
        r#""aaa""#,
        r#""A.b.C""#,
        r#""\U0001F600 smile""#,
        r#""abab xx""#,
        r#""  padded  ""#,
        r#""\u00c9cole \u00e9t\u00e9""#,
        r#""1,234.5""#,
        r#""iconv("caf\u00e9", "UTF-8", "latin1")"#,
        r#""\u0131\u0130 \u01c5""#,
        r#""a\u00a0b""#,
    ];
    const OPTIONS: [&str; 8] = [
        "",
        "",
        ", perl = TRUE",
        ", fixed = TRUE",
        ", ignore.case = TRUE",
        ", useBytes = TRUE",
        ", perl = TRUE, ignore.case = TRUE",
        ", fixed = TRUE, useBytes = TRUE",
    ];
    const REPLACEMENTS: [&str; 8] = [
        r#""X""#,
        r#""\\1""#,
        r#""[\\0]""#,
        r#""""#,
        r#""\\U\\1""#,
        r#""\u00df""#,
        r#""\\\\""#,
        r#""<\\1\\2>""#,
    ];
    let mut subjects: Vec<String> = Vec::new();
    for _ in 0..1 + writer.zoo.rng.below(3) {
        subjects.push(if writer.zoo.rng.chance(1, 4) {
            writer.zoo.string().to_owned()
        } else {
            // The latin1 subject is a call, kept whole by taking the literal's inside.
            let subject = SUBJECTS[writer.zoo.rng.below(SUBJECTS.len())];
            match subject.strip_prefix("\"iconv(") {
                Some(_) => subject[1..].to_owned(),
                None => subject.to_owned(),
            }
        });
    }
    let subject = format!("c({})", subjects.join(", "));
    let pattern = PATTERNS[writer.zoo.rng.below(PATTERNS.len())];
    let options = OPTIONS[writer.zoo.rng.below(OPTIONS.len())];
    let replacement = REPLACEMENTS[writer.zoo.rng.below(REPLACEMENTS.len())];
    let count = 1 + writer.zoo.rng.below(4);
    let call = match writer.zoo.rng.below(40) {
        0 | 1 => format!("grepl({pattern}, {subject}{options})"),
        2 | 3 => format!("sub({pattern}, {replacement}, {subject}{options})"),
        4..=6 => format!("gsub({pattern}, {replacement}, {subject}{options})"),
        7 | 8 => format!("regexpr({pattern}, {subject}{options})"),
        9 | 10 => format!("gregexpr({pattern}, {subject}{options})"),
        11 => format!("regmatches({subject}, regexpr({pattern}, {subject}{options}))"),
        12 => format!("regmatches({subject}, gregexpr({pattern}, {subject}{options}))"),
        // GNU R's default engine splits an ASCII string at `\b` into empty strings.
        13 | 14 if pattern == r#""\\b""# => format!("strsplit({subject}, \"\\\\s\"{options})"),
        13 | 14 => format!("strsplit({subject}, {pattern}{options})"),
        15 => format!("grep({pattern}, {subject}, value = TRUE{options})"),
        16 => format!("regexec({pattern}, {subject}{options})"),
        17 => format!(
            "list(nchar({subject}, \"chars\"), nchar({subject}, \"bytes\"), nchar({subject}, \"width\"))"
        ),
        18 => format!("substr({subject}, {count}, {})", count + 2),
        19 => format!("substring({subject}, 1:3, 3:5)"),
        20 => format!(
            "list(toupper({subject}), tolower({subject}), casefold({subject}, upper = TRUE))"
        ),
        21 => format!("chartr(\"a\\u00e4e\", \"b\\u00f6\\u00e9\", {subject})"),
        22 => format!("sprintf(\"%5s|%-5s|%s\", {subject}, {subject}, nchar({subject}))"),
        23 => format!(
            "list(formatC({subject}, width = 6), formatC({subject}, width = -6), formatC({subject}, flag = \"0\", width = 8))"
        ),
        24 => format!(
            "list(format({subject}, width = 6), format({subject}, justify = \"right\"), format({subject}, justify = \"centre\", width = 9))"
        ),
        25 => format!(
            "list(strtrim({subject}, {count}), trimws({subject}), trimws({subject}, \"left\"))"
        ),
        26 => format!(
            "lapply({subject}, function(s) if (is.na(s)) NA else intToUtf8(rev(utf8ToInt(s))))"
        ),
        27 => format!(
            "list(iconv({subject}, \"UTF-8\", \"latin1\"), iconv({subject}, \"UTF-8\", \"ASCII\", sub = \"?\"), iconv({subject}, \"UTF-8\", \"ASCII//TRANSLIT\"), Encoding(enc2native({subject})))"
        ),
        28 => format!(
            "list(shQuote({subject}), dQuote({subject}, FALSE), sQuote({subject}, FALSE), noquote({subject}))"
        ),
        29 => format!(
            "list(encodeString({subject}), encodeString({subject}, quote = '\"', width = 8, justify = \"centre\"), encodeString({subject}, na.encode = FALSE))"
        ),
        30 => format!(
            "list(make.names({subject}), make.unique(c({subject}, {subject})), abbreviate({subject}, {count}))"
        ),
        31 => format!(
            "lapply({subject}, function(s) if (is.na(s)) NA else rawToChar(rev(charToRaw(s))))"
        ),
        32 => format!(
            "list(startsWith({subject}, \"a\"), endsWith({subject}, \"e\"), validUTF8({subject}), utf8ToInt({subject}[1]))"
        ),
        33 => format!("regexpr({pattern}, {subject}, fixed = TRUE, useBytes = TRUE)"),
        34 => format!(
            "list(strsplit({subject}, \"\"), strsplit({subject}, NULL), strsplit({subject}, \" \", fixed = TRUE))"
        ),
        35 => format!(
            "list(sprintf(\"%2$s-%1$s\", {subject}, \"k\"), sprintf(\"%*d\", {count}, 7L), sprintf(\"%%|%5.1f|%-8s|%+d\", 2.25, {subject}, 3L))"
        ),
        36 => format!(
            "list(rev(strtoi({subject}, 16L)), agrepl(\"lasy\", {subject}), basename({subject}), tolower(LETTERS[{count}]))"
        ),
        37 => format!(
            "list(sort({subject}), order({subject}), rank({subject}), unique({subject}), duplicated({subject}), rev({subject}) < {subject})"
        ),
        38 => format!(
            "list(paste({subject}, collapse = \"+\"), paste0({subject}, 1:2), toString({subject}), strrep({subject}, {count}), rev({subject}))"
        ),
        _ => format!(
            "list(regexpr({pattern}, {subject}, perl = TRUE), gregexpr({pattern}, {subject}, perl = TRUE)[[1]])"
        ),
    };
    writer.labels += 1;
    format!("log_(\"text {}\", t_({call}))", writer.labels)
}

/// The text of an R expression of at most `depth` levels, built to parse whatever its parts
/// are: a part that needs them gets parentheses in the template. `loops` allows `while` and
/// `repeat`, which an expression that is evaluated must not hold.
fn quoted(writer: &mut Writer, depth: usize, loops: bool) -> String {
    const LEAVES: [&str; 32] = [
        "a",
        "b",
        "x1",
        "`my var`",
        "`if`",
        "1",
        "2L",
        "1.5",
        "1e5",
        "1e-15",
        "0x10L",
        "100000",
        "123456789012",
        "0.1",
        "TRUE",
        "NULL",
        "NA",
        "NA_integer_",
        "Inf",
        "-1",
        "\"s\"",
        "\"a\\tb\"",
        "\"\\u00e4\"",
        "'single'",
        "1i",
        "..1",
        ".x",
        "T",
        "f()",
        "pkg::name",
        "break",
        "next",
    ];
    const NODES: [&str; 78] = [
        "A + B",
        "A - B",
        "A * B",
        "A / B",
        "A ^ B",
        "-A",
        "+A",
        "!A",
        "(A)",
        "A %in% B",
        "A %% B",
        "A %/% B",
        "A %o% B",
        "A:B",
        "A < B",
        "A == B",
        "A != B",
        "A >= B",
        "A && B",
        "A || B",
        "A & B",
        "A | B",
        "T <- B",
        "T <<- B",
        "(T = B)",
        "A -> T",
        "A ~ B",
        "~A",
        "A$b",
        "A@b",
        "A[B]",
        "A[[B]]",
        "A[B, C]",
        "A[, B]",
        "A[B, , drop = FALSE]",
        "f(A)",
        "f(A, k = B)",
        "(A)(B)",
        "f(A)(B)",
        "if (A) B",
        "if (A) B else C",
        "for (i in A) B",
        "{ A; B }",
        "{}",
        "function(p, q = A) B",
        "function(...) A",
        "\\(p) A",
        "A |> f()",
        "A |> f(k = B)",
        "-A^B",
        "(-A)^B",
        "A^-B",
        "!A == B",
        "A$`b c`",
        "\"f\"(A)",
        "`[`(A, B)",
        "`+`(A, B)",
        "`+`(A)",
        "`if`(A, B)",
        "`{`(A)",
        "`(`(A)",
        "`-`(A, B, C)",
        "`<-`(T, B)",
        "`$`(A, \"b\")",
        "return(A)",
        "A := B",
        "if (A) B else if (C) A else B",
        "A[[B]][[C]]",
        "A$b$c",
        "(T <- B) + C",
        "A + (B + C)",
        "A - (B - C)",
        "A^B^C",
        "(A^B)^C",
        "A / (B * C)",
        "- -A",
        "(function(p) p)(A)",
        "if (A) (B) else {C}",
    ];
    const LOOPS: [&str; 3] = ["while (A) B", "repeat A", "while (TRUE) break"];
    if depth == 0 || writer.zoo.rng.chance(1, 4) {
        let leaf = LEAVES[writer.zoo.rng.below(LEAVES.len())];
        // `break` and `next` outside a loop are an error only when evaluated, but an
        // evaluated expression must not leave the loop the statement sits in.
        return if !loops && matches!(leaf, "break" | "next") {
            "a".to_owned()
        } else {
            leaf.to_owned()
        };
    }
    let node = if loops && writer.zoo.rng.chance(1, 20) {
        LOOPS[writer.zoo.rng.below(LOOPS.len())]
    } else {
        NODES[writer.zoo.rng.below(NODES.len())]
    };
    // A part is an `A`, `B` or `C` that stands alone, which the `A` of `FALSE` does not.
    let word = |character: Option<char>| character.is_some_and(char::is_alphanumeric);
    let mut text = String::new();
    let mut previous = None;
    let mut characters = node.chars().peekable();
    while let Some(character) = characters.next() {
        let alone = !word(previous) && !word(characters.peek().copied());
        if alone && matches!(character, 'A'..='C') {
            text.push_str(&quoted(writer, depth - 1, loops));
        } else if alone && character == 'T' {
            text.push_str(&assignment_target(writer, depth - 1, loops));
        } else {
            text.push(character);
        }
        previous = Some(character);
    }
    text
}

/// The left side of a quoted assignment: a name, or calls whose first arguments end at one.
/// GNU R's `cmpfun` stops at any other target ("bad assignment"), inside `quote()` too.
fn assignment_target(writer: &mut Writer, depth: usize, loops: bool) -> String {
    const NAMES: [&str; 5] = ["a", "b", "x1", "`my var`", ".x"];
    let name = NAMES[writer.zoo.rng.below(NAMES.len())];
    match writer.zoo.rng.below(9) {
        0..=3 => name.to_owned(),
        4 => format!("{name}[{}]", quoted(writer, depth, loops)),
        5 => format!("{name}$b"),
        6 => format!("names({name})"),
        7 => format!("f({name})[[{}]]", quoted(writer, depth, loops)),
        _ => "\"s\"".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_deterministic_and_parse() {
        for seed in 0..400 {
            let text = program(seed);
            assert!(
                syntax::parse(&text).errors().is_empty(),
                "seed {seed} does not parse:\n{text}"
            );
            assert_eq!(text, program(seed));
        }
    }

    #[test]
    fn every_construct_is_listed_once_in_declaration_order() {
        for (index, construct) in CONSTRUCTS.iter().enumerate() {
            assert_eq!(*construct as usize, index);
        }
    }

    #[test]
    fn every_hole_is_filled() {
        for seed in 0..200 {
            let text = program(seed);
            for hole in [
                "#E#", "#L#", "#I#", "#V#", "#e#", "#S#", "#N#", "#Q#", "#q#", "#X#", "#BODY#",
            ] {
                assert!(!text.contains(hole), "seed {seed} keeps {hole}:\n{text}");
            }
        }
    }
}
