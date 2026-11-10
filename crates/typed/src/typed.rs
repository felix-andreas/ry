//! ry's typed core: native values (`values`) and the compiler that turns a strict typed closure
//! into machine code over them.
//!
//! A call of a typed closure from R reaches [`call`], which reads the argument values' types,
//! compiles the closure for them once (lowering its body to the IR, inserting the reference
//! counts and generating code), converts the arguments to native values, runs the code and
//! converts its value back. Whatever the compiler cannot handle, and every call whose code gives
//! up partway, runs in the interpreter instead, which the caller does when [`call`] returns
//! `None`. Code gives up before anything it did is visible, so running the call again from the
//! start is always correct.

#![allow(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod ir;
pub mod lower;
pub mod ownership;
pub mod values;

use {
    crate::{
        backend::{Callee, Code, Trampoline},
        ir::{Item, Type},
        lower::{Callees, Speculation},
        values::{List, Str},
    },
    heap::{
        object::{Sexp, SexpType, gp},
        strings::Encoding,
    },
    interp::{unwind::Unwind, values as rvalues},
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::{cell::RefCell, sync::OnceLock},
};

/// Why a closure does not compile, for the log.
#[derive(Clone, Copy, Debug)]
pub struct Refusal(pub &'static str);

impl From<ssa::Error> for Refusal {
    fn from(error: ssa::Error) -> Self {
        Refusal(error.0)
    }
}

/// A closure compiled for one list of argument types.
pub struct Compiled {
    pub code: Code,
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
    /// The function lookups this code and every function it calls assume.
    pub speculations: Vec<Speculation>,
}

type Key = (Sexp, SmallVec<[Type; 4]>);

thread_local! {
    /// Compiled closures and refusals, by closure and argument types.
    static COMPILED: RefCell<FxHashMap<Key, Option<&'static Compiled>>> =
        RefCell::new(FxHashMap::default());
    /// The closures being compiled, so mutual recursion stops instead of looping.
    static COMPILING: RefCell<FxHashSet<Key>> = RefCell::new(FxHashSet::default());
}

/// How many closures a session compiles at most, since each keeps its closure alive.
const MAX_COMPILED: usize = 4096;

/// How many refusals the table keeps before it forgets them. A refusal is keyed by a closure
/// that is not rooted, so its address may be reused by another closure that deserves a try.
const MAX_REFUSALS: usize = 65536;

/// The types a recursive call's result is first assumed to have, when the closure declares
/// none. Each lowering that finds a different result type retries with the one it found.
const GUESSES: [Type; 4] = [Type::Dbl, Type::Int, Type::Lgl, Type::Str];

/// How often a recursive closure's lowering may revise its result type.
const MAX_REVISIONS: usize = 3;

/// Calls the typed closure `function` with argument values `values`, one per formal in order,
/// through its compiled code. `None` when the closure does not compile for these values or the
/// code gives up, and the caller then makes the call in the interpreter.
pub unsafe fn call(function: Sexp, values: &[Sexp]) -> Option<Result<Sexp, Unwind>> {
    if !interp::annotations::is_typed(function)
        || function.has_gp(gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE)
    {
        return None;
    }
    let mut params: SmallVec<[Type; 4]> = SmallVec::new();
    for (position, &value) in values.iter().enumerate() {
        // An `I64` formal converts a whole R number at the boundary, which `Arguments::push`
        // does, or gives up for the interpreter to raise the error.
        if interp::annotations::declares_i64(function, position)
            && matches!(type_of(value), Some(Type::Int | Type::Dbl | Type::I64))
        {
            params.push(Type::I64);
            continue;
        }
        let ty = type_of(value)?;
        // A length-one vector passed where the closure declares a vector is that vector, not a
        // scalar.
        let ty = match ty.item() {
            Some(item) if interp::annotations::declares_vector(function, position) => {
                Type::vector(item)
            }
            _ => ty,
        };
        params.push(ty);
    }
    let compiled = compiled_for(function, &params)?;
    if matches!(compiled.result, Type::Dict { .. }) {
        return None;
    }
    let mut arguments = Arguments::default();
    for (&value, &ty) in values.iter().zip(&params) {
        if !arguments.push(value, ty) {
            arguments.release();
            return None;
        }
    }
    let heap = interp::heap();
    let mark = heap.handle_mark();
    let mut out = [0u64; 2];
    let trampoline: Trampoline = std::mem::transmute(compiled.code.trampoline);
    let status = trampoline(depth_budget(), arguments.words.as_ptr(), out.as_mut_ptr());
    if status != 0 {
        arguments.release();
        heap.release_handles(mark);
        return None;
    }
    let value = to_r(compiled.result, out);
    drop_words(compiled.result, out);
    arguments.release();
    heap.release_handles(mark);
    interp::interp().visible = true;
    Some(value)
}

/// The compiled code for calling `function` with arguments of types `params`, compiling it on
/// first use. `None` when the closure does not compile for them.
pub unsafe fn compiled_for(function: Sexp, params: &[Type]) -> Option<&'static Compiled> {
    let key: Key = (function, SmallVec::from_slice(params));
    match COMPILED.with_borrow(|compiled| compiled.get(&key).copied()) {
        // A function the code calls was rebound since it compiled: compile it again.
        Some(Some(compiled)) if !still_resolves(compiled) => {}
        Some(found) => return found,
        None => {}
    }
    if COMPILED.with_borrow(|compiled| compiled.values().filter(|entry| entry.is_some()).count())
        >= MAX_COMPILED
        || !COMPILING.with_borrow_mut(|compiling| compiling.insert(key.clone()))
    {
        return None;
    }
    let result = compile(function, params);
    COMPILING.with_borrow_mut(|compiling| compiling.remove(&key));
    let compiled = match result {
        Ok(compiled) => {
            interp::heap().add_global_root(function);
            if logging() {
                eprintln!("typed: compiled for {params:?} -> {:?}", compiled.result);
            }
            Some(&*Box::leak(Box::new(compiled)))
        }
        Err(Refusal(why)) => {
            if logging() {
                eprintln!("typed: no code for {params:?}: {why}");
            }
            None
        }
    };
    COMPILED.with_borrow_mut(|table| {
        if compiled.is_none() && table.len() >= MAX_REFUSALS {
            table.retain(|_, entry| entry.is_some());
        }
        table.insert(key, compiled);
    });
    compiled
}

/// Whether `function` has compiled code for some argument types.
pub fn has_compiled(function: Sexp) -> bool {
    COMPILED.with_borrow(|compiled| {
        compiled
            .iter()
            .any(|((closure, _), entry)| *closure == function && entry.is_some())
    })
}

/// Whether every function lookup `compiled` assumes still finds the same function.
unsafe fn still_resolves(compiled: &Compiled) -> bool {
    compiled.speculations.iter().all(|assumed| {
        let resolve = if interp::values::is_function(assumed.function) {
            lower::resolve_function
        } else {
            lower::resolve_value
        };
        resolve(assumed.symbol, assumed.env)
            .is_some_and(|now| now.cell == assumed.cell && now.function == assumed.function)
    })
}

unsafe fn compile(function: Sexp, params: &[Type]) -> Result<Compiled, Refusal> {
    let declared = if interp::annotations::returns_i64(function) {
        Some(Type::I64)
    } else {
        interp::annotations::declared_return_type(function).and_then(declared_type)
    };
    let guesses: SmallVec<[Type; 5]> = declared.into_iter().chain(GUESSES).collect();
    let mut refusal = Refusal("the result type depends on the recursion");
    let mut found = None;
    'guesses: for &first in &guesses {
        let mut guess = first;
        for _ in 0..=MAX_REVISIONS {
            let mut linker = Linker::default();
            let lowered = match lower::lower(function, params, guess, &mut linker) {
                Ok(lowered) => lowered,
                Err(why) => {
                    refusal = why;
                    continue 'guesses;
                }
            };
            let result = lowered.function.result;
            if result == guess || !calls_itself(&lowered.function) {
                found = Some((lowered, linker));
                break 'guesses;
            }
            guess = result;
        }
    }
    let (lowered, linker) = found.ok_or(refusal)?;
    let mut ir = lowered.function;
    let arguments: SmallVec<[Option<(SexpType, bool)>; 4]> =
        params.iter().map(|&ty| storage(ty)).collect();
    if !interp::annotations::admits_storage(function, &arguments, storage(ir.result)) {
        return Err(Refusal("the types fail the closure's annotations"));
    }
    ownership::insert(&mut ir);
    let code = backend::compile(&ir, &linker.callees)?;
    let mut speculations = lowered.speculations;
    speculations.extend(linker.speculations);
    Ok(Compiled {
        code,
        params: SmallVec::from_slice(params),
        result: ir.result,
        speculations,
    })
}

/// Whether the function calls itself, so its result type is an assumption until confirmed.
fn calls_itself(function: &ir::Function) -> bool {
    function.blocks.iter().any(|block| {
        block
            .insts
            .iter()
            .any(|inst| matches!(inst.op, ir::Op::CallTyped(lower::SELF, _)))
    })
}

/// The compiled typed closures a function calls, in the order its calls index them.
#[derive(Default)]
struct Linker {
    callees: Vec<Callee>,
    speculations: Vec<Speculation>,
}

impl Callees for Linker {
    unsafe fn callee(&mut self, function: Sexp, params: &[Type]) -> Result<(usize, Type), Refusal> {
        let compiled = compiled_for(function, params)
            .ok_or(Refusal("a typed function it calls does not compile"))?;
        // Code that calls another function directly is only right while that function's own
        // lookups hold.
        self.speculations.extend_from_slice(&compiled.speculations);
        self.callees.push(Callee {
            address: compiled.code.address,
            params: SmallVec::from_slice(params),
            result: compiled.result,
        });
        Ok((self.callees.len() - 1, compiled.result))
    }
}

fn declared_type(declared: interp::annotations::Declared) -> Option<Type> {
    let item = match declared.kind {
        SexpType::Integer => Item::Int,
        SexpType::Real => Item::Dbl,
        SexpType::Logical => Item::Lgl,
        _ => return None,
    };
    Some(if declared.scalar {
        Type::of_item(item)
    } else {
        Type::vector(item)
    })
}

/// The R storage type of a value of `ty` once it crosses back, and whether it is an `Array`.
fn storage(ty: Type) -> Option<(SexpType, bool)> {
    Some(match ty {
        Type::Int => (SexpType::Integer, false),
        Type::Dbl => (SexpType::Real, false),
        Type::I64 => (SexpType::I64, false),
        Type::Lgl | Type::Bool => (SexpType::Logical, false),
        Type::Str => (SexpType::String, false),
        Type::Null => (SexpType::Nil, false),
        Type::Vector { item, array } => (item_storage(item), array),
        Type::List(_) => (SexpType::List, false),
        Type::Dict { .. } => return None,
    })
}

fn item_storage(item: Item) -> SexpType {
    match item {
        Item::Int => SexpType::Integer,
        Item::Dbl => SexpType::Real,
        Item::Lgl => SexpType::Logical,
        Item::Str => SexpType::String,
        Item::I64 => SexpType::I64,
    }
}

fn item_kind(item: Item) -> u32 {
    u32::from(match item {
        Item::Int => values::INT,
        Item::Dbl => values::DBL,
        Item::Lgl => values::LGL,
        Item::Str => values::STR,
        Item::I64 => values::I64,
    })
}

/// The typed core's type for an R value, or `None` when it has none: a length-one plain vector
/// is a scalar, a longer or empty one a vector, an `Array` an array, and a plain list of
/// length-one strings, numbers or logicals of one storage type a list of them.
unsafe fn type_of(value: Sexp) -> Option<Type> {
    if value.is_altrep() || value.attrib() != rvalues::nil() {
        return None;
    }
    let array = value.is_array();
    let length = rvalues::length(value);
    let item = match value.kind() {
        SexpType::Integer => Item::Int,
        SexpType::Real => Item::Dbl,
        SexpType::Logical => Item::Lgl,
        SexpType::I64 => Item::I64,
        SexpType::String => {
            if !(0..length).all(|index| usable_string(value.element(index))) {
                return None;
            }
            Item::Str
        }
        SexpType::List if !array => return list_type(value, length),
        _ => return None,
    };
    if length == 1 && !array {
        return Some(Type::of_item(item));
    }
    Some(Type::Vector { item, array })
}

unsafe fn list_type(value: Sexp, length: usize) -> Option<Type> {
    let mut item = None;
    for index in 0..length {
        let element = value.element(index);
        let ty = type_of(element)?;
        let found = ty.item().filter(|_| ty != Type::Bool)?;
        if item.is_some_and(|item| item != found) {
            return None;
        }
        item = Some(found);
    }
    // An empty list could hold anything, and the typed core only knows lists of one item type.
    item.map(Type::List)
}

/// Whether a `CHARSXP` is a string the typed core can hold: not `NA`, and UTF-8.
unsafe fn usable_string(charsxp: Sexp) -> bool {
    charsxp != interp::heap().known().na_string
        && !charsxp.has_gp(gp::LATIN1 | gp::BYTES)
        && std::str::from_utf8(charsxp.bytes()).is_ok()
}

/// The arguments of one call as machine words, with what converting them allocated.
#[derive(Default)]
struct Arguments {
    words: SmallVec<[u64; 8]>,
    views: SmallVec<[*mut List; 4]>,
    lists: SmallVec<[*mut List; 4]>,
    strings: SmallVec<[Str; 4]>,
}

impl Arguments {
    /// Appends `value`, of type `ty`, and whether converting it worked.
    unsafe fn push(&mut self, value: Sexp, ty: Type) -> bool {
        match ty {
            Type::Int | Type::Lgl => self.words.push(rvalues::ints(value)[0] as u32 as u64),
            Type::Dbl => self.words.push(rvalues::reals(value)[0].to_bits()),
            Type::I64 => {
                let number = match interp::numbers::read(value) {
                    Some(number) => Some(number),
                    None => interp::numbers::r_source(value)
                        .ok()
                        .and_then(interp::numbers::convert),
                };
                let Some(number) = number else {
                    return false;
                };
                self.words.push(number as u64);
            }
            Type::Str => {
                let string = str_of(value.element(0));
                self.strings.push(string);
                self.words.extend([string.lo, string.hi]);
            }
            Type::Vector {
                item: Item::Str, ..
            } => {
                let list = string_list(value);
                if list.is_null() {
                    return false;
                }
                self.lists.push(list);
                self.words.push(list as u64);
            }
            Type::Vector { item, .. } => {
                let data = value.data();
                let view =
                    values::ry_list_view(item_kind(item), data, rvalues::length(value) as u64);
                if view.is_null() {
                    return false;
                }
                self.views.push(view);
                self.words.push(view as u64);
            }
            Type::List(item) => {
                let length = rvalues::length(value);
                let list = values::ry_list_new(item_kind(item), length as u64);
                if list.is_null() {
                    return false;
                }
                for index in 0..length {
                    let element = value.element(index);
                    let slot = (*list).data;
                    match item {
                        Item::Int | Item::Lgl => {
                            *(slot as *mut i32).add(index) = rvalues::ints(element)[0]
                        }
                        Item::Dbl => *(slot as *mut f64).add(index) = rvalues::reals(element)[0],
                        Item::I64 => *(slot as *mut i64).add(index) = element.slice::<i64>()[0],
                        Item::Str => *(slot as *mut Str).add(index) = str_of(element.element(0)),
                    }
                }
                self.lists.push(list);
                self.words.push(list as u64);
            }
            Type::Bool | Type::Null | Type::Dict { .. } => return false,
        }
        true
    }

    unsafe fn release(&mut self) {
        for &view in &self.views {
            values::ry_view_release(view);
        }
        for &list in &self.lists {
            values::ry_list_drop(list);
        }
        for &string in &self.strings {
            values::ry_str_drop(string.lo, string.hi);
        }
        self.views.clear();
        self.lists.clear();
        self.strings.clear();
    }
}

unsafe fn str_of(charsxp: Sexp) -> Str {
    let bytes = charsxp.bytes();
    values::ry_str_from_bytes(bytes.as_ptr(), bytes.len() as u64)
}

/// A string list holding the strings of the character vector `value`.
unsafe fn string_list(value: Sexp) -> *mut List {
    let length = rvalues::length(value);
    let list = values::ry_list_new(u32::from(values::STR), length as u64);
    if list.is_null() {
        return list;
    }
    let data = (*list).data as *mut Str;
    for index in 0..length {
        *data.add(index) = str_of(value.element(index));
    }
    list
}

/// The R value of the native value whose words are `out`, which stays owned by the caller.
unsafe fn to_r(ty: Type, out: [u64; 2]) -> Result<Sexp, Unwind> {
    let word = out[0];
    match ty {
        Type::Int => rvalues::scalar_integer(word as u32 as i32),
        Type::Lgl | Type::Bool => rvalues::scalar_logical(word as u32 as i32),
        Type::Dbl => rvalues::scalar_real(f64::from_bits(word)),
        Type::I64 => interp::numbers::alloc(word as i64),
        Type::Null => Ok(rvalues::nil()),
        Type::Str => {
            let string = Str {
                lo: out[0],
                hi: out[1],
            };
            let vector = rvalues::alloc(SexpType::String, 1)?;
            interp::heap().set_element(vector, 0, charsxp_of(string)?);
            Ok(vector)
        }
        Type::Vector { item, array } => {
            let list = word as *mut List;
            let length = (*list).length as usize;
            let vector = rvalues::alloc(item_storage(item), length)?;
            let data = (*list).data;
            match item {
                Item::Int | Item::Lgl => {
                    let items = std::slice::from_raw_parts(data as *const i32, length);
                    rvalues::ints_mut(vector).copy_from_slice(items);
                }
                Item::Dbl => {
                    let items = std::slice::from_raw_parts(data as *const f64, length);
                    rvalues::reals_mut(vector).copy_from_slice(items);
                }
                Item::I64 => {
                    let items = std::slice::from_raw_parts(data as *const i64, length);
                    vector.slice_mut::<i64>().copy_from_slice(items);
                }
                Item::Str => {
                    let items = std::slice::from_raw_parts(data as *const Str, length);
                    for (index, &string) in items.iter().enumerate() {
                        interp::heap().set_element(vector, index, charsxp_of(string)?);
                    }
                }
            }
            vector.set_array(array);
            Ok(vector)
        }
        Type::List(item) => {
            let list = word as *mut List;
            let length = (*list).length as usize;
            let vector = rvalues::alloc(SexpType::List, length)?;
            let data = (*list).data;
            for index in 0..length {
                let element = match item {
                    Item::Int => rvalues::scalar_integer(*(data as *const i32).add(index))?,
                    Item::Lgl => rvalues::scalar_logical(*(data as *const i32).add(index))?,
                    Item::Dbl => rvalues::scalar_real(*(data as *const f64).add(index))?,
                    Item::I64 => interp::numbers::alloc(*(data as *const i64).add(index))?,
                    Item::Str => {
                        let string = rvalues::alloc(SexpType::String, 1)?;
                        interp::heap().set_element(
                            string,
                            0,
                            charsxp_of(*(data as *const Str).add(index))?,
                        );
                        string
                    }
                };
                interp::heap().set_element(vector, index, element);
            }
            Ok(vector)
        }
        Type::Dict { .. } => Err(interp::unwind::error(
            "a typed function returned a dictionary",
        )),
    }
}

unsafe fn charsxp_of(string: Str) -> Result<Sexp, Unwind> {
    rvalues::mkchar_bytes(string.bytes(), Encoding::Utf8)
}

/// Drops the native value whose words are `out`.
unsafe fn drop_words(ty: Type, out: [u64; 2]) {
    match ty {
        Type::Str => values::ry_str_drop(out[0], out[1]),
        Type::Vector { .. } | Type::List(_) | Type::Dict { .. } => {
            values::ry_list_drop(out[0] as *mut List)
        }
        _ => {}
    }
}

/// How many more calls deep compiled code may go before it gives up, which leaves the
/// interpreter to report running out of stack as it would.
fn depth_budget() -> i64 {
    let state = interp::interp();
    i64::from(state.max_depth) - state.frames.len() as i64
}

pub(crate) fn logging() -> bool {
    static LOGGING: OnceLock<bool> = OnceLock::new();
    *LOGGING.get_or_init(|| std::env::var_os("RY_TYPED_LOG").is_some())
}
