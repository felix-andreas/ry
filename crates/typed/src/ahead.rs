//! Typed closures compiled when their package is installed, and their code taken up by a later
//! process.
//!
//! An install compiles each typed closure of a package for the argument types it declares
//! ([`Session`]), into the relocatable object that becomes the native section of the package's
//! code image. Nothing runs: the types come from the annotations, and a function whose code
//! would need a fact only a run gives is refused with the reason, and compiles at its first
//! call as a function loaded from source does.
//!
//! A process that maps the image offers each function's code here ([`offer`]). The first call
//! of a closure with the argument types its code was compiled for adopts the code ([`adopt`]):
//! every name the code looks up is resolved again from the closure's environment, and the code
//! is taken only when each finds what the install found. The lookups it found become the
//! code's speculations, so the check every call makes guards install-time code exactly as it
//! guards code compiled in the session.

use {
    crate::{
        Compiled, GUESSES, Key, MAX_REVISIONS, Refusal,
        backend::{self, Callee, Code, Target},
        calls_itself, declared_type, fusion,
        ir::{Function, Item, Type},
        lower::{self, Callees, Speculation},
        ownership, storage,
    },
    heap::object::{Sexp, SexpType},
    interp::{annotations, values as rvalues},
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    ssa::{FuncId, ObjectModule},
    std::{borrow::Cow, cell::RefCell},
};

/// A typed closure compiled for one list of argument types, as a function of the object.
pub struct Emitted {
    pub closure: Sexp,
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
    /// The names the object exports the code and its trampoline under.
    pub function: Box<str>,
    pub trampoline: Box<str>,
    /// The lookups the code, and every function it calls, assumes.
    pub lookups: Vec<Lookup<Sexp>>,
}

/// A name compiled code looks up from its closure's environment, and what the lookup must find.
/// `S` names a sibling: its closure while the package is compiled, its [`Identity`] in a
/// process that mapped the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lookup<S> {
    pub symbol: Sexp,
    pub finds: Finds<S>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finds<S> {
    /// The builtin of that name, or the function base R binds to it.
    Function,
    /// The standard library's constant of that name, read as a value.
    Constant,
    /// A typed closure of the same package, bound in the closure's own environment.
    Sibling(S),
}

/// Which closure of which mapped image a closure is: the image, the record that made the
/// closure and the closure's position in the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub image: u32,
    pub entry: u32,
    pub ordinal: u32,
}

/// Install-time code for a closure, mapped by this process.
#[derive(Clone)]
pub struct Offer {
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
    pub code: Code,
    pub lookups: Vec<Lookup<Identity>>,
}

/// The typed closures of one package being compiled into `object`.
pub struct Session<'a> {
    object: &'a mut ObjectModule,
    /// The package's closures, which are the only ones install-time code may call.
    siblings: FxHashSet<Sexp>,
    functions: Vec<Emitted>,
    ids: Vec<FuncId>,
    /// Each closure and argument types tried: the index of its function, or why it has none.
    tried: FxHashMap<Key, Result<usize, Refusal>>,
    compiling: FxHashSet<Key>,
    /// How many names the object has been given, which numbers the next.
    named: usize,
    /// Why nothing more may go into the object, once a function could not be defined at all.
    broken: Option<ssa::Error>,
}

const SELF_CALL: &str = "it is called while its own code is being compiled";

impl<'a> Session<'a> {
    pub fn new(object: &'a mut ObjectModule, siblings: impl IntoIterator<Item = Sexp>) -> Self {
        Session {
            object,
            siblings: siblings.into_iter().collect(),
            functions: Vec::new(),
            ids: Vec::new(),
            tried: FxHashMap::default(),
            compiling: FxHashSet::default(),
            named: 0,
            broken: None,
        }
    }

    /// Compiles the typed closure `closure` for the argument types it declares, and gives the
    /// index of its function among [`Session::finish`]'s.
    pub unsafe fn compile_declared(&mut self, closure: Sexp) -> Result<usize, Refusal> {
        let params = declared_params(closure)?;
        self.specialize(closure, &params)
    }

    /// The function `index` names.
    pub fn function(&self, index: usize) -> Option<&Emitted> {
        self.functions.get(index)
    }

    /// Every function compiled, or why the object must be discarded.
    pub fn finish(self) -> Result<Vec<Emitted>, ssa::Error> {
        match self.broken {
            Some(error) => Err(error),
            None => Ok(self.functions),
        }
    }

    unsafe fn specialize(&mut self, closure: Sexp, params: &[Type]) -> Result<usize, Refusal> {
        if let Some(ssa::Error(why)) = self.broken {
            return Err(Refusal::new(why));
        }
        let key: Key = (closure, SmallVec::from_slice(params));
        if let Some(tried) = self.tried.get(&key) {
            return tried.clone();
        }
        if !self.compiling.insert(key.clone()) {
            return Err(Refusal::new(SELF_CALL));
        }
        let result = self.compile(closure, params);
        self.compiling.remove(&key);
        self.tried.insert(key, result.clone());
        result
    }

    /// Lowers `closure` for `params` as a first call would, and emits it into the object.
    unsafe fn compile(&mut self, closure: Sexp, params: &[Type]) -> Result<usize, Refusal> {
        let declared = if annotations::returns_i64(closure) {
            Some(Type::I64)
        } else {
            annotations::declared_return_type(closure).and_then(declared_type)
        };
        let guesses: SmallVec<[Type; 5]> = declared.into_iter().chain(GUESSES).collect();
        let mut refusal = Refusal::new("the result type depends on the recursion");
        let mut found = None;
        'guesses: for &first in &guesses {
            let mut guess = first;
            for _ in 0..=MAX_REVISIONS {
                let mut linker = Linker {
                    session: self,
                    env: closure.tag(),
                    callees: Vec::new(),
                    lookups: Vec::new(),
                };
                let lowered = match lower::lower(closure, params, guess, &mut linker) {
                    Ok(lowered) => lowered,
                    Err(why) => {
                        refusal = why;
                        continue 'guesses;
                    }
                };
                let result = lowered.function.result;
                if result == guess || !calls_itself(&lowered.function) {
                    found = Some((lowered, linker.callees, linker.lookups));
                    break 'guesses;
                }
                guess = result;
            }
        }
        let (lowered, callees, mut lookups) = found.ok_or(refusal)?;
        if !lowered.r_calls.is_empty() {
            return Err(Refusal::new(backend::R_CALL_IN_IMAGE));
        }
        if lowered.epoch.is_some() {
            return Err(Refusal::new(
                "a `match` over any tag, whose code tests for the shapes a session has met",
            ));
        }
        let mut ir = lowered.function;
        if !lowered.constructed.is_empty() || ir.locals.iter().any(|&ty| names_a_shape(ty)) {
            return Err(Refusal::new(backend::SHAPES_IN_IMAGE));
        }
        let arguments: SmallVec<[Option<(SexpType, bool)>; 4]> =
            params.iter().map(|&ty| storage(ty)).collect();
        if !annotations::admits_storage(closure, &arguments, storage(ir.result)) {
            return Err(Refusal::new("the types fail the closure's annotations"));
        }
        for speculation in &lowered.speculations {
            let lookup = self.lookup(closure, speculation)?;
            if !lookups.contains(&lookup) {
                lookups.push(lookup);
            }
        }
        fusion::fuse(&mut ir);
        ownership::insert(&mut ir);
        let id = self.emit(&ir, &callees)?;
        self.ids.push(id.0);
        self.functions.push(Emitted {
            closure,
            params: SmallVec::from_slice(params),
            result: ir.result,
            function: id.1,
            trampoline: id.2,
            lookups,
        });
        Ok(self.functions.len() - 1)
    }

    fn emit(
        &mut self,
        function: &Function,
        callees: &[Callee],
    ) -> Result<(FuncId, Box<str>, Box<str>), Refusal> {
        let name = format!("typed{}", self.named);
        let trampoline = format!("typed{}.entry", self.named);
        self.named += 1;
        match backend::compile_into(self.object, function, &name, &trampoline, callees) {
            Ok(Ok(id)) => Ok((id, name.into(), trampoline.into())),
            Ok(Err(refusal)) => Err(refusal),
            Err(error) => {
                self.broken = Some(error);
                Err(Refusal::new(error.0))
            }
        }
    }

    /// What a later process must find where `speculation` found its function, for code of
    /// `closure`: a lookup that starts from the closure's environment and finds a function or
    /// a constant every process of this build has, or a sibling.
    unsafe fn lookup(
        &self,
        closure: Sexp,
        speculation: &Speculation,
    ) -> Result<Lookup<Sexp>, Refusal> {
        let env = closure.tag();
        if speculation.env != env {
            return Err(Refusal::new(
                "a lookup from an environment other than the closure's own",
            ));
        }
        let finds = if !rvalues::is_function(speculation.function) {
            if !is_library_constant(speculation) {
                return Err(Refusal::new("a value read from outside the package"));
            }
            Finds::Constant
        } else if lower::is_base_function(speculation) {
            Finds::Function
        } else if self.siblings.contains(&speculation.function)
            && interp::heap().frame_cell(env, speculation.symbol) == Some(speculation.cell)
        {
            Finds::Sibling(speculation.function)
        } else {
            return Err(Refusal::new(
                "a call of a function from outside the package",
            ));
        };
        Ok(Lookup {
            symbol: speculation.symbol,
            finds,
        })
    }
}

/// The functions of the object one function calls, in the order its calls index them.
struct Linker<'s, 'a> {
    session: &'s mut Session<'a>,
    /// The environment of the function whose calls these are.
    env: Sexp,
    callees: Vec<Callee>,
    lookups: Vec<Lookup<Sexp>>,
}

impl Callees for Linker<'_, '_> {
    unsafe fn callee(&mut self, function: Sexp, params: &[Type]) -> Result<(usize, Type), Refusal> {
        if !self.session.siblings.contains(&function) {
            return Err(Refusal::new(
                "a call of a typed function from outside the package",
            ));
        }
        // A later process checks the callee's lookups from the caller's environment.
        if function.tag() != self.env {
            return Err(Refusal::new(
                "a call of a typed function with another environment",
            ));
        }
        let index = self
            .session
            .specialize(function, params)
            .map_err(|refusal| {
                Refusal(Cow::Owned(format!(
                    "a typed function it calls does not compile at install: {}",
                    refusal.0
                )))
            })?;
        let (Some(emitted), Some(&id)) = (
            self.session.functions.get(index),
            self.session.ids.get(index),
        ) else {
            return Err(Refusal::new("a call of a missing function"));
        };
        // Code that calls another function directly is only right while that function's own
        // lookups hold.
        for lookup in &emitted.lookups {
            if !self.lookups.contains(lookup) {
                self.lookups.push(*lookup);
            }
        }
        self.callees.push(Callee {
            target: Target::Function(id),
            params: SmallVec::from_slice(params),
            result: emitted.result,
        });
        Ok((self.callees.len() - 1, emitted.result))
    }
}

/// The argument types a typed closure declares, which are the types a call from R with values
/// of the declared types gives ([`crate::call`]).
unsafe fn declared_params(closure: Sexp) -> Result<SmallVec<[Type; 4]>, Refusal> {
    let formals = closure.car();
    let body = interp::bytecode::closure_body(closure);
    let (declared, _) = annotations::declared_formals(formals, body);
    let mut params = SmallVec::new();
    for (position, (symbol, _)) in rvalues::pairlist_items(formals).into_iter().enumerate() {
        if symbol == rvalues::symbols().dots {
            return Err(Refusal::new("`...` among the formals"));
        }
        let ty = if annotations::declares_i64(closure, position) {
            Some(Type::I64)
        } else {
            annotations::formal_annotation(formals, body, position)
                .as_deref()
                .and_then(annotated_type)
                .or_else(|| {
                    declared
                        .get(position)
                        .copied()
                        .flatten()
                        .and_then(declared_type)
                })
        };
        params.push(ty.ok_or_else(|| {
            Refusal(Cow::Owned(format!(
                "parameter `{}` declares no type that install-time code is compiled for",
                rvalues::symbol_name(symbol)
            )))
        })?);
    }
    Ok(params)
}

/// The type an annotation's text names, for the annotations whose type the checks' storage
/// masks do not give.
fn annotated_type(annotation: &str) -> Option<Type> {
    let annotation = annotation.trim();
    if let Some(items) = annotation.strip_suffix("[]") {
        return annotated_item(items).map(Type::vector);
    }
    if let Some(items) = generic(annotation, "List") {
        return annotated_item(items).map(|item| Type::Vector { item, array: true });
    }
    if let Some(items) = generic(annotation, "Set") {
        return annotated_item(items).map(Type::Set);
    }
    if let Some(inner) = generic(annotation, "Dict") {
        let (key, value) = inner.split_once(',')?;
        return Some(Type::Dict {
            key: annotated_item(key)?,
            value: annotated_item(value)?,
        });
    }
    annotated_item(annotation).map(Type::of_item)
}

fn generic<'t>(annotation: &'t str, name: &str) -> Option<&'t str> {
    annotation
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix('<')?
        .strip_suffix('>')
}

fn annotated_item(name: &str) -> Option<Item> {
    Some(match name.trim() {
        "I64" => Item::I64,
        "F64" | "double" => Item::Dbl,
        "integer" => Item::Int,
        "Bool" | "logical" => Item::Lgl,
        "Str" | "character" => Item::Str,
        _ => return None,
    })
}

/// Whether `ty` names a shape, whose id only the process that interned it knows.
fn names_a_shape(ty: Type) -> bool {
    let item = |item: Item| matches!(item, Item::Record(_) | Item::Tag(_) | Item::Array(_));
    match ty {
        Type::Record(_) | Type::Tag(_) => true,
        Type::Vector { item: held, .. } | Type::List(held) | Type::Set(held) => item(held),
        Type::Dict { key, value } => item(key) || item(value),
        Type::Int
        | Type::Dbl
        | Type::Lgl
        | Type::Bool
        | Type::Str
        | Type::I64
        | Type::Null
        | Type::Table => false,
    }
}

/// Whether a value lookup found the standard library's own constant of that name. The attached
/// library binds the very object the builtins environment holds, so the object identifies it.
unsafe fn is_library_constant(speculation: &Speculation) -> bool {
    interp::standard_library::builtins_env()
        .and_then(|env| interp::heap().frame_cell(env, speculation.symbol))
        .is_some_and(|cell| cell.cdr() == speculation.function)
}

/// `ty` as an annotation spells it, for the install's report.
pub fn type_name(ty: Type) -> String {
    let item = |item: Item| match item {
        Item::Int => "integer",
        Item::Dbl => "F64",
        Item::Lgl => "logical",
        Item::Str => "Str",
        Item::I64 => "I64",
        Item::Record(_) => "a record",
        Item::Tag(_) => "a tag",
        Item::Array(_) => "a list",
    };
    match ty {
        Type::Int => "integer".to_owned(),
        Type::Dbl => "F64".to_owned(),
        Type::Lgl => "logical".to_owned(),
        Type::Bool => "Bool".to_owned(),
        Type::Str => "Str".to_owned(),
        Type::I64 => "I64".to_owned(),
        Type::Null => "NULL".to_owned(),
        Type::Vector { item: held, array } => {
            if array {
                format!("List<{}>", item(held))
            } else {
                format!("{}[]", item(held))
            }
        }
        Type::List(held) => format!("list[{}]", item(held)),
        Type::Set(held) => format!("Set<{}>", item(held)),
        Type::Dict { key, value } => format!("Dict<{}, {}>", item(key), item(value)),
        Type::Record(_) => "a record".to_owned(),
        Type::Tag(_) => "a tag".to_owned(),
        Type::Table => "DataFrame".to_owned(),
    }
}

/// Writes `ty` for a code image's manifest. `false` for a type that names a shape, which no
/// function of an image has.
pub fn encode_type(ty: Type, out: &mut Vec<u8>) -> bool {
    let item = |item: Item| {
        Some(match item {
            Item::Int => 0u8,
            Item::Dbl => 1,
            Item::Lgl => 2,
            Item::Str => 3,
            Item::I64 => 4,
            Item::Record(_) | Item::Tag(_) | Item::Array(_) => return None,
        })
    };
    let bytes: Option<SmallVec<[u8; 3]>> = (|| {
        Some(match ty {
            Type::Int => SmallVec::from_slice(&[0]),
            Type::Dbl => SmallVec::from_slice(&[1]),
            Type::Lgl => SmallVec::from_slice(&[2]),
            Type::Bool => SmallVec::from_slice(&[3]),
            Type::Str => SmallVec::from_slice(&[4]),
            Type::I64 => SmallVec::from_slice(&[5]),
            Type::Null => SmallVec::from_slice(&[6]),
            Type::Vector { item: held, array } => {
                SmallVec::from_slice(&[7 + u8::from(array), item(held)?])
            }
            Type::List(held) => SmallVec::from_slice(&[9, item(held)?]),
            Type::Set(held) => SmallVec::from_slice(&[10, item(held)?]),
            Type::Dict { key, value } => SmallVec::from_slice(&[11, item(key)?, item(value)?]),
            Type::Table => SmallVec::from_slice(&[12]),
            Type::Record(_) | Type::Tag(_) => return None,
        })
    })();
    match bytes {
        Some(bytes) => {
            out.extend_from_slice(&bytes);
            true
        }
        None => false,
    }
}

/// Reads a type [`encode_type`] wrote from the front of `bytes`, which it advances.
pub fn decode_type(bytes: &mut &[u8]) -> Option<Type> {
    let mut next = || {
        let (&first, rest) = bytes.split_first()?;
        *bytes = rest;
        Some(first)
    };
    let item = |code: u8| {
        Some(match code {
            0 => Item::Int,
            1 => Item::Dbl,
            2 => Item::Lgl,
            3 => Item::Str,
            4 => Item::I64,
            _ => return None,
        })
    };
    Some(match next()? {
        0 => Type::Int,
        1 => Type::Dbl,
        2 => Type::Lgl,
        3 => Type::Bool,
        4 => Type::Str,
        5 => Type::I64,
        6 => Type::Null,
        code @ (7 | 8) => Type::Vector {
            item: item(next()?)?,
            array: code == 8,
        },
        9 => Type::List(item(next()?)?),
        10 => Type::Set(item(next()?)?),
        11 => Type::Dict {
            key: item(next()?)?,
            value: item(next()?)?,
        },
        12 => Type::Table,
        _ => return None,
    })
}

thread_local! {
    /// The install-time code no call has asked for yet, by closure.
    static OFFERS: RefCell<FxHashMap<Sexp, Vec<Offer>>> = RefCell::new(FxHashMap::default());
    /// The closures of mapped images that install-time code names as siblings.
    static IDENTITIES: RefCell<FxHashMap<Sexp, Identity>> = RefCell::new(FxHashMap::default());
}

/// Says which closure of a mapped image `closure` is, so that code which calls it as a sibling
/// can be adopted. The closure is kept alive, because its address names it from here on.
pub unsafe fn identify(closure: Sexp, identity: Identity) {
    IDENTITIES.with_borrow_mut(|identities| {
        if !identities.contains_key(&closure) {
            interp::heap().preserve(closure);
            identities.insert(closure, identity);
        }
    });
}

/// Offers install-time code for `closure`, which its first call with matching argument types
/// adopts. A closure is offered code once.
pub unsafe fn offer(closure: Sexp, offers: Vec<Offer>) {
    OFFERS.with_borrow_mut(|offered| {
        if !offered.contains_key(&closure) {
            interp::heap().preserve(closure);
            offered.insert(closure, offers);
        }
    });
}

/// The install-time code offered for `function` with arguments of types `params`, once every
/// name it looks up finds in this process what the install found. The offer is used up either
/// way: code that is not adopted now is compiled in this session instead.
pub(crate) unsafe fn adopt(function: Sexp, params: &[Type]) -> Option<Compiled> {
    let offer = OFFERS.with_borrow_mut(|offered| {
        let offers = offered.get_mut(&function)?;
        let position = offers.iter().position(|offer| &*offer.params == params)?;
        Some(offers.swap_remove(position))
    })?;
    let env = function.tag();
    let mut speculations = Vec::with_capacity(offer.lookups.len());
    for lookup in &offer.lookups {
        match resolve(env, lookup) {
            Some(speculation) => speculations.push(speculation),
            None => {
                if annotations::typed_logging() {
                    eprintln!(
                        "typed: install-time code for {params:?} is not used: `{}` is no longer \
                         what it was compiled against",
                        rvalues::symbol_name(lookup.symbol)
                    );
                }
                return None;
            }
        }
    }
    Some(Compiled {
        code: offer.code,
        params: offer.params,
        result: offer.result,
        speculations,
        epoch: None,
        calls_r: false,
    })
}

/// What `lookup` finds from `env` in this process, when that is what the install found.
unsafe fn resolve(env: Sexp, lookup: &Lookup<Identity>) -> Option<Speculation> {
    match lookup.finds {
        Finds::Constant => {
            lower::resolve_value(lookup.symbol, env).filter(|found| is_library_constant(found))
        }
        Finds::Function => lower::resolve_function(lookup.symbol, env)
            .filter(|found| lower::is_base_function(found)),
        Finds::Sibling(identity) => lower::resolve_function(lookup.symbol, env).filter(|found| {
            interp::heap().frame_cell(env, lookup.symbol) == Some(found.cell)
                && IDENTITIES
                    .with_borrow(|identities| identities.get(&found.function) == Some(&identity))
        }),
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        syntax::testing::{SplitMix64, for_each_seed},
    };

    const ITEMS: [Item; 5] = [Item::Int, Item::Dbl, Item::Lgl, Item::Str, Item::I64];

    fn random_type(rng: &mut SplitMix64) -> Type {
        let mut item = || ITEMS[rng.below(ITEMS.len())];
        let (first, second) = (item(), item());
        match rng.below(13) {
            0 => Type::Int,
            1 => Type::Dbl,
            2 => Type::Lgl,
            3 => Type::Bool,
            4 => Type::Str,
            5 => Type::I64,
            6 => Type::Null,
            7 => Type::vector(first),
            8 => Type::Vector {
                item: first,
                array: true,
            },
            9 => Type::List(first),
            10 => Type::Set(first),
            11 => Type::Dict {
                key: first,
                value: second,
            },
            _ => Type::Table,
        }
    }

    /// A type reads back as written, a type that names a shape is not written, and any bytes
    /// give a type or nothing, never a panic.
    #[test]
    fn fuzz_types_round_trip_and_never_panic() {
        for_each_seed(2000, |seed| {
            let mut rng = SplitMix64(seed);
            let ty = random_type(&mut rng);
            let mut bytes = Vec::new();
            assert!(encode_type(ty, &mut bytes));
            let mut rest = &bytes[..];
            assert_eq!(decode_type(&mut rest), Some(ty));
            assert!(rest.is_empty());
            let damaged: Vec<u8> = (0..rng.below(4)).map(|_| rng.next_u64() as u8).collect();
            let mut rest = &damaged[..];
            if let Some(decoded) = decode_type(&mut rest) {
                let mut again = Vec::new();
                assert!(encode_type(decoded, &mut again));
                assert_eq!(&damaged[..damaged.len() - rest.len()], &again[..]);
            }
        });
        let mut bytes = Vec::new();
        assert!(!encode_type(Type::Record(0), &mut bytes));
        assert!(!encode_type(Type::List(Item::Tag(0)), &mut bytes));
        assert!(bytes.is_empty());
    }
}
