//! Constructing and reading R values. Every constructor here roots what it allocates in the
//! current handle scope (see the crate documentation), so callers never protect by hand.

use {
    crate::{
        interp, records, standard_library,
        unwind::{R, error},
    },
    heap::{
        Heap, Marker,
        object::{NA_INTEGER, NA_LOGICAL, NA_REAL, Rcomplex, Sexp, SexpType, gp, is_na_real},
        records::NativeKind,
        strings::Encoding,
    },
    std::borrow::Cow,
};

/// Symbols the interpreter compares against, installed once.
pub struct Symbols {
    pub brace: Sexp,
    pub paren: Sexp,
    pub if_: Sexp,
    pub for_: Sexp,
    pub while_: Sexp,
    pub repeat: Sexp,
    pub break_: Sexp,
    pub next: Sexp,
    pub function: Sexp,
    pub return_: Sexp,
    pub assign: Sexp,
    pub assign2: Sexp,
    pub assign_eq: Sexp,
    pub dots: Sexp,
    pub names: Sexp,
    pub dim: Sexp,
    pub dimnames: Sexp,
    pub class: Sexp,
    pub levels: Sexp,
    pub tsp: Sexp,
    pub row_names: Sexp,
    pub comment: Sexp,
    pub srcref: Sexp,
    pub srcfile: Sexp,
    pub whole_srcref: Sexp,
    pub tmp: Sexp,
    pub value: Sexp,
    pub dot_generic: Sexp,
    pub dot_class: Sexp,
    pub dot_method: Sexp,
    pub dot_group: Sexp,
    pub dot_generic_call_env: Sexp,
    pub dot_generic_def_env: Sexp,
    pub dot_s3_methods_table: Sexp,
    pub dot_options: Sexp,
    pub dot_random_seed: Sexp,
    pub dot_last_value: Sexp,
    pub dot_package_name: Sexp,
    pub dot_namespace: Sexp,
    pub bracket: Sexp,
    pub bracket2: Sexp,
    pub dollar: Sexp,
    pub at: Sexp,
    pub double_colon: Sexp,
    pub triple_colon: Sexp,
    pub tilde: Sexp,
    pub quote: Sexp,
    pub drop: Sexp,
    pub exact: Sexp,
    pub x: Sexp,
    pub object: Sexp,
    pub package: Sexp,
    pub dot_data: Sexp,
    pub dot_xdata: Sexp,
    pub dot_s3_class: Sexp,
    pub generic: Sexp,
    pub dot_defined: Sexp,
    pub dot_target: Sexp,
    pub dot_method_symbol: Sexp,
    pub dot_methods_dispatch: Sexp,
    pub plus: Sexp,
    pub minus: Sexp,
    pub not: Sexp,
    pub and2: Sexp,
    pub or2: Sexp,
    pub colon: Sexp,
    pub mode: Sexp,
    pub use_names: Sexp,
    pub na_rm: Sexp,
    pub previous: Sexp,
    pub call: Sexp,
    pub message: Sexp,
    pub dot_device: Sexp,
    pub dot_internal: Sexp,
    pub dot_primitive: Sexp,
}

macro_rules! symbol_table {
    ($($field:ident = $name:literal),* $(,)?) => {
        impl Symbols {
            pub(crate) unsafe fn install(heap: &mut Heap) -> Symbols {
                Symbols {
                    $($field: heap
                        .install($name.as_bytes())
                        .unwrap_or_else(|error| panic!("cannot install `{}`: {error}", $name)),)*
                }
            }

            pub(crate) unsafe fn trace(&self, marker: &mut Marker<'_>) {
                $(marker.mark(self.$field);)*
            }
        }
    };
}

symbol_table! {
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
    value = "value",
    dot_generic = ".Generic",
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
    bracket = "[",
    bracket2 = "[[",
    dollar = "$",
    at = "@",
    double_colon = "::",
    triple_colon = ":::",
    tilde = "~",
    quote = "quote",
    drop = "drop",
    exact = "exact",
    x = "x",
    object = "object",
    package = "package",
    dot_data = ".Data",
    dot_xdata = ".xData",
    dot_s3_class = ".S3Class",
    generic = "generic",
    dot_defined = ".defined",
    dot_target = ".target",
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
    na_rm = "na.rm",
    previous = "previous",
    call = "call",
    message = "message",
    dot_device = ".Device",
    dot_internal = ".Internal",
    dot_primitive = ".Primitive",
}

/// Session-wide objects the builtins keep. Each is rooted for the life of the session.
pub struct Globals {
    /// `R_MethodsNamespace`, once `methods` is loaded.
    pub methods_namespace: Sexp,
    /// Whether the methods package turned S4 dispatch on, which makes `standardGeneric` and
    /// primitive generics dispatch.
    pub methods_dispatch: bool,
    /// Whether primitives may dispatch to S4 methods at all. The methods package turns this off
    /// while it selects an inherited method, so that selection itself never dispatches.
    pub allow_primitive_methods: bool,
    /// Primitives the methods package turned generic, by primitive index.
    pub primitive_methods: rustc_hash::FxHashMap<u32, crate::dispatch::PrimitiveMethods>,
    /// Environments `attach` put on the search path are ordinary enclosures of the global
    /// environment; this keeps `Autoloads` and the like alive between `attach` and first use.
    pub keep: Vec<Sexp>,
    /// `.cache_class`: each S4 class's S3 class vector (the class and what it extends), which
    /// the methods package keeps current as it defines and removes classes.
    pub s4_extends: rustc_hash::FxHashMap<Box<[u8]>, Sexp>,
}

impl Globals {
    pub(crate) fn placeholder() -> Globals {
        Globals {
            methods_namespace: Sexp::dangling(),
            methods_dispatch: false,
            allow_primitive_methods: true,
            primitive_methods: rustc_hash::FxHashMap::default(),
            keep: Vec::new(),
            s4_extends: rustc_hash::FxHashMap::default(),
        }
    }

    pub(crate) unsafe fn install() {
        let nil = crate::heap().nil();
        let globals = &mut interp().globals;
        globals.methods_namespace = nil;
    }

    pub(crate) unsafe fn trace(&self, marker: &mut Marker<'_>) {
        marker.mark(self.methods_namespace);
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
    }
}

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

#[inline]
pub unsafe fn alloc(kind: SexpType, length: usize) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.alloc_vector(kind, length as isize)?;
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
    mkchar_bytes(text.as_bytes(), Encoding::Utf8)
}

pub unsafe fn mkchar_bytes(bytes: &[u8], encoding: Encoding) -> R<Sexp> {
    let heap = crate::heap();
    let object = heap.mk_char(bytes, encoding)?;
    heap.root(object);
    Ok(object)
}

pub unsafe fn install(name: &str) -> R<Sexp> {
    Ok(crate::heap().install(name.as_bytes())?)
}

pub unsafe fn install_bytes(name: &[u8]) -> R<Sexp> {
    Ok(crate::heap().install(name)?)
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
    let object = alloc(SexpType::Integer, 1)?;
    object.slice_mut::<i32>()[0] = value;
    Ok(object)
}

pub unsafe fn scalar_real(value: f64) -> R<Sexp> {
    let object = alloc(SexpType::Real, 1)?;
    object.slice_mut::<f64>()[0] = value;
    Ok(object)
}

pub unsafe fn scalar_complex(value: Rcomplex) -> R<Sexp> {
    let object = alloc(SexpType::Complex, 1)?;
    object.slice_mut::<Rcomplex>()[0] = value;
    Ok(object)
}

pub unsafe fn scalar_raw(value: u8) -> R<Sexp> {
    let object = alloc(SexpType::Raw, 1)?;
    object.slice_mut::<u8>()[0] = value;
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

/// A call `function(arguments...)` with untagged arguments.
pub unsafe fn call(function: Sexp, arguments: &[Sexp]) -> R<Sexp> {
    let mut items = vec![(nil(), function)];
    items.extend(arguments.iter().map(|&argument| (nil(), argument)));
    pairlist(&items, SexpType::Language)
}

// Reading.

/// R's `length()` for every type, without dispatch.
pub unsafe fn length(object: Sexp) -> usize {
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
    crate::heap().frame_symbols(env).len()
}

/// Asks the class's `Length` method, without materializing the elements. `length` cannot
/// fail, so an error from the method is parked like an element accessor's
/// ([`crate::unwind::park`]) and the length reads as 0 until it is raised.
unsafe fn altrep_length(object: Sexp) -> usize {
    match (interp().hooks.altrep_length)(object) {
        Ok(count) => count,
        Err(unwind) => {
            crate::unwind::park(unwind);
            0
        }
    }
}

/// An ordinary vector with the contents of `object`, which is `object` itself unless it is
/// ALTREP.
pub unsafe fn materialize(object: Sexp) -> R<Sexp> {
    if object.is_altrep() {
        let vector = (interp().hooks.materialize_altrep)(object)?;
        crate::heap().root(vector);
        Ok(vector)
    } else {
        Ok(object)
    }
}

macro_rules! typed_slice {
    ($name:ident, $name_mut:ident, $type:ty) => {
        /// The elements, which must be of the matching type. An ALTREP vector is materialized.
        #[inline]
        pub unsafe fn $name<'a>(object: Sexp) -> &'a [$type] {
            if object.is_altrep() {
                // The accessor cannot fail, so a method's error is parked, as for elements.
                match materialize(object) {
                    Ok(vector) => vector.slice::<$type>(),
                    Err(unwind) => {
                        crate::unwind::park(unwind);
                        &[]
                    }
                }
            } else {
                object.slice::<$type>()
            }
        }

        #[inline]
        #[allow(clippy::mut_from_ref)]
        pub unsafe fn $name_mut<'a>(object: Sexp) -> &'a mut [$type] {
            object.slice_mut::<$type>()
        }
    };
}

typed_slice!(ints, ints_mut, i32);
typed_slice!(reals, reals_mut, f64);
typed_slice!(complexes, complexes_mut, Rcomplex);
typed_slice!(raws, raws_mut, u8);

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

/// A `CHARSXP` as text. Invalid UTF-8 is replaced, which only affects byte-encoded strings.
pub unsafe fn char_str<'a>(charsxp: Sexp) -> Cow<'a, str> {
    let bytes = charsxp.bytes();
    if charsxp.has_gp(gp::LATIN1) {
        return Cow::Owned(bytes.iter().map(|&byte| byte as char).collect());
    }
    String::from_utf8_lossy(bytes)
}

/// Whether two distinct interned strings hold the same text. Strings intern by bytes and
/// encoding, so a native string and a UTF-8 one with the same bytes are distinct objects, and so
/// are a latin1 string and its UTF-8 translation; GNU R's `Seql` calls both pairs equal because
/// it compares their UTF-8 translations, and the session's native encoding is UTF-8.
pub unsafe fn same_text(left: Sexp, right: Sexp) -> bool {
    if left.has_gp(gp::BYTES) || right.has_gp(gp::BYTES) {
        return false;
    }
    // Interning by bytes and encoding means two distinct strings of the same encoding differ in
    // their bytes, which keeps `x == "a"` from decoding every element.
    if [gp::LATIN1, gp::UTF8]
        .iter()
        .all(|&bit| left.has_gp(bit) == right.has_gp(bit))
    {
        return false;
    }
    char_str(left) == char_str(right)
}

/// The text of a symbol's name.
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
    matches!(
        object.kind(),
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

/// Sets an attribute without validating it, or removes it when `value` is `NULL`. The object
/// bit follows the class attribute.
pub unsafe fn set_attr(object: Sexp, name: Sexp, value: Sexp) -> R<()> {
    let heap = crate::heap();
    let nil = nil();
    if value == nil {
        remove_attr(object, name);
        return Ok(());
    }
    let mut cell = object.attrib();
    let mut last = nil;
    while cell.kind() == SexpType::Pairlist {
        if cell.tag() == name {
            heap.set_car(cell, value);
            if name == symbols().class {
                object.set_object(true);
            }
            return Ok(());
        }
        last = cell;
        cell = cell.cdr();
    }
    let fresh = heap.alloc_triple(SexpType::Pairlist, value, nil, name)?;
    heap.root(fresh);
    if last == nil {
        heap.set_attrib(object, fresh);
    } else {
        heap.set_cdr(last, fresh);
    }
    if name == symbols().class {
        object.set_object(true);
    }
    Ok(())
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
    let symbols = symbols();
    let mut cell = from.attrib();
    while cell.kind() == SexpType::Pairlist {
        let tag = cell.tag();
        let structural = tag == symbols.names || tag == symbols.dim || tag == symbols.dimnames;
        if keep_names_dims || !structural {
            set_attr(to, tag, cell.car())?;
        }
        cell = cell.cdr();
    }
    if from.is_object() {
        to.set_object(true);
    }
    if from.has_gp(gp::S4_OBJECT) {
        to.set_gp_bits(gp::S4_OBJECT, true);
    }
    Ok(())
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
    if matches!(object.kind(), SexpType::Pairlist | SexpType::Language) {
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
    let count = pairlist_length(list);
    let mut any = false;
    let mut cell = list;
    while cell.kind() != SexpType::Nil {
        if cell.tag() != nil() {
            any = true;
            break;
        }
        cell = cell.cdr();
    }
    if !any {
        return Ok(nil());
    }
    let result = alloc(SexpType::String, count)?;
    let mut cell = list;
    let mut index = 0;
    while index < count {
        let tag = cell.tag();
        if tag.kind() == SexpType::Symbol {
            set_elt(result, index, tag.car());
        }
        cell = cell.cdr();
        index += 1;
    }
    Ok(result)
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

/// The dim attribute as integers, if any.
pub unsafe fn dims(object: Sexp) -> Option<Vec<usize>> {
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
pub unsafe fn duplicate(object: Sexp) -> R<Sexp> {
    duplicate_with(object, true)
}

/// A copy of the top level only: list elements and attribute values are shared.
pub unsafe fn shallow_duplicate(object: Sexp) -> R<Sexp> {
    duplicate_with(object, false)
}

unsafe fn duplicate_with(object: Sexp, deep: bool) -> R<Sexp> {
    let heap = crate::heap();
    let kind = object.kind();
    let copy = match kind {
        SexpType::Nil
        | SexpType::Symbol
        | SexpType::Environment
        | SexpType::Special
        | SexpType::Builtin
        | SexpType::Promise
        | SexpType::ExternalPtr
        | SexpType::WeakRef
        | SexpType::Char
        | SexpType::Binding
        | SexpType::Bytecode => return Ok(object),
        SexpType::Pairlist | SexpType::Language | SexpType::Dots => {
            let mut items = Vec::new();
            let mut cell = object;
            while matches!(
                cell.kind(),
                SexpType::Pairlist | SexpType::Language | SexpType::Dots
            ) {
                let value = if deep {
                    duplicate(cell.car())?
                } else {
                    cell.car()
                };
                items.push((cell.tag(), value, cell.attrib()));
                cell = cell.cdr();
            }
            let mut list = nil();
            for (position, (tag, value, attributes)) in items.into_iter().enumerate().rev() {
                let cell_kind = if position == 0 {
                    kind
                } else {
                    SexpType::Pairlist
                };
                list = triple(cell_kind, value, list, tag)?;
                // The head cell's attributes and flags are the object's, copied below.
                if position != 0 && attributes != nil() {
                    let attributes = if deep {
                        duplicate(attributes)?
                    } else {
                        attributes
                    };
                    heap.set_attrib(list, attributes);
                }
            }
            if list == nil() {
                return Ok(list);
            }
            list
        }
        SexpType::Closure => {
            let copy = triple(SexpType::Closure, object.car(), object.cdr(), object.tag())?;
            copy
        }
        SexpType::Object => {
            let copy = triple(SexpType::Object, nil(), nil(), nil())?;
            copy
        }
        _ if kind.is_vector() => {
            let source = materialize(object)?;
            let count = source.length() as usize;
            let copy = alloc(kind, count)?;
            match kind {
                SexpType::String => {
                    for index in 0..count {
                        heap.set_element(copy, index, source.element(index));
                    }
                }
                SexpType::List | SexpType::Expression => {
                    for index in 0..count {
                        let element = source.element(index);
                        let element = if deep { duplicate(element)? } else { element };
                        heap.set_element(copy, index, element);
                    }
                }
                _ => {
                    let bytes = count * kind.element_size().unwrap_or(0);
                    std::ptr::copy_nonoverlapping(source.data(), copy.data(), bytes);
                }
            }
            copy
        }
        _ => return Ok(object),
    };
    if object.attrib() != nil() {
        let attributes = if deep {
            duplicate(object.attrib())?
        } else {
            shallow_duplicate(object.attrib())?
        };
        heap.set_attrib(copy, attributes);
    }
    copy.set_object(object.is_object());
    copy.set_gp(object.gp() & (gp::S4_OBJECT));
    Ok(copy)
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
pub unsafe fn require_string(object: Sexp, what: &str) -> R<String> {
    match first_string(object) {
        Some(text) if length(object) == 1 => Ok(text.into_owned()),
        _ => Err(error(format!("invalid '{what}' argument"))),
    }
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
    let items = pairlist_items(list);
    let values: Vec<Sexp> = items.iter().map(|(_, value)| *value).collect();
    let result = list_vector(&values)?;
    if items.iter().any(|(tag, _)| *tag != nil()) {
        let names = alloc(SexpType::String, items.len())?;
        for (index, (tag, _)) in items.iter().enumerate() {
            if tag.kind() == SexpType::Symbol {
                set_elt(names, index, tag.car());
            }
        }
        set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// The list vector as a pairlist with its names as tags.
pub unsafe fn list_to_pairlist(list: Sexp, kind: SexpType) -> R<Sexp> {
    let names = names(list);
    let mut items = Vec::new();
    for index in 0..length(list) {
        let tag = if names.kind() == SexpType::String {
            let name = names.element(index);
            if name.bytes().is_empty() || name == na_string() {
                nil()
            } else {
                crate::heap().install_char(name)?
            }
        } else {
            nil()
        };
        let value = match list.kind() {
            SexpType::List | SexpType::Expression => list.element(index),
            _ => crate::subset::vector_element(list, index)?,
        };
        items.push((tag, value));
    }
    pairlist(&items, kind)
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
