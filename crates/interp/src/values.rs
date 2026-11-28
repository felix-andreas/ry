//! Constructing and reading R values. Every constructor here roots what it allocates in the
//! current handle scope (see the crate documentation), so callers never protect by hand.

use {
    crate::{
        handles::{self, Alt, Cx, Gc, Local, Owned},
        interp, records, standard_library,
        unwind::{R, error},
    },
    heap::{
        Heap, Marker,
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp, is_na_real},
        records::NativeKind,
        strings::Encoding,
    },
    memchr::memchr,
    rustc_hash::FxHashMap,
    std::borrow::Cow,
};

macro_rules! symbol_table {
    ($($field:ident = $name:literal),* $(,)?) => {
        /// Symbols the interpreter compares against or looks up by, installed once.
        pub struct Symbols {
            $(pub $field: Sexp,)*
            /// `typeof`'s answer for each type code, as the interned string: the print name of
            /// the symbol of that name. `NULL` for a code that names no type.
            pub type_names: [Sexp; 32],
        }

        impl Symbols {
            pub(crate) unsafe fn install(heap: &mut Heap) -> Symbols {
                let mut type_names = [heap.nil(); 32];
                for (code, slot) in type_names.iter_mut().enumerate() {
                    if let Some(kind) = SexpType::from_code(code as u32) {
                        *slot = heap
                            .install(kind.name().as_bytes())
                            .unwrap_or_else(|error| panic!("cannot install `{}`: {error}", kind.name()))
                            .car();
                    }
                }
                Symbols {
                    $($field: heap
                        .install($name.as_bytes())
                        .unwrap_or_else(|error| panic!("cannot install `{}`: {error}", $name)),)*
                    type_names,
                }
            }
        }
    };
}

symbol_table! {
    contains = "contains",
    subclasses = "subclasses",
    slots = "slots",
    virtual_ = "virtual",
    distance = "distance",
    prototype = "prototype",
    sub_class = "subClass",
    test = "test",
    signature = "signature",
    data_part = "dataPart",
    validity = "validity",
    simple = "simple",
    replace = "replace",
    coerce = "coerce",
    class_name = "className",
    by = "by",
    super_class = "superClass",
    sealed = "sealed",
    group = "group",
    default = "default",
    defined = "defined",
    target = "target",
    mtable = ".MTable",
    all_mtable = ".AllMTable",
    sig_length = ".SigLength",
    sig_args = ".SigArgs",
    class_table = ".classTable",
    get_data_part = "getDataPart",
    check_at_assignment = "checkAtAssignment",
    set_data_part = "setDataPart",
    brace = "{",
    paren = "(",
    if_ = "if",
    for_ = "for",
    while_ = "while",
    repeat = "repeat",
    break_ = "break",
    next = "next",
    function = "function",
    return_ = "return",
    ry_match = ".ry_match",
    assign = "<-",
    assign2 = "<<-",
    assign_eq = "=",
    dots = "...",
    names = "names",
    dim = "dim",
    dimnames = "dimnames",
    class = "class",
    levels = "levels",
    tsp = "tsp",
    row_names = "row.names",
    comment = "comment",
    srcref = "srcref",
    srcfile = "srcfile",
    whole_srcref = "wholeSrcref",
    tmp = "*tmp*",
    vtmp = "*vtmp*",
    value = "value",
    dot_generic = ".Generic",
    standard_generic = "standardGeneric",
    dot_class = ".Class",
    dot_method = ".Method",
    dot_group = ".Group",
    dot_generic_call_env = ".GenericCallEnv",
    dot_generic_def_env = ".GenericDefEnv",
    dot_s3_methods_table = ".__S3MethodsTable__.",
    dot_options = ".Options",
    dot_random_seed = ".Random.seed",
    dot_last_value = ".Last.value",
    dot_package_name = ".packageName",
    dot_namespace = ".__NAMESPACE__.",
    package_arg = "PACKAGE",
    address = "address",
    num_parameters = "numParameters",
    native_symbol_info = "NativeSymbolInfo",
    na_ok = "NAOK",
    dup = "DUP",
    encoding = "ENCODING",
    name = "name",
    spec = "spec",
    dot_environment = ".Environment",
    dot_ry_loader = ".__ry_loader__.",
    bracket = "[",
    bracket2 = "[[",
    dollar = "$",
    dollar_assign = "$<-",
    seq = "seq",
    at = "@",
    at_assign = "@<-",
    double_colon = "::",
    triple_colon = ":::",
    tilde = "~",
    quote = "quote",
    drop = "drop",
    exact = "exact",
    x = "x",
    unset = "unset",
    object = "object",
    package = "package",
    dot_data = ".Data",
    dot_xdata = ".xData",
    dot_s3_class = ".S3Class",
    generic = "generic",
    dot_defined = ".defined",
    dot_target = ".target",
    missing = "missing",
    dot_method_symbol = ".Methods",
    dot_methods_dispatch = ".methodsDispatch",
    plus = "+",
    minus = "-",
    not = "!",
    and2 = "&&",
    or2 = "||",
    colon = ":",
    mode = "mode",
    use_names = "use.names",
    recursive = "recursive",
    na_rm = "na.rm",
    previous = "previous",
    call = "call",
    message = "message",
    dot_device = ".Device",
    dot_internal = ".Internal",
    list = "list",
    dot_primitive = ".Primitive",
    from = "from",
    ry_s4_initialize = "ryS4Initialize",
    access = "access",
    version = "version",
    s3_methods = "S3methods",
    representation = "representation",
    base = "base",
    exports = "exports",
    lazydata = "lazydata",
    load_namespace = "loadNamespace",
    arg = "arg",
    choices = "choices",
    several_ok = "several.ok",
    domain = "domain",
    append_lf = "appendLF",
    call_dot = "call.",
    immediate_dot = "immediate.",
    cond = "cond",
    msg = "msg",
    condition_message_simple_message = "conditionMessage.simpleMessage",
    condition_message_message = "conditionMessage.message",
    exprs = "exprs",
    expr_object = "exprObject",
    init = "init",
    f = "f",
    y = "y",
    i = "i",
    mis = "mis",
    yes = "yes",
    no = "no",
    obj = "obj",
    force = "force",
    table = "table",
    len = "len",
    c = "c",
    upper_x = "X",
    fun = "FUN",
    right = "right",
    accumulate = "accumulate",
    justify = "justify",
    big_mark = "big.mark",
    big_interval = "big.interval",
    small_mark = "small.mark",
    small_interval = "small.interval",
    zero_print = "zero.print",
    drop0trailing = "drop0trailing",
    trim = "trim",
    digits = "digits",
    nsmall = "nsmall",
    width = "width",
    na_encode = "na.encode",
    scientific = "scientific",
    decimal_mark = "decimal.mark",
    out_dec = "OutDec",
    scipen = "scipen",
    expr = "expr",
    width_cutoff = "width.cutoff",
    backtick = "backtick",
    control = "control",
    nlines = "nlines",
    format = "format",
    try_formats = "tryFormats",
    flag = "flag",
    preserve_width = "preserve.width",
    deparse_options = "..deparseOpts",
    top = "top",
    bottom = "bottom",
    envs = "envs",
    action = "action",
    new_options = "new_options",
    old_options = "old_options",
    lhs = "lhs",
    rhs = "rhs",
    handler = "handler",
    handlers = "handlers",
    classes = "classes",
    parentenv = "parentenv",
    restart = "restart",
    restarts = "restarts",
    exit = "exit",
    finally = "finally",
    description = "description",
    interactive = "interactive",
    nh = "nh",
    r = "r",
    restart_args = "restartArgs",
    inherits = "inherits",
    try_invoke_restart = "tryInvokeRestart",
    w = "w",
    decreasing = "decreasing",
    na_last = "na.last",
    method = "method",
    partial = "partial",
    index_return = "index.return",
}

/// Session-wide objects the builtins keep. Each is rooted for the life of the session.
pub struct Globals {
    /// `R_MethodsNamespace`, once `methods` is loaded.
    pub methods_namespace: Sexp,
    /// The expression templates the native class definitions fill in, as the methods package's
    /// R code fills them in with `substitute()`.
    pub methods_templates: Sexp,
    /// The methods package's `checkAtAssignment` as the first slot assignment found it, which
    /// every later one calls, as in GNU R (`subset::slot_check`); nil before.
    pub check_at_assignment: Sexp,
    /// The `checkAtAssignment` that `subset::slot_accepts` answers for, which
    /// `methods_core::install_fast_paths` knows by its hash; nil when the package has another.
    pub answered_slot_check: Sexp,
    /// Whether the methods package turned S4 dispatch on, which makes `standardGeneric` and
    /// primitive generics dispatch.
    pub methods_dispatch: bool,
    /// Whether primitives may dispatch to S4 methods at all. The methods package turns this off
    /// while it selects an inherited method, so that selection itself never dispatches.
    pub allow_primitive_methods: bool,
    /// Primitives the methods package turned generic, by primitive index.
    pub primitive_methods: FxHashMap<u32, crate::dispatch::PrimitiveMethods>,
    /// Environments `attach` put on the search path are ordinary enclosures of the global
    /// environment; this keeps `Autoloads` and the like alive between `attach` and first use.
    pub keep: Vec<Sexp>,
    /// `.cache_class`: each S4 class's S3 class vector (the class and what it extends), which
    /// the methods package keeps current as it defines and removes classes.
    pub s4_extends: FxHashMap<Box<[u8]>, Sexp>,
    /// The implicit class vectors S3 dispatch builds for unclassed objects, by dimension shape
    /// and type, so that dispatching on a plain vector allocates no class vector.
    pub implicit_classes: FxHashMap<(u8, SexpType), Sexp>,
    /// S4 definitions a package loader holds back until a lookup needs them.
    pub deferred_definitions: Vec<crate::methods::DeferredDefinitions>,
    /// The environment the host's sessions attach beside base R, as a start-up image carries it
    /// ([`crate::image::ImageHost::environment`]); nil when the session started without one.
    pub host_environment: Sexp,
    /// The call `FUN(X[[i]], ...)` that `lapply` and `vapply` run for each element, built by the
    /// first of them ([`crate::envir::element_call`]); nil before.
    pub element_call: Sexp,
}

impl Globals {
    pub(crate) fn placeholder() -> Globals {
        Globals {
            methods_namespace: Sexp::dangling(),
            methods_templates: Sexp::dangling(),
            check_at_assignment: Sexp::dangling(),
            answered_slot_check: Sexp::dangling(),
            methods_dispatch: false,
            allow_primitive_methods: true,
            primitive_methods: rustc_hash::FxHashMap::default(),
            keep: Vec::new(),
            s4_extends: rustc_hash::FxHashMap::default(),
            implicit_classes: rustc_hash::FxHashMap::default(),
            deferred_definitions: Vec::new(),
            host_environment: Sexp::dangling(),
            element_call: Sexp::dangling(),
        }
    }

    pub(crate) unsafe fn install() {
        let nil = crate::heap().nil();
        let globals = &mut interp().globals;
        globals.methods_namespace = nil;
        globals.methods_templates = nil;
        globals.check_at_assignment = nil;
        globals.answered_slot_check = nil;
        globals.host_environment = nil;
        globals.element_call = nil;
    }

    pub(crate) unsafe fn trace(&self, marker: &mut Marker<'_>) {
        marker.mark(self.methods_namespace);
        marker.mark(self.methods_templates);
        marker.mark(self.check_at_assignment);
        marker.mark(self.answered_slot_check);
        marker.mark(self.host_environment);
        marker.mark(self.element_call);
        for methods in self.primitive_methods.values() {
            marker.mark(methods.generic);
            marker.mark(methods.methods);
        }
        for &object in &self.keep {
            marker.mark(object);
        }
        for &extends in self.s4_extends.values() {
            marker.mark(extends);
        }
        for &classes in self.implicit_classes.values() {
            marker.mark(classes);
        }
    }
}

#[inline]
pub fn symbols() -> &'static Symbols {
    &interp().symbols
}

// Well-known objects.

#[inline]
pub fn nil() -> Sexp {
    crate::heap().known().nil
}

#[inline]
pub fn unbound() -> Sexp {
    crate::heap().known().unbound
}

#[inline]
pub fn missing_arg() -> Sexp {
    crate::heap().known().missing_arg
}

#[inline]
pub fn na_string() -> Sexp {
    crate::heap().known().na_string
}

#[inline]
pub fn blank_string() -> Sexp {
    crate::heap().known().blank_string
}

#[inline]
pub fn global_env() -> Sexp {
    crate::heap().known().global_env
}

#[inline]
pub fn base_env() -> Sexp {
    crate::heap().known().base_env
}

#[inline]
pub fn base_namespace() -> Sexp {
    crate::heap().known().base_namespace
}

#[inline]
pub fn empty_env() -> Sexp {
    crate::heap().known().empty_env
}

#[inline]
pub unsafe fn is_nil(object: Sexp) -> bool {
    object == nil()
}

// Allocation. Every constructor roots its result.

// Out of line with the heap's inline path in its body, so a caller pays one call for a rooted
// vector.
#[inline(never)]
pub unsafe fn alloc(kind: SexpType, length: usize) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.alloc_vector_inline(kind, length as isize)?;
    heap.root(object);
    Ok(object)
}

/// [`alloc`] without zeroing a numeric vector's elements; the caller writes all of them.
pub unsafe fn alloc_uninit(kind: SexpType, length: usize) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.alloc_vector_uninit(kind, length)?;
    heap.root(object);
    Ok(object)
}

pub unsafe fn cons(car: Sexp, cdr: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.cons(car, cdr)?;
    heap.root(object);
    Ok(object)
}

pub unsafe fn cons_tagged(car: Sexp, cdr: Sexp, tag: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.alloc_triple(SexpType::Pairlist, car, cdr, tag)?;
    heap.root(object);
    Ok(object)
}

pub unsafe fn lcons(car: Sexp, cdr: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.alloc_triple(SexpType::Language, car, cdr, nil())?;
    heap.root(object);
    Ok(object)
}

pub unsafe fn triple(kind: SexpType, first: Sexp, second: Sexp, third: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.alloc_triple(kind, first, second, third)?;
    heap.root(object);
    Ok(object)
}

pub unsafe fn new_env(enclosure: Sexp) -> R<Sexp> {
    let heap = crate::heap();
    let env = heap.alloc_environment(enclosure)?;
    heap.root(env);
    Ok(env)
}

pub unsafe fn mkchar(text: &str) -> R<Sexp> {
    match unescape_invalid_bytes(text) {
        Some(bytes) => mkchar_bytes(&bytes, Encoding::Native),
        None => mkchar_bytes(text.as_bytes(), Encoding::Utf8),
    }
}

pub unsafe fn mkchar_bytes(bytes: &[u8], encoding: Encoding) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.mk_char(bytes, encoding)?;
    heap.root(object);
    Ok(object)
}

/// Interns `text` as [`mkchar`] does and stores it as element `index` of the string vector
/// `vector`, which the caller keeps rooted.
pub unsafe fn set_mkchar(vector: Sexp, index: usize, text: &str) -> R<()> {
    match unescape_invalid_bytes(text) {
        Some(bytes) => set_mkchar_bytes(vector, index, &bytes, Encoding::Native),
        None => set_mkchar_bytes(vector, index, text.as_bytes(), Encoding::Utf8),
    }
}

/// Interns `bytes` straight into element `index` of the string vector `vector`, which the caller
/// keeps rooted. A string stored at once needs no root handle of its own, and a handle per element
/// would make a long result hold a second vector's worth of handles until the builtin returns.
pub unsafe fn set_mkchar_bytes(
    vector: Sexp,
    index: usize,
    bytes: &[u8],
    encoding: Encoding,
) -> R<()> {
    let heap = crate::heap();
    let charsxp = heap.mk_char(bytes, encoding)?;
    heap.set_element(vector, index, charsxp);
    Ok(())
}

/// Interns the native strings one vectorized call produces, through a table of the call's own
/// results in front of the heap's string table. The heap's table holds every string of the
/// session, so at millions of strings even one probe into it misses cache, while a call that extracts
/// repeating pieces (a code inside ids, a year inside dates) has few distinct results whose table
/// stays in cache. A call whose results rarely repeat would only pay for the inserts, so the memo
/// switches itself off after a window with few hits.
#[derive(Default)]
pub struct StringMemo<'a> {
    pieces: FxHashMap<&'a [u8], Sexp>,
    lookups: usize,
    hits: usize,
    off: bool,
}

impl StringMemo<'_> {
    /// The vector the strings are stored into must keep them alive, since the memo does not
    /// protect them; the collector never moves a string, so the borrowed bytes stay valid.
    pub unsafe fn mk_char(&mut self, bytes: &[u8]) -> R<Sexp> {
        const WINDOW: usize = 1 << 16;
        if self.off {
            return Ok(crate::heap().mk_char(bytes, Encoding::Native)?);
        }
        // A call that makes only a few strings would spend more on building the table than
        // the table saves.
        const SHORT: usize = 16;
        self.lookups += 1;
        if self.lookups <= SHORT && self.pieces.is_empty() {
            return Ok(crate::heap().mk_char(bytes, Encoding::Native)?);
        }
        if self.lookups == WINDOW {
            if self.hits < WINDOW / 8 {
                self.off = true;
                self.pieces = FxHashMap::default();
            }
            self.lookups = 0;
            self.hits = 0;
        }
        if let Some(&found) = self.pieces.get(bytes) {
            self.hits += 1;
            return Ok(found);
        }
        let made = crate::heap().mk_char(bytes, Encoding::Native)?;
        if !self.off {
            self.pieces.insert(made.bytes(), made);
        }
        Ok(made)
    }
}

/// The symbol named `name`, whose stand-ins (see [`escape_invalid_bytes`]) name the bytes they
/// stand for, as [`mkchar`] interns them.
pub unsafe fn install(name: &str) -> R<Sexp> {
    Ok(match unescape_invalid_bytes(name) {
        Some(bytes) => crate::heap().install(&bytes)?,
        None => crate::heap().install(name.as_bytes())?,
    })
}

pub unsafe fn install_bytes(name: &[u8]) -> R<Sexp> {
    Ok(crate::heap().install(name)?)
}

/// The symbol named by the `CHARSXP` `name`. An interned ASCII or UTF-8 string is a print name
/// as it is: an ASCII one is what `install` would intern, and a UTF-8 one keeps its mark when
/// it installs the symbol, as GNU R's `installTrChar` does, which also translates a latin1 string
/// to its UTF-8 text first. Any other string goes through `install`, which also rejects an empty
/// or overlong name. `NA_STRING` is not the interned `"NA"`, so it goes through `install` too.
pub unsafe fn install_charsxp(name: Sexp) -> R<Sexp> {
    if name != na_string()
        && name.has_gp(gp::CACHED)
        && name.length() > 0
        && (name.has_gp(gp::ASCII) || (name.has_gp(gp::UTF8) && name.is_valid_utf8()))
    {
        return Ok(crate::heap().install_char(name)?);
    }
    if name.has_gp(gp::LATIN1) {
        return install(&char_str(name));
    }
    install_bytes(name.bytes())
}

/// The symbol whose name is the text of the `CHARSXP` `name`, as GNU R's `installTrChar`
/// gives: a string in another encoding installs under its UTF-8 text, and `NA_STRING` installs
/// as `NA`, as GNU R's `installTrChar` does.
pub unsafe fn install_charsxp_text(name: Sexp) -> R<Sexp> {
    if name == na_string() {
        return install("NA");
    }
    if name.has_gp(gp::ASCII) {
        return install_charsxp(name);
    }
    install(&char_str(name))
}

pub unsafe fn scalar_logical(value: i32) -> R<Sexp> {
    let known = crate::heap().known();
    Ok(match value {
        0 => known.false_value,
        NA_LOGICAL => known.logical_na_value,
        _ => known.true_value,
    })
}

pub unsafe fn fresh_logical(value: i32) -> R<Sexp> {
    let object = alloc(SexpType::Logical, 1)?;
    object.slice_mut::<i32>()[0] = value;
    Ok(object)
}

pub unsafe fn scalar_bool(value: bool) -> R<Sexp> {
    scalar_logical(value as i32)
}

pub unsafe fn scalar_integer(value: i32) -> R<Sexp> {
    scalar(SexpType::Integer, [value as u32 as u64, 0])
}

pub unsafe fn scalar_real(value: f64) -> R<Sexp> {
    scalar(SexpType::Real, [value.to_bits(), 0])
}

pub unsafe fn scalar_complex(value: Rcomplex) -> R<Sexp> {
    scalar(SexpType::Complex, [value.r.to_bits(), value.i.to_bits()])
}

pub unsafe fn scalar_raw(value: u8) -> R<Sexp> {
    scalar(SexpType::Raw, [value as u64, 0])
}

#[inline]
unsafe fn scalar(kind: SexpType, payload: [u64; 2]) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.alloc_scalar(kind, payload)?;
    heap.root(object);
    Ok(object)
}

pub unsafe fn scalar_string(text: &str) -> R<Sexp> {
    let charsxp = mkchar(text)?;
    scalar_string_charsxp(charsxp)
}

pub unsafe fn scalar_string_charsxp(charsxp: Sexp) -> R<Sexp> {
    let object = alloc(SexpType::String, 1)?;
    crate::heap().set_element(object, 0, charsxp);
    Ok(object)
}

pub unsafe fn string_vector<S: AsRef<str>>(items: &[S]) -> R<Sexp> {
    let object = alloc(SexpType::String, items.len())?;
    for (index, item) in items.iter().enumerate() {
        let charsxp = mkchar(item.as_ref())?;
        crate::heap().set_element(object, index, charsxp);
    }
    Ok(object)
}

/// A character vector where `None` is `NA`.
pub unsafe fn string_vector_na(items: &[Option<String>]) -> R<Sexp> {
    let object = alloc(SexpType::String, items.len())?;
    for (index, item) in items.iter().enumerate() {
        let charsxp = match item {
            Some(text) => mkchar(text)?,
            None => na_string(),
        };
        crate::heap().set_element(object, index, charsxp);
    }
    Ok(object)
}

pub unsafe fn integer_vector(items: &[i32]) -> R<Sexp> {
    let object = alloc(SexpType::Integer, items.len())?;
    object.slice_mut::<i32>().copy_from_slice(items);
    Ok(object)
}

pub unsafe fn real_vector(items: &[f64]) -> R<Sexp> {
    let object = alloc(SexpType::Real, items.len())?;
    object.slice_mut::<f64>().copy_from_slice(items);
    Ok(object)
}

pub unsafe fn logical_vector(items: &[i32]) -> R<Sexp> {
    let object = alloc(SexpType::Logical, items.len())?;
    object.slice_mut::<i32>().copy_from_slice(items);
    Ok(object)
}

pub unsafe fn list_vector(items: &[Sexp]) -> R<Sexp> {
    let object = alloc(SexpType::List, items.len())?;
    for (index, &item) in items.iter().enumerate() {
        crate::heap().set_element(object, index, item);
    }
    Ok(object)
}

/// A named list.
pub unsafe fn named_list(items: &[(&str, Sexp)]) -> R<Sexp> {
    let values: Vec<Sexp> = items.iter().map(|(_, value)| *value).collect();
    let list = list_vector(&values)?;
    let names: Vec<&str> = items.iter().map(|(name, _)| *name).collect();
    let names = string_vector(&names)?;
    set_attr(list, symbols().names, names)?;
    Ok(list)
}

/// A pairlist (or call, with `kind` `Language`) of `items`, each with an optional tag.
pub unsafe fn pairlist(items: &[(Sexp, Sexp)], kind: SexpType) -> R<Sexp> {
    let mut list = nil();
    for (position, &(tag, value)) in items.iter().enumerate().rev() {
        let cell_kind = if position == 0 {
            kind
        } else {
            SexpType::Pairlist
        };
        list = triple(cell_kind, value, list, tag)?;
    }
    Ok(list)
}

/// Builds a pairlist (or call) front to back, so the items need not be collected first.
pub struct PairlistBuilder {
    kind: SexpType,
    head: Sexp,
    last: Sexp,
}

impl PairlistBuilder {
    pub fn new(kind: SexpType) -> Self {
        Self {
            kind,
            head: nil(),
            last: nil(),
        }
    }

    pub unsafe fn push(&mut self, tag: Sexp, value: Sexp) -> R<()> {
        if self.head == nil() {
            self.head = triple(self.kind, value, nil(), tag)?;
            self.last = self.head;
            return Ok(());
        }
        // The rooted head reaches every cell once it is linked, and nothing allocates between
        // making a cell and linking it, so only the head takes a root.
        let heap = crate::heap();
        let cell = heap.alloc_triple(SexpType::Pairlist, value, nil(), tag)?;
        heap.set_cdr(self.last, cell);
        self.last = cell;
        Ok(())
    }

    /// The cell pushed last, or `NULL` before the first push.
    pub fn last(&self) -> Sexp {
        self.last
    }

    pub fn finish(self) -> Sexp {
        self.head
    }
}

/// A call `function(arguments...)` with untagged arguments.
pub unsafe fn call(function: Sexp, arguments: &[Sexp]) -> R<Sexp> {
    let mut list = nil();
    for &argument in arguments.iter().rev() {
        list = cons(argument, list)?;
    }
    lcons(function, list)
}

// Reading.

/// R's `length()` for every type, without dispatch.
#[inline]
pub unsafe fn length(object: Sexp) -> usize {
    if object.kind().is_vector() && !object.is_altrep() {
        return object.length() as usize;
    }
    other_length(object)
}

// Out of line, so that `length` of a plain vector inlines.
#[inline(never)]
unsafe fn other_length(object: Sexp) -> usize {
    match object.kind() {
        SexpType::Nil => 0,
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => pairlist_length(object),
        SexpType::Environment => env_length(object),
        SexpType::Record => match records::native_kind(object) {
            NativeKind::Dict | NativeKind::Set => standard_library::entry_count(object),
            NativeKind::Record | NativeKind::Tuple | NativeKind::Tag => object.record_slot_count(),
        },
        _ if object.kind().is_vector() => {
            if object.is_altrep() {
                altrep_length(object)
            } else {
                object.length() as usize
            }
        }
        _ => 1,
    }
}

pub unsafe fn pairlist_length(mut list: Sexp) -> usize {
    let mut count = 0;
    while matches!(
        list.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        count += 1;
        list = list.cdr();
    }
    count
}

pub unsafe fn env_length(env: Sexp) -> usize {
    crate::heap().bound_symbols(env).len()
}

/// Asks the class's `Length` method, without materializing the elements. `length` cannot
/// fail, so an error from the method is parked like an element accessor's
/// ([`crate::unwind::park`]) and the length reads as 0 until it is raised.
unsafe fn altrep_length(object: Sexp) -> usize {
    if let Some((count, _, _)) = compact_sequence(object) {
        return count;
    }
    match class_length(object) {
        Ok(count) => count,
        Err(unwind) => {
            crate::unwind::park(unwind);
            0
        }
    }
}

/// The length an ALTREP vector's class's `Length` method answers.
pub unsafe fn class_length(object: Sexp) -> R<usize> {
    on_altrep(object, |context, object| {
        (interp().hooks.altrep_length)(context, object)
    })
}

/// A compact sequence of an integer or double `kind`, as the hook makes it, rooted in the
/// caller's scope, or `None` when the host has no compact sequences.
pub unsafe fn compact_sequence_of(
    kind: SexpType,
    count: usize,
    first: f64,
    increment: f64,
) -> R<Option<Sexp>> {
    handles::in_caller_scope(|context| {
        let sequence = (interp().hooks.compact_sequence)(context, kind, count, first, increment)?;
        Ok(sequence.map(|sequence| sequence.raw()))
    })
}

/// `as.character` of the integer or double vector `object` as deferred strings, rooted in the
/// caller's scope, or `None` when the host has no such class.
pub unsafe fn deferred_strings(object: Sexp) -> R<Option<Sexp>> {
    handles::in_caller_scope(|context| {
        let object = handles::held(context, object);
        let deferred = (interp().hooks.defer_strings)(context, object)?;
        Ok(deferred.map(|deferred| deferred.raw()))
    })
}

/// An ordinary vector with the contents of `object`, which is `object` itself unless it is
/// ALTREP.
#[inline]
pub unsafe fn materialize(object: Sexp) -> R<Sexp> {
    if !object.is_altrep() {
        return Ok(object);
    }
    altrep_result(object, |context, object| {
        (interp().hooks.materialize_altrep)(context, object)
    })
}

/// Runs `hook` on the ALTREP object `object` in a scope of its own, which roots the object,
/// because a class's method is native code that may allocate.
unsafe fn on_altrep<T>(
    object: Sexp,
    hook: impl for<'s> FnOnce(&mut Cx<'s>, Local<'s, Alt>) -> R<T>,
) -> R<T> {
    handles::with(|context| {
        let object = context.root(Gc::<Alt>::adopt(object));
        hook(context, object)
    })
}

/// [`on_altrep`] for a hook that answers with an object, which is rooted in the caller's scope
/// as a raw allocation's result is. It stays out of line so that the non-ALTREP test of a
/// caller such as [`materialize`] inlines alone.
#[cold]
#[inline(never)]
unsafe fn altrep_result(
    object: Sexp,
    hook: impl for<'s> FnOnce(&mut Cx<'s>, Local<'s, Alt>) -> R<Local<'s>>,
) -> R<Sexp> {
    handles::with_result(|context| {
        let object = context.root(Gc::<Alt>::adopt(object));
        Ok(hook(context, object)?.raw())
    })
}

/// An ordinary vector with the contents of `object` to read, leaving `object` as it is: a compact
/// sequence that is not yet expanded is computed into a fresh vector instead of expanding in place.
/// GNU R prints and formats through element reads, so `x <- 1:10; print(x)` leaves `x` compact
/// there, and `.Internal(inspect(x))` must report the same afterwards.
pub unsafe fn read_contents(object: Sexp) -> R<Sexp> {
    match compact_sequence(object) {
        Some((count, first, increment)) if object.cdr() == nil() => {
            let kind = object.kind();
            let copy = alloc_uninit(kind, count)?;
            if kind == SexpType::Integer {
                for (index, slot) in copy.slice_mut::<i32>().iter_mut().enumerate() {
                    *slot = (first + increment * index as f64) as i32;
                }
            } else {
                for (index, slot) in copy.slice_mut::<f64>().iter_mut().enumerate() {
                    *slot = first + increment * index as f64;
                }
            }
            Ok(copy)
        }
        _ => materialize(object),
    }
}

macro_rules! typed_slice {
    ($name:ident, $name_mut:ident, $altrep:ident, $type:ty) => {
        /// The elements, which must be of the matching type. An ALTREP vector is materialized.
        #[inline(always)]
        pub unsafe fn $name<'a>(object: Sexp) -> &'a [$type] {
            if object.is_altrep() {
                $altrep(object)
            } else {
                object.slice::<$type>()
            }
        }

        // Out of line, so that the plain-vector read every builtin does inlines to a type test
        // and a pointer.
        #[cold]
        #[inline(never)]
        unsafe fn $altrep<'a>(object: Sexp) -> &'a [$type] {
            // A compact sequence keeps its expansion in its second data slot, which reads
            // without a call into the class.
            if compact_sequence(object).is_some() && object.cdr() != nil() {
                return object.cdr().slice::<$type>();
            }
            // The accessor cannot fail, so a method's error is parked, as for elements. The
            // class's own data pointer comes first, since materializing copies the whole vector
            // on every read.
            let data = match on_altrep(object, |context, object| {
                (interp().hooks.altrep_dataptr)(context, object)
            }) {
                Ok(Some(pointer)) if pointer.is_null() => Ok(&[][..]),
                Ok(Some(pointer)) => Ok(std::slice::from_raw_parts(
                    pointer.cast::<$type>(),
                    length(object),
                )),
                Ok(None) => materialize(object).map(|vector| vector.slice::<$type>()),
                Err(unwind) => Err(unwind),
            };
            match data {
                Ok(slice) => slice,
                Err(unwind) => {
                    crate::unwind::park(unwind);
                    &[]
                }
            }
        }

        #[inline]
        #[allow(clippy::mut_from_ref)]
        pub unsafe fn $name_mut<'a>(object: Sexp) -> &'a mut [$type] {
            object.slice_mut::<$type>()
        }
    };
}

typed_slice!(ints, ints_mut, ints_altrep, i32);
typed_slice!(reals, reals_mut, reals_altrep, f64);
typed_slice!(complexes, complexes_mut, complexes_altrep, Rcomplex);
typed_slice!(raws, raws_mut, raws_altrep, u8);

/// Element `index` of a logical or integer vector. An ALTREP vector answers through its class's
/// `Elt` method, so a compact sequence is never expanded to read one element.
#[inline]
pub unsafe fn int_elt(object: Sexp, index: usize) -> R<i32> {
    if object.is_altrep() {
        altrep_int_elt(object, index)
    } else {
        Ok(object.slice::<i32>()[index])
    }
}

/// Element `index` of a double vector, read like [`int_elt`].
#[inline]
pub unsafe fn real_elt(object: Sexp, index: usize) -> R<f64> {
    if object.is_altrep() {
        altrep_real_elt(object, index)
    } else {
        Ok(object.slice::<f64>()[index])
    }
}

/// The ALTREP branch of [`int_elt`]. A compact sequence computes the element from its first
/// element and increment, which is what its class's `Elt` method returns, without the call
/// through the class's method table that every other class pays.
#[cold]
#[inline(never)]
unsafe fn altrep_int_elt(object: Sexp, index: usize) -> R<i32> {
    match compact_sequence(object) {
        Some((_, first, increment)) => Ok((first + increment * index as f64) as i32),
        None => on_altrep(object, |context, object| {
            (interp().hooks.altrep_int_elt)(context, object, index)
        }),
    }
}

/// The ALTREP branch of [`real_elt`], read like [`altrep_int_elt`].
#[cold]
#[inline(never)]
unsafe fn altrep_real_elt(object: Sexp, index: usize) -> R<f64> {
    match compact_sequence(object) {
        Some((_, first, increment)) => Ok(first + increment * index as f64),
        None => on_altrep(object, |context, object| {
            (interp().hooks.altrep_real_elt)(context, object, index)
        }),
    }
}

/// Element `index` of a character ALTREP vector, read through its class's `Elt` method, so a
/// deferred string vector converts only that element. A plain vector's caller reads
/// [`string_elt`] instead and keeps the ALTREP test out of its loop.
#[cold]
#[inline(never)]
pub unsafe fn altrep_string_elt(object: Sexp, index: usize) -> R<Sexp> {
    altrep_result(object, |context, object| {
        Ok((interp().hooks.altrep_string_elt)(context, object, index)?.value())
    })
}

#[inline]
pub unsafe fn string_elt(object: Sexp, index: usize) -> Sexp {
    object.element(index)
}

#[inline]
pub unsafe fn list_elt(object: Sexp, index: usize) -> Sexp {
    object.element(index)
}

#[inline]
pub unsafe fn set_elt(object: Sexp, index: usize, value: Sexp) {
    crate::heap().set_element(object, index, value);
}

/// GNU R's `translateChar` refusal: a builtin that needs the characters of a string cannot read
/// one declared as bytes, unless it is ASCII. Checks every element of the string vector `strings`.
pub unsafe fn refuse_bytes(strings: Sexp) -> R<()> {
    if strings.kind() != SexpType::String {
        return Ok(());
    }
    for index in 0..length(strings) {
        let charsxp = strings.element(index);
        if charsxp.has_gp(gp::BYTES) && !charsxp.bytes().is_ascii() {
            return Err(crate::unwind::error(
                "translating strings with \"bytes\" encoding is not allowed",
            ));
        }
    }
    Ok(())
}

/// Whether two `CHARSXP`s hold the same text. The string cache interns each text once per
/// encoding, and ASCII text always unmarked, so a cached ASCII string is the only string that
/// holds its text (`NA_STRING` is uncached), and a different string can equal it only by being
/// the same object. Only strings that may differ in encoding need their text compared.
///
/// A name lookup calls this once per candidate with the wanted name as `right`, so `right` is
/// tested first: for the usual ASCII name the scan then reads no candidate's header.
#[inline]
pub unsafe fn same_string(left: Sexp, right: Sexp) -> bool {
    if left == right {
        return true;
    }
    let interned_ascii = gp::ASCII | gp::CACHED;
    if right.gp() & interned_ascii == interned_ascii || left.gp() & interned_ascii == interned_ascii
    {
        if cfg!(debug_assertions) && syntax::testing::invariants_enabled() {
            crate::handles::assert_distinct_text(
                heap::handle::Gc::adopt(left),
                heap::handle::Gc::adopt(right),
            );
        }
        return false;
    }
    same_text_across_encodings(left, right)
}

/// [`same_string`] for two strings neither of which is cached ASCII, kept out of line so the
/// common case inlines into a name scan: inlined, the text comparison made a name scan slower.
#[inline(never)]
unsafe fn same_text_across_encodings(left: Sexp, right: Sexp) -> bool {
    let na = na_string();
    if left == na || right == na {
        return false;
    }
    char_str(left) == char_str(right)
}

/// Where `byte` first occurs in `haystack`. Most R strings are a few bytes long, where
/// `memchr`'s vector setup costs more than a plain scan.
#[inline]
pub fn find_byte(byte: u8, haystack: &[u8]) -> Option<usize> {
    if haystack.len() < 16 {
        haystack.iter().position(|&candidate| candidate == byte)
    } else {
        memchr(byte, haystack)
    }
}

/// A `CHARSXP` as text. A byte that is not valid UTF-8 becomes its private-use stand-in (see
/// [`escape_invalid_bytes`]), which [`mkchar`] turns back into the byte.
#[inline]
pub unsafe fn char_str<'a>(charsxp: Sexp) -> Cow<'a, str> {
    if charsxp.is_valid_utf8() && !charsxp.has_gp(gp::LATIN1 | gp::BYTES) {
        // GNU R reads `CHAR()` as a C string, so native code that trims a string in place by
        // writing a NUL into it (udunits' `ut_trim`, called by units) shortens the text R sees.
        let bytes = charsxp.bytes();
        let bytes = find_byte(0, bytes).map_or(bytes, |end| &bytes[..end]);
        // SAFETY: the heap validated these bytes when it interned them, and a prefix that ends
        // before a NUL byte is still valid UTF-8.
        return Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(bytes) });
    }
    char_str_decoded(charsxp)
}

/// [`char_str`] for a string the heap has not validated as UTF-8 text.
#[cold]
unsafe fn char_str_decoded<'a>(charsxp: Sexp) -> Cow<'a, str> {
    let bytes = charsxp.bytes();
    // GNU R reads `CHAR()` as a C string, so native code that trims a string in place by
    // writing a NUL into it (udunits' `ut_trim`, called by units) shortens the text R sees.
    let bytes = memchr(0, bytes).map_or(bytes, |end| &bytes[..end]);
    // Strict validation runs a word at a time over ASCII, where the lossy decoder steps through
    // chunks. The bit is only a hint, since native code can set any of a string's flags.
    if charsxp.has_gp(gp::ASCII)
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return Cow::Borrowed(text);
    }
    if charsxp.has_gp(gp::LATIN1) {
        // GNU R translates a latin1 string as Windows-1252, and spells a byte that code page
        // leaves undefined `<xx>`.
        let mut text = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            match byte {
                0x80..=0x9f => match WINDOWS_1252_HIGH[usize::from(byte - 0x80)] {
                    '\0' => text.push_str(&format!("<{byte:02x}>")),
                    character => text.push(character),
                },
                _ => text.push(char::from(byte)),
            }
        }
        return Cow::Owned(text);
    }
    // GNU R reads a string declared as bytes through its `\xNN` spelling wherever it needs
    // text, so printing, formatting and pasting show the escapes rather than decoded characters.
    if charsxp.has_gp(gp::BYTES) {
        let mut text = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            if byte.is_ascii() {
                text.push(char::from(byte));
            } else {
                text.push_str(&format!("\\x{byte:02x}"));
            }
        }
        return Cow::Owned(text);
    }
    escape_invalid_bytes(bytes)
}

/// The characters of bytes 0x80 to 0x9F in Windows-1252, with NUL for a byte it leaves undefined.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20ac}', '\0', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\0', '\u{17d}', '\0', '\0',
    '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{2dc}',
    '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\0', '\u{17e}', '\u{178}',
];

/// Decodes bytes that need not be UTF-8 as text. GNU R keeps a string's bytes as they are, so a
/// Latin-1 string in a UTF-8 session, or a piece `strsplit(useBytes = TRUE)` cut out of a
/// character, must survive text processing unchanged. Each byte that is not valid UTF-8 becomes
/// the private-use character `U+10FF00 + byte`, which [`mkchar`] and string literals turn back
/// into the byte. A genuine character in `U+10FF80..=U+10FFFF` is therefore read back as a byte
/// when text holding it is interned again; those code points are the last of the last private-use
/// plane, so real text does not use them.
pub fn escape_invalid_bytes(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(text);
    }
    let mut text = String::with_capacity(bytes.len() + bytes.len() / 2);
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        for &byte in chunk.invalid() {
            text.push(stand_in(byte));
        }
    }
    Cow::Owned(text)
}

/// The private-use character a byte past ASCII travels as (see [`escape_invalid_bytes`]).
pub fn stand_in(byte: u8) -> char {
    char::from_u32(ESCAPED_BYTE_BASE + u32::from(byte)).unwrap_or('\u{fffd}')
}

/// Whether `text` holds a character in the stand-in range. Each one encodes as `F4 8F BE xx` or
/// `F4 8F BF xx` in UTF-8, and `F4` leads only the last plane of the code space, so most text is
/// rejected by the byte scan alone.
pub fn holds_stand_in(text: &str) -> bool {
    text.as_bytes().contains(&0xF4)
        && text
            .chars()
            .any(|character| escaped_byte(character).is_some())
}

/// Decodes bytes as text with every byte past ASCII as its private-use stand-in (see
/// [`escape_invalid_bytes`]), valid UTF-8 or not, so that text processing sees one character per
/// byte.
pub fn escape_high_bytes(bytes: &[u8]) -> Cow<'_, str> {
    if bytes.is_ascii() {
        return Cow::Borrowed(std::str::from_utf8(bytes).unwrap_or_default());
    }
    Cow::Owned(
        bytes
            .iter()
            .map(|&byte| match byte {
                0..=0x7F => byte as char,
                _ => stand_in(byte),
            })
            .collect(),
    )
}

/// The base of the private-use characters [`escape_invalid_bytes`] maps bytes to. The last code
/// points of plane 16 are the least likely to occur in real text.
const ESCAPED_BYTE_BASE: u32 = 0x10FF00;

/// The byte `character` stands for, if [`escape_invalid_bytes`] made it from one.
pub fn escaped_byte(character: char) -> Option<u8> {
    let code = u32::from(character);
    (ESCAPED_BYTE_BASE + 0x80..=ESCAPED_BYTE_BASE + 0xFF)
        .contains(&code)
        .then(|| (code - ESCAPED_BYTE_BASE) as u8)
}

/// `text` with every private-use stand-in turned back into its byte, or `None` when it holds
/// none.
pub fn unescape_invalid_bytes(text: &str) -> Option<Vec<u8>> {
    // ASCII text, the common case, needs only the vectorized ASCII scan.
    if text.is_ascii() || !holds_stand_in(text) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len());
    for character in text.chars() {
        match escaped_byte(character) {
            Some(byte) => bytes.push(byte),
            None => bytes.extend_from_slice(character.encode_utf8(&mut [0u8; 4]).as_bytes()),
        }
    }
    Some(bytes)
}

/// Text on its way out of the session (to the console or a connection) with every stand-in
/// turned back into its byte, so a string that is not valid UTF-8 is written as the bytes it
/// holds. Every stand-in encodes as `F4 8F BE xx` or `F4 8F BF xx`.
pub fn unescape_output(bytes: &[u8]) -> Cow<'_, [u8]> {
    let stand_in_at = |from: usize| {
        memchr::memchr_iter(0xF4, &bytes[from..])
            .map(|offset| from + offset)
            .find(|&start| {
                matches!(
                    bytes.get(start + 1..start + 4),
                    Some([0x8F, 0xBE | 0xBF, 0x80..=0xBF])
                )
            })
    };
    let Some(mut next) = stand_in_at(0) else {
        return Cow::Borrowed(bytes);
    };
    let mut output = Vec::with_capacity(bytes.len());
    let mut position = 0;
    loop {
        output.extend_from_slice(&bytes[position..next]);
        output.push(((bytes[next + 2] & 0x03) << 6) | (bytes[next + 3] & 0x3F));
        position = next + 4;
        match stand_in_at(position) {
            Some(start) => next = start,
            None => break,
        }
    }
    output.extend_from_slice(&bytes[position..]);
    Cow::Owned(output)
}

/// The text of a symbol's name.
#[inline]
pub unsafe fn symbol_name<'a>(symbol: Sexp) -> Cow<'a, str> {
    char_str(symbol.car())
}

/// Element `index` of a character vector as text, or `None` for `NA`.
pub unsafe fn string_at<'a>(object: Sexp, index: usize) -> Option<Cow<'a, str>> {
    if object.kind() != SexpType::String || index >= length(object) {
        return None;
    }
    let charsxp = object.element(index);
    if charsxp == na_string() {
        None
    } else {
        Some(char_str(charsxp))
    }
}

/// Every element of a character vector as owned text, with `NA` as `None`.
pub unsafe fn strings(object: Sexp) -> Vec<Option<String>> {
    (0..length(object))
        .map(|index| string_at(object, index).map(Cow::into_owned))
        .collect()
}

pub unsafe fn is_string_scalar(object: Sexp) -> bool {
    object.kind() == SexpType::String && length(object) == 1
}

/// The first element of a character vector as text.
pub unsafe fn first_string<'a>(object: Sexp) -> Option<Cow<'a, str>> {
    if object.kind() == SexpType::String && length(object) >= 1 {
        string_at(object, 0)
    } else {
        None
    }
}

pub unsafe fn is_function(object: Sexp) -> bool {
    is_function_type(object.kind())
}

/// Whether a value of type `kind` can be called: a closure, a builtin or a special.
pub fn is_function_type(kind: SexpType) -> bool {
    matches!(
        kind,
        SexpType::Closure | SexpType::Builtin | SexpType::Special
    )
}

pub unsafe fn is_numeric_type(kind: SexpType) -> bool {
    matches!(kind, SexpType::Logical | SexpType::Integer | SexpType::Real)
}

pub fn is_na_int(value: i32) -> bool {
    value == NA_INTEGER
}

pub fn na_real() -> f64 {
    NA_REAL
}

pub fn is_na(value: f64) -> bool {
    is_na_real(value)
}

// Attributes.

pub unsafe fn get_attr(object: Sexp, name: Sexp) -> Sexp {
    let mut cell = object.attrib();
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == name {
            return cell.car();
        }
        cell = cell.cdr();
    }
    nil()
}

pub unsafe fn has_attributes(object: Sexp) -> bool {
    object.attrib() != nil()
}

/// GNU R's `setAttrib` refuses `NULL` and the primitive functions: each is one object for the
/// whole session, so an attribute on it would show on every use.
pub fn check_attributable(kind: SexpType) -> R<()> {
    match kind {
        SexpType::Nil => Err(error("attempt to set an attribute on NULL")),
        kind @ (SexpType::Builtin | SexpType::Special | SexpType::Symbol) => Err(error(format!(
            "cannot set an attribute on a '{}'",
            kind.name()
        ))),
        _ => Ok(()),
    }
}

/// Sets an attribute without validating its value, or removes it when `value` is `NULL`. The
/// object bit follows the class attribute.
pub unsafe fn set_attr(object: Sexp, name: Sexp, value: Sexp) -> R<()> {
    handles::in_caller_scope(|context| {
        let mut object = Owned::assume(handles::held(context, object));
        let value = handles::held(context, value);
        handles::set_attribute(context, &mut object, Gc::adopt(name), value)
    })
}

pub unsafe fn remove_attr(object: Sexp, name: Sexp) {
    let heap = crate::heap();
    let nil = nil();
    let mut cell = object.attrib();
    let mut previous = nil;
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == name {
            if previous == nil {
                heap.set_attrib(object, cell.cdr());
            } else {
                heap.set_cdr(previous, cell.cdr());
            }
            break;
        }
        previous = cell;
        cell = cell.cdr();
    }
    if name == symbols().class {
        object.set_object(false);
    }
}

/// Copies every attribute of `from` onto `to`, sharing the values.
pub unsafe fn copy_most_attributes(from: Sexp, to: Sexp, keep_names_dims: bool) -> R<()> {
    handles::in_caller_scope(|context| {
        let from = handles::held(context, from);
        let mut to = Owned::assume(handles::held(context, to));
        handles::copy_most_attributes(context, from, &mut to, keep_names_dims)
    })
}

pub unsafe fn copy_attributes(from: Sexp, to: Sexp) -> R<()> {
    copy_most_attributes(from, to, true)
}

pub unsafe fn clear_attributes(object: Sexp) {
    crate::heap().set_attrib(object, nil());
    object.set_object(false);
    object.set_gp_bits(gp::S4_OBJECT, false);
}

pub unsafe fn names(object: Sexp) -> Sexp {
    if matches!(
        object.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        return pairlist_names(object).unwrap_or(nil());
    }
    if object.kind() == SexpType::Environment {
        return nil();
    }
    let names = get_attr(object, symbols().names);
    if names != nil() {
        return names;
    }
    // A one-dimensional array's names are its dimnames.
    let dim = get_attr(object, symbols().dim);
    if dim.kind() == SexpType::Integer && length(dim) == 1 {
        let dimnames = get_attr(object, symbols().dimnames);
        if dimnames.kind() == SexpType::List && length(dimnames) == 1 {
            return dimnames.element(0);
        }
    }
    nil()
}

/// The tags of a pairlist as a character vector, or `None` when no cell has a tag.
pub unsafe fn pairlist_names(list: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let list = handles::held(context, list);
        Ok(handles::pairlist_names(context, list)?.raw())
    })
}

pub unsafe fn class_attr(object: Sexp) -> Sexp {
    if object.is_object() {
        get_attr(object, symbols().class)
    } else {
        nil()
    }
}

/// Whether the class attribute names `class`.
pub unsafe fn inherits(object: Sexp, class: &str) -> bool {
    let classes = class_attr(object);
    if classes.kind() != SexpType::String {
        return false;
    }
    (0..length(classes)).any(|index| classes.element(index).bytes() == class.as_bytes())
}

/// An array's extents: matrices and small arrays keep them on the stack.
pub type Dims = smallvec::SmallVec<[usize; 4]>;

/// The dim attribute as integers, if any.
pub unsafe fn dims(object: Sexp) -> Option<Dims> {
    let dim = get_attr(object, symbols().dim);
    if dim == nil() {
        return None;
    }
    match dim.kind() {
        SexpType::Integer => Some(
            ints(dim)
                .iter()
                .map(|&value| value.max(0) as usize)
                .collect(),
        ),
        SexpType::Real => Some(
            reals(dim)
                .iter()
                .map(|&value| value.max(0.0) as usize)
                .collect(),
        ),
        _ => None,
    }
}

// Copying.

/// A deep copy, as `duplicate()` makes: vectors and pairlists are copied all the way down, while
/// environments, symbols, promises and external objects are shared.
///
/// The copy of an Array is a plain R vector. Whoever copies is about to write, and a builtin or C
/// code may write a missing value or an attribute that an Array cannot hold, so only the Array
/// code (which checks every write) puts the bit back. Arrays nested inside a copied list keep it,
/// because the copy does not write into them.
pub unsafe fn duplicate(object: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let object = handles::held(context, object);
        Ok(handles::deep_copy(context, object)?.finish().raw())
    })
}

/// A copy of the top level only: list elements and attribute values are shared. Like
/// [`duplicate`], it drops the Array bit.
pub unsafe fn shallow_duplicate(object: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let object = handles::held(context, object);
        Ok(handles::shallow_copy(context, object)?.finish().raw())
    })
}

/// GNU R's `R_shallow_duplicate_attr`: a copy of `object` for a caller that changes only its
/// attributes, which for a long vector (atomic, character or list) is a wrapper that shares the
/// data until something writes to it through the ALTREP interface. Its storage must never be
/// written directly (`slice_mut`, `reals_mut`). Like [`shallow_duplicate`], it drops the Array
/// bit.
pub unsafe fn shallow_duplicate_attr(object: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let object = handles::held(context, object);
        Ok(handles::attribute_copy(context, object)?.finish().raw())
    })
}

/// The copy a replacement function such as `attr<-` or `[<-` works on when its target is shared:
/// [`shallow_duplicate_attr`], except that an ALTREP instance becomes a new instance of its class
/// over the same data. Every instance counts as shared, so that an element write copies it into
/// ordinary storage, which means GNU R's in-place change of an unshared instance's attributes is
/// a copy here; a copy into ordinary storage would drop the class, and with it methods such as
/// `Serialized_state`. A caller that writes elements into the copy itself must use
/// [`shallow_duplicate`], because the wrapper `shallow_duplicate_attr` makes of a long vector
/// has no storage to write into.
pub unsafe fn replacement_copy(object: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let object = handles::held(context, object);
        Ok(handles::replacement_copy(context, object)?.raw())
    })
}

/// The length, first element and increment of a compact integer or double sequence, or `None`
/// for any other vector. The class's first data slot is GNU R's `c(n, n1, inc)`, which
/// serialization writes too, so reading it here saves the call into the host on hot paths.
#[inline]
pub unsafe fn compact_sequence(object: Sexp) -> Option<(usize, f64, f64)> {
    if !object.is_altrep() {
        return None;
    }
    let [integer, real] = crate::compact_sequence_classes()?;
    let class = object.tag();
    if class != integer && class != real {
        return None;
    }
    let &[count, first, increment] = object.car().slice::<f64>() else {
        return None;
    };
    Some((count as usize, first, increment))
}

/// Whether a complex assignment binds the data under `value` instead of `value`: a wrapper
/// [`shallow_duplicate_attr`] introduced is dropped for its data once the replacement has copied
/// that data, as GNU R's `try_assign_unwrap` does. `bound` says whether the variable's binding
/// is the one reference the value has.
#[inline]
pub unsafe fn unwraps(value: Sexp, bound: bool) -> bool {
    value.is_altrep() && (value.refcount() == 0 || (value.refcount() == 1 && bound))
}

/// GNU R's `WRAP_THRESHOLD`: a shorter vector is cheaper to copy than to wrap.
pub(crate) const WRAP_THRESHOLD: usize = 64;

/// `object` as a plain R vector: an Array becomes a copy without the Array bit, and anything else
/// is returned as is. R's own conversions (`as.double`, `as.vector`, `as.list`) go through here,
/// so they hand back an ordinary vector that `identical()` equates with `c(...)`.
pub unsafe fn plain(object: Sexp) -> R<Sexp> {
    if !object.is_array() {
        return Ok(object);
    }
    shallow_duplicate(object)
}

/// `MAYBE_SHARED`: more than one reference, so a modification must copy first.
#[inline]
pub unsafe fn maybe_shared(object: Sexp) -> bool {
    object.refcount() > 1
}

// Scalars from vectors.

/// The first element as a logical: `TRUE`, `FALSE` or `NA`.
pub unsafe fn as_logical_scalar(object: Sexp) -> i32 {
    if length(object) == 0 {
        return NA_LOGICAL;
    }
    match object.kind() {
        SexpType::Logical => ints(object)[0],
        SexpType::Integer => {
            let value = ints(object)[0];
            if value == NA_INTEGER {
                NA_LOGICAL
            } else {
                (value != 0) as i32
            }
        }
        SexpType::Real => {
            let value = reals(object)[0];
            if value.is_nan() {
                NA_LOGICAL
            } else {
                (value != 0.0) as i32
            }
        }
        SexpType::Complex => {
            let value = complexes(object)[0];
            if value.r.is_nan() || value.i.is_nan() {
                NA_LOGICAL
            } else {
                (value.r != 0.0 || value.i != 0.0) as i32
            }
        }
        SexpType::String => match string_at(object, 0).as_deref() {
            Some("TRUE" | "true" | "T" | "True") => 1,
            Some("FALSE" | "false" | "F" | "False") => 0,
            _ => NA_LOGICAL,
        },
        SexpType::Raw => (raws(object)[0] != 0) as i32,
        _ => NA_LOGICAL,
    }
}

/// The first element as an integer, or `NA`.
pub unsafe fn as_integer_scalar(object: Sexp) -> i32 {
    if length(object) == 0 {
        return NA_INTEGER;
    }
    match object.kind() {
        SexpType::Logical | SexpType::Integer => ints(object)[0],
        SexpType::Real => crate::coerce::real_to_int(reals(object)[0]).0,
        SexpType::Complex => crate::coerce::real_to_int(complexes(object)[0].r).0,
        SexpType::String => match string_at(object, 0) {
            Some(text) => {
                crate::coerce::real_to_int(crate::coerce::parse_real(&text).unwrap_or(NA_REAL)).0
            }
            None => NA_INTEGER,
        },
        SexpType::Raw => raws(object)[0] as i32,
        _ => NA_INTEGER,
    }
}

/// The first element as an integer, or `NA`, with the coercion warnings of GNU R's `asInteger`:
/// text that is no number and a double outside the integer range warn.
pub unsafe fn as_integer_scalar_warning(object: Sexp) -> R<i32> {
    if length(object) == 0 {
        return Ok(NA_INTEGER);
    }
    let mut warnings = crate::coerce::CoercionWarnings::default();
    let value = match object.kind() {
        SexpType::Real => {
            let (value, out_of_range) = crate::coerce::real_to_int(reals(object)[0]);
            warnings.integer_range = out_of_range;
            value
        }
        SexpType::Complex => {
            let value = complexes(object)[0];
            let (integer, out_of_range) = crate::coerce::real_to_int(value.r);
            warnings.integer_range = out_of_range;
            warnings.imaginary = !value.r.is_nan() && !value.i.is_nan() && value.i != 0.0;
            integer
        }
        SexpType::String => crate::coerce::integer_from_string(
            heap::handle::Gc::adopt(object.element(0)),
            &mut warnings,
        )?,
        _ => as_integer_scalar(object),
    };
    warnings.report()?;
    Ok(value)
}

/// The first element as a double, or `NA`, with the coercion warnings of GNU R's `asReal`.
pub unsafe fn as_real_scalar_warning(object: Sexp) -> R<f64> {
    if length(object) == 0 {
        return Ok(NA_REAL);
    }
    let mut warnings = crate::coerce::CoercionWarnings::default();
    let value = match object.kind() {
        SexpType::Complex => {
            let value = complexes(object)[0];
            warnings.imaginary = !value.r.is_nan() && !value.i.is_nan() && value.i != 0.0;
            value.r
        }
        SexpType::String => crate::coerce::real_from_string(
            heap::handle::Gc::adopt(object.element(0)),
            &mut warnings,
        )?,
        _ => as_real_scalar(object),
    };
    warnings.report()?;
    Ok(value)
}

/// The first element as a double, or `NA`.
pub unsafe fn as_real_scalar(object: Sexp) -> f64 {
    if length(object) == 0 {
        return NA_REAL;
    }
    match object.kind() {
        SexpType::Logical | SexpType::Integer => {
            let value = ints(object)[0];
            if value == NA_INTEGER {
                NA_REAL
            } else {
                value as f64
            }
        }
        SexpType::Real => reals(object)[0],
        SexpType::Complex => complexes(object)[0].r,
        SexpType::String => match string_at(object, 0) {
            Some(text) => crate::coerce::parse_real(&text).unwrap_or(NA_REAL),
            None => NA_REAL,
        },
        SexpType::Raw => raws(object)[0] as f64,
        _ => NA_REAL,
    }
}

/// The first element as text, as `asChar` gives it, or `None` for `NA` and non-vectors.
pub unsafe fn as_string_scalar(object: Sexp) -> R<Option<String>> {
    match object.kind() {
        SexpType::Symbol => Ok(Some(symbol_name(object).into_owned())),
        SexpType::Char => Ok(Some(char_str(object).into_owned())),
        kind if kind.is_atomic() && length(object) > 0 => {
            let charsxp = crate::coerce::element_as_charsxp(object, 0)?;
            if charsxp == na_string() {
                Ok(None)
            } else {
                Ok(Some(char_str(charsxp).into_owned()))
            }
        }
        _ => Ok(None),
    }
}

/// Requires a single non-`NA` string.
pub unsafe fn require_str<'a>(object: Sexp, what: &str) -> R<Cow<'a, str>> {
    match first_string(object) {
        Some(text) if length(object) == 1 => Ok(text),
        _ => Err(error(format!("invalid '{what}' argument"))),
    }
}

/// [`require_str`] as an owned string.
pub unsafe fn require_string(object: Sexp, what: &str) -> R<String> {
    require_str(object, what).map(Cow::into_owned)
}

/// Requires a single logical that is `TRUE` or `FALSE`.
pub unsafe fn require_flag(object: Sexp, what: &str) -> R<bool> {
    match as_logical_scalar(object) {
        NA_LOGICAL => Err(error(format!("invalid '{what}' argument"))),
        value => Ok(value != 0),
    }
}

/// The list's element named `name`, or `NULL`.
pub unsafe fn list_get(list: Sexp, name: &str) -> Sexp {
    let names = names(list);
    if names.kind() != SexpType::String {
        return nil();
    }
    for index in 0..length(names) {
        if names.element(index).bytes() == name.as_bytes() {
            return match list.kind() {
                SexpType::List | SexpType::Expression => list.element(index),
                _ => nil(),
            };
        }
    }
    nil()
}

/// The pairlist cells as `(tag, value)` pairs.
pub unsafe fn pairlist_items(mut list: Sexp) -> Vec<(Sexp, Sexp)> {
    let mut items = Vec::new();
    while matches!(
        list.kind(),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots
    ) {
        items.push((list.tag(), list.car()));
        list = list.cdr();
    }
    items
}

/// A list vector's elements.
pub unsafe fn list_items(list: Sexp) -> Vec<Sexp> {
    (0..length(list)).map(|index| list.element(index)).collect()
}

/// The pairlist as a list vector with the tags as names.
pub unsafe fn pairlist_to_list(list: Sexp) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let list = handles::held(context, list);
        Ok(handles::pairlist_to_list(context, list)?.raw())
    })
}

/// The list vector as a pairlist with its names as tags. An `NA` name is the tag `NA` under
/// `tag_na`, as where GNU R converts a list, and no tag otherwise, as where it sets names.
pub unsafe fn list_to_pairlist(list: Sexp, kind: SexpType, tag_na: bool) -> R<Sexp> {
    handles::in_caller_scope(|context| {
        let list = handles::held(context, list);
        Ok(handles::vector_to_pairlist(context, list, kind, tag_na)?
            .finish()
            .raw())
    })
}

/// Whether a `CHARSXP` is the empty string.
pub unsafe fn is_blank(charsxp: Sexp) -> bool {
    charsxp.bytes().is_empty()
}

pub unsafe fn is_true(object: Sexp) -> bool {
    object.kind() == SexpType::Logical && length(object) == 1 && ints(object)[0] == 1
}

/// The builtin's name, for a primitive object.
pub unsafe fn primitive_name<'a>(object: Sexp) -> &'a str {
    interp()
        .builtin(object.primitive_index())
        .map_or("<unknown>", |definition| definition.name)
}

/// The row count that compact row names (`c(NA, n)` or `c(NA, -n)`) stand for, or `None` for
/// row names of any other form.
pub unsafe fn compact_row_count(row_names: Sexp) -> Option<usize> {
    if row_names.kind() == SexpType::Integer
        && length(row_names) == 2
        && ints(row_names)[0] == NA_INTEGER
    {
        Some(ints(row_names)[1].unsigned_abs() as usize)
    } else {
        None
    }
}
