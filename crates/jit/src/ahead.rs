//! Install-time machine code: the native functions of a package compiled when it is installed,
//! from facts that need no run, and written into its code image (`interp::code_image`).
//!
//! An install compiles each closure of the package's namespace as a native function
//! ([`crate::native`]) for a few guesses at its argument types. The guesses come from the
//! closure's own annotations and constant defaults, and otherwise from the scalar and vector
//! types native code takes; each guess is tested where the call enters, and a call whose
//! arguments match none runs in the interpreter, so a wrong guess costs one comparison and never
//! a deoptimization. Calls between the package's own native functions are direct calls inside
//! the code, and a function inlines its small siblings.
//!
//! Every function the code calls by name is a check the code needs: a base builtin such as `+`
//! must still be that builtin, and a sibling must still be the closure the code was compiled
//! from. A call checks them once its namespace is locked, and the result holds until a binding
//! is unlocked anywhere or a closure is flagged for debugging ([`interp::code_image::check_epoch`]),
//! because a locked binding cannot
//! change and unlocking is the only way to change one (`unlockBinding`, `assignInNamespace`).
//! A sibling still waiting in its lazy-load promise passes: the image matched its database
//! record by record, so the promise can only make that closure.
//!
//! The manifest that names each function's closure, argument types and checks travels in the
//! native section's metadata. Its layout, integers little-endian:
//!
//! | Field | Content |
//! |---|---|
//! | count | `u32`, then per function: |
//! | closure | record entry `u32` and ordinal `u32` |
//! | types | parameter count `u8`, a type code per parameter, then the result's type code |
//! | entry | the exported entry's name, a `u16` length and the bytes |
//! | checks | `u32` count, then per check the name looked up (`u16` length and bytes) and either `0` and the builtin's name, or `1`, an entry `u32` and an ordinal `u32` |
//!
//! Type codes are 0 integer, 1 double, 2 logical, 3 integer vector, 4 double vector and 5
//! logical vector.

use {
    crate::{
        analysis::{Speculation, Ty, base_builtin},
        native::{self, Native},
        runtime,
    },
    heap::object::{Sexp, SexpType, gp},
    interp::{
        annotations,
        builtins::Arg,
        bytecode,
        code_image::{NativeBuild, NativeCandidate, NativeHooks},
        unwind::R,
        values,
    },
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    ssa::{
        AbiParam, Builder, CallConv, MemFlagsData, ObjectModule, Signature, section,
        types::I64,
    },
    std::{cell::Cell, cell::RefCell, path::Path},
};

/// Gives `interp` the hooks it calls to compile, load and call install-time machine code.
pub fn install() {
    interp::code_image::set_native_hooks(NativeHooks {
        compile,
        load,
        attach,
        call,
    });
}

/// The most argument types one closure is compiled for.
const MAX_SPECIALIZATIONS: usize = 4;
/// The most argument type guesses tried per closure.
const MAX_ATTEMPTS: usize = 24;
/// The most parameters a closure may have to be guessed for.
const MAX_GUESSED_PARAMETERS: usize = 4;
/// The types a parameter without a declared type is guessed as, likeliest first.
const GUESSES: [Ty; 6] = [Ty::Dbl, Ty::Int, Ty::DblVec, Ty::IntVec, Ty::Lgl, Ty::LglVec];

/// A function name the code looks up, and what the lookup must find.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Check {
    symbol: Box<str>,
    expected: Expected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Expected {
    /// The base builtin of this name.
    Builtin(Box<str>),
    /// The package's closure made by this record entry at this ordinal.
    Sibling { entry: u32, ordinal: u32 },
}

/// One native function of the manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Specialization {
    entry: u32,
    ordinal: u32,
    params: SmallVec<[Ty; 4]>,
    result: Ty,
    function: Box<str>,
    checks: Vec<Check>,
}

/// How a closure fared, for the install's report.
struct Outcome {
    name: String,
    size: usize,
    compiled: Vec<Specialization>,
    refusal: &'static str,
}

/// Compiles what it can of a package's closures, for [`NativeHooks::compile`].
unsafe fn compile(candidates: &[NativeCandidate], explain: bool) -> Option<NativeBuild> {
    let object = match ObjectModule::new() {
        Ok(object) => object,
        Err(ssa::Error(why)) => {
            if explain {
                println!("machine code: none ({why})");
            }
            return None;
        }
    };
    let identities: FxHashMap<Sexp, (u32, u32)> = candidates
        .iter()
        .map(|candidate| (candidate.closure, (candidate.entry, candidate.ordinal)))
        .collect();
    let names = binding_names(candidates);
    let (object, outcomes) = native::ahead_of_time(object, || {
        let mut outcomes = Vec::with_capacity(candidates.len());
        let mut count = 0usize;
        for candidate in candidates {
            outcomes.push(compile_closure(candidate, &identities, &names, &mut count));
        }
        outcomes
    });
    if explain {
        report(&outcomes);
    }
    let specializations: Vec<Specialization> = outcomes
        .into_iter()
        .flat_map(|outcome| outcome.compiled)
        .collect();
    if specializations.is_empty() {
        return None;
    }
    let mut code = match object?.finish(&|name| native::helper_address(name).is_some()) {
        Ok(code) => code,
        Err(ssa::Error(why)) => {
            if explain {
                println!("machine code: none (linking failed: {why})");
            }
            return None;
        }
    };
    code.metadata = encode_manifest(&specializations);
    Some(NativeBuild {
        functions: specializations.len(),
        encode: Box::new(move |base| section::encode(&code, base)),
    })
}

/// The name each candidate is bound to in its namespace, for the report.
unsafe fn binding_names(candidates: &[NativeCandidate]) -> FxHashMap<Sexp, String> {
    let mut names = FxHashMap::default();
    let mut seen = Vec::new();
    for candidate in candidates {
        let env = candidate.closure.tag();
        if env.kind() != SexpType::Environment || seen.contains(&env) {
            continue;
        }
        seen.push(env);
        for symbol in interp::envir::env_symbols(env, true, true) {
            if let Some(cell) = interp::heap().frame_cell(env, symbol) {
                let mut value = cell.cdr();
                if value.kind() == SexpType::Promise && value.car() != values::unbound() {
                    value = value.car();
                }
                if value.kind() == SexpType::Closure {
                    names
                        .entry(value)
                        .or_insert_with(|| values::symbol_name(symbol).to_string());
                }
            }
        }
    }
    names
}

unsafe fn compile_closure(
    candidate: &NativeCandidate,
    identities: &FxHashMap<Sexp, (u32, u32)>,
    names: &FxHashMap<Sexp, String>,
    count: &mut usize,
) -> Outcome {
    let closure = candidate.closure;
    let mut outcome = Outcome {
        name: names
            .get(&closure)
            .cloned()
            .unwrap_or_else(|| format!("<closure {}:{}>", candidate.entry, candidate.ordinal)),
        size: body_size(bytecode::closure_body(closure)),
        compiled: Vec::new(),
        refusal: "no argument types fit",
    };
    let guesses = match guesses(closure) {
        Ok(guesses) => guesses,
        Err(why) => {
            outcome.refusal = why;
            return outcome;
        }
    };
    for params in guesses {
        if outcome.compiled.len() == MAX_SPECIALIZATIONS {
            break;
        }
        let Some(native) = native::native_for(closure, &params) else {
            if let Some(why) = native::last_refusal() {
                outcome.refusal = why;
            }
            continue;
        };
        let checks = match checks(closure, native, identities) {
            Ok(checks) => checks,
            Err(why) => {
                outcome.refusal = why;
                continue;
            }
        };
        let Some(id) = native.object else {
            continue;
        };
        let function = format!("entry{count}");
        if let Err(crate::analysis::Refusal(why)) = define_entry(&function, id, native) {
            outcome.refusal = why;
            continue;
        }
        *count += 1;
        outcome.compiled.push(Specialization {
            entry: candidate.entry,
            ordinal: candidate.ordinal,
            params: native.params.clone(),
            result: native.result,
            function: function.into(),
            checks,
        });
    }
    outcome
}

/// The argument types to try for `closure`, likeliest first: a declared or constant-default
/// type where the closure gives one, and otherwise each of [`GUESSES`].
unsafe fn guesses(closure: Sexp) -> Result<Vec<SmallVec<[Ty; 4]>>, &'static str> {
    let formals = values::pairlist_items(closure.car());
    if formals
        .iter()
        .any(|&(symbol, _)| symbol == values::symbols().dots)
    {
        return Err("`...` among the formals");
    }
    let (declared, _) = annotations::declared_formals(closure.car(), closure.cdr());
    let mut choices: Vec<SmallVec<[Ty; 6]>> = Vec::with_capacity(formals.len());
    for (index, &(_, default)) in formals.iter().enumerate() {
        let known = declared
            .get(index)
            .copied()
            .flatten()
            .and_then(|declared| declared_type(declared.kind, declared.scalar))
            .or_else(|| default_type(default));
        choices.push(match known {
            Some(ty) => SmallVec::from_slice(&[ty]),
            None => SmallVec::from_slice(&GUESSES),
        });
    }
    if choices.iter().filter(|choice| choice.len() > 1).count() > MAX_GUESSED_PARAMETERS {
        return Err("more parameters than the install guesses types for");
    }
    // Every combination, ordered by how far it strays from each parameter's likeliest type.
    let mut combinations: Vec<(usize, SmallVec<[usize; 4]>)> = vec![(0, SmallVec::new())];
    for choice in &choices {
        let mut next = Vec::with_capacity(combinations.len() * choice.len());
        for (cost, indices) in &combinations {
            for index in 0..choice.len() {
                let mut indices = indices.clone();
                indices.push(index);
                next.push((cost + index, indices));
            }
        }
        combinations = next;
    }
    combinations.sort();
    Ok(combinations
        .into_iter()
        .take(MAX_ATTEMPTS)
        .map(|(_, indices)| {
            indices
                .iter()
                .zip(&choices)
                .filter_map(|(&index, choice)| choice.get(index).copied())
                .collect()
        })
        .collect())
}

fn declared_type(kind: SexpType, scalar: bool) -> Option<Ty> {
    Some(match (kind, scalar) {
        (SexpType::Integer, true) => Ty::Int,
        (SexpType::Real, true) => Ty::Dbl,
        (SexpType::Logical, true) => Ty::Lgl,
        (SexpType::Integer, false) => Ty::IntVec,
        (SexpType::Real, false) => Ty::DblVec,
        (SexpType::Logical, false) => Ty::LglVec,
        _ => return None,
    })
}

unsafe fn default_type(default: Sexp) -> Option<Ty> {
    if default.attrib() != values::nil() || values::length(default) != 1 {
        return None;
    }
    match default.kind() {
        SexpType::Integer => Some(Ty::Int),
        SexpType::Real => Some(Ty::Dbl),
        SexpType::Logical => Some(Ty::Lgl),
        _ => None,
    }
}

/// The lookups `native` assumes, as checks a later process can make: each must start from the
/// closure's own environment and find a base builtin, or a sibling bound in that environment.
unsafe fn checks(
    closure: Sexp,
    native: &Native,
    identities: &FxHashMap<Sexp, (u32, u32)>,
) -> Result<Vec<Check>, &'static str> {
    let env = closure.tag();
    let mut checks: Vec<Check> = Vec::new();
    for Speculation {
        symbol,
        env: from,
        cell,
        function,
    } in &native.speculations
    {
        if *from != env {
            return Err("a lookup from an environment other than the namespace");
        }
        let expected = if let Some(name) = base_builtin(*function, *symbol) {
            Expected::Builtin(name.into())
        } else if let Some(&(entry, ordinal)) = identities.get(function)
            && interp::heap().frame_cell(env, *symbol) == Some(*cell)
        {
            Expected::Sibling { entry, ordinal }
        } else {
            return Err("a call of a function from outside the package");
        };
        let name = values::symbol_name(*symbol);
        // The manifest stores a name's length in 16 bits.
        if name.len() > usize::from(u16::MAX) {
            return Err("a name longer than 65,535 bytes");
        }
        let check = Check {
            symbol: name.as_ref().into(),
            expected,
        };
        if !checks.contains(&check) {
            checks.push(check);
        }
    }
    Ok(checks)
}

/// Defines the exported entry `name` of `native`, which takes the call budget, a pointer to
/// the arguments as 64-bit words and a pointer to store the value's bits, and returns the
/// status: the shape [`Entry`] calls, whatever the argument types.
fn define_entry(
    name: &str,
    id: ssa::FuncId,
    native: &Native,
) -> Result<(), crate::analysis::Refusal> {
    let mut outer = Signature::new(CallConv::Host);
    outer.params.extend([AbiParam::new(I64); 3]);
    outer.returns.push(AbiParam::new(I64));
    let entry_id = native::declare_ahead(name, &outer)?;
    let mut builder = Builder::new(outer);
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    let &[budget, words, out] = builder.block_params(block) else {
        return Err(crate::analysis::Refusal("an entry without its parameters"));
    };
    let mut arguments: SmallVec<[ssa::Value; 5]> = SmallVec::new();
    arguments.push(budget);
    for (index, &ty) in native.params.iter().enumerate() {
        arguments.push(builder.ins().load(
            native::machine_type(ty),
            MemFlagsData::trusted(),
            words,
            8 * index as i32,
        ));
    }
    let inner = builder.import_function(
        id,
        native::signature(&native.params, native.result, CallConv::Host),
    );
    let call = builder.ins().call(inner, &arguments);
    let &[status, value] = builder.inst_results(call) else {
        return Err(crate::analysis::Refusal("a native call without a status and a value"));
    };
    builder.ins().store(MemFlagsData::trusted(), value, out, 0);
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
    native::define_ahead(entry_id, &builder.finalize())
}

/// How many calls and operators a body holds, which ranks the report.
unsafe fn body_size(body: Sexp) -> usize {
    if body.kind() != SexpType::Language {
        return 0;
    }
    1 + values::pairlist_items(body.cdr())
        .iter()
        .map(|&(_, argument)| body_size(argument))
        .sum::<usize>()
}

/// Prints what kept each closure in the interpreter, largest first, and then what each native
/// function checks: the names it looks up when first called and the arguments it tests on every
/// call.
fn report(outcomes: &[Outcome]) {
    let compiled = outcomes
        .iter()
        .filter(|outcome| !outcome.compiled.is_empty())
        .count();
    println!(
        "machine code: {compiled} of {} functions, by size (calls and operators in the body)",
        outcomes.len()
    );
    let mut order: Vec<&Outcome> = outcomes.iter().collect();
    order.sort_by(|a, b| {
        (!a.compiled.is_empty(), std::cmp::Reverse(a.size), &a.name).cmp(&(
            !b.compiled.is_empty(),
            std::cmp::Reverse(b.size),
            &b.name,
        ))
    });
    for outcome in order {
        if outcome.compiled.is_empty() {
            println!(
                "  interpreter {:>5}  {}: {}",
                outcome.size, outcome.name, outcome.refusal
            );
            continue;
        }
        for specialization in &outcome.compiled {
            let types: Vec<&str> = specialization.params.iter().map(|&ty| ty_name(ty)).collect();
            let lookups: Vec<&str> = specialization
                .checks
                .iter()
                .map(|check| &*check.symbol)
                .collect();
            println!(
                "  native      {:>5}  {}({}) -> {}: tests {} argument{} on entry; looks up {} once",
                outcome.size,
                outcome.name,
                types.join(", "),
                ty_name(specialization.result),
                types.len(),
                if types.len() == 1 { "" } else { "s" },
                if lookups.is_empty() {
                    "nothing".to_owned()
                } else {
                    lookups.join(", ")
                },
            );
        }
    }
}

fn ty_name(ty: Ty) -> &'static str {
    match ty {
        Ty::Int => "integer",
        Ty::Dbl => "double",
        Ty::Lgl => "logical",
        Ty::IntVec => "integer[]",
        Ty::DblVec => "double[]",
        Ty::LglVec => "logical[]",
        _ => "other",
    }
}

fn ty_code(ty: Ty) -> u8 {
    match ty {
        Ty::Int => 0,
        Ty::Dbl => 1,
        Ty::Lgl => 2,
        Ty::IntVec => 3,
        Ty::DblVec => 4,
        Ty::LglVec => 5,
        _ => u8::MAX,
    }
}

fn ty_of(code: u8) -> Option<Ty> {
    Some(match code {
        0 => Ty::Int,
        1 => Ty::Dbl,
        2 => Ty::Lgl,
        3 => Ty::IntVec,
        4 => Ty::DblVec,
        5 => Ty::LglVec,
        _ => return None,
    })
}

fn encode_manifest(specializations: &[Specialization]) -> Vec<u8> {
    let mut out = Vec::new();
    let name = |out: &mut Vec<u8>, name: &str| {
        let bytes = &name.as_bytes()[..name.len().min(u16::MAX as usize)];
        out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(bytes);
    };
    out.extend_from_slice(&(specializations.len() as u32).to_le_bytes());
    for specialization in specializations {
        out.extend_from_slice(&specialization.entry.to_le_bytes());
        out.extend_from_slice(&specialization.ordinal.to_le_bytes());
        out.push(specialization.params.len() as u8);
        out.extend(specialization.params.iter().map(|&ty| ty_code(ty)));
        out.push(ty_code(specialization.result));
        name(&mut out, &specialization.function);
        out.extend_from_slice(&(specialization.checks.len() as u32).to_le_bytes());
        for check in &specialization.checks {
            name(&mut out, &check.symbol);
            match &check.expected {
                Expected::Builtin(builtin) => {
                    out.push(0);
                    name(&mut out, builtin);
                }
                Expected::Sibling { entry, ordinal } => {
                    out.push(1);
                    out.extend_from_slice(&entry.to_le_bytes());
                    out.extend_from_slice(&ordinal.to_le_bytes());
                }
            }
        }
    }
    out
}

/// Reads a manifest. Any bytes give a manifest or an error, never a read out of bounds.
fn decode_manifest(bytes: &[u8]) -> Result<Vec<Specialization>, &'static str> {
    let mut input = Input { bytes, position: 0 };
    let count = input.u32()? as usize;
    if count > bytes.len() / 12 {
        return Err("the manifest names more functions than it holds");
    }
    let mut specializations = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = input.u32()?;
        let ordinal = input.u32()?;
        let arity = input.u8()?;
        let mut params = SmallVec::new();
        for _ in 0..arity {
            params.push(ty_of(input.u8()?).ok_or("an unknown argument type")?);
        }
        let result = ty_of(input.u8()?).ok_or("an unknown result type")?;
        if !result.is_unboxed() {
            return Err("a result that is not a scalar");
        }
        let function = input.name()?.into();
        let check_count = input.u32()? as usize;
        if check_count > bytes.len() / 3 {
            return Err("the manifest names more checks than it holds");
        }
        let mut checks = Vec::with_capacity(check_count);
        for _ in 0..check_count {
            let symbol = input.name()?.into();
            let expected = match input.u8()? {
                0 => Expected::Builtin(input.name()?.into()),
                1 => Expected::Sibling {
                    entry: input.u32()?,
                    ordinal: input.u32()?,
                },
                _ => return Err("an unknown kind of check"),
            };
            checks.push(Check { symbol, expected });
        }
        specializations.push(Specialization {
            entry,
            ordinal,
            params,
            result,
            function,
            checks,
        });
    }
    if input.position != bytes.len() {
        return Err("the manifest holds more than its functions");
    }
    Ok(specializations)
}

struct Input<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Input<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], &'static str> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len())
            .ok_or("the manifest ends early")?;
        let taken = &self.bytes[self.position..end];
        self.position = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn name(&mut self) -> Result<&'a str, &'static str> {
        let length = self.take(2)?;
        let length = u16::from_le_bytes([length[0], length[1]]) as usize;
        std::str::from_utf8(self.take(length)?).map_err(|_| "a name that is not UTF-8")
    }
}

/// Calls a native entry: the call budget, the arguments as words, where to store the value.
type Entry = unsafe extern "C" fn(i64, *const u64, *mut u64) -> i64;

/// A native function a process mapped, with its checks resolved to symbols.
struct Mapped {
    params: SmallVec<[Ty; 4]>,
    result: Ty,
    entry: Entry,
    checks: Vec<(Sexp, Expected)>,
}

/// A mapped section: its functions by record entry and ordinal.
struct Section {
    functions: FxHashMap<(u32, u32), SmallVec<[Mapped; 2]>>,
}

/// A closure with native code: where its code lies, per function the [`check_epoch`] plus one
/// at which its checks last passed (0 for never), and whether `RY_JIT_LOG` has reported a call
/// that ran it.
///
/// [`check_epoch`]: interp::code_image::check_epoch
struct Attached {
    section: u32,
    entry: u32,
    ordinal: u32,
    checked: SmallVec<[Cell<u64>; 2]>,
    reported: Cell<bool>,
}

thread_local! {
    static SECTIONS: RefCell<Vec<Section>> = const { RefCell::new(Vec::new()) };
    /// Each closure with native code, preserved so that its address names it for good.
    static ATTACHED: RefCell<FxHashMap<Sexp, Attached>> = RefCell::new(FxHashMap::default());
}

/// Maps a native section, for [`NativeHooks::load`].
unsafe fn load(path: &Path, offset: u64, bytes: &[u8]) -> Result<Option<u32>, String> {
    // SAFETY: the resolver gives each helper's address in this build, which is the build the
    // image is stamped with.
    let loaded = match section::load(path, offset, bytes, &native::helper_address) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return Ok(None),
        Err(ssa::Error(why)) => return Err(why.to_owned()),
    };
    let manifest = decode_manifest(&loaded.metadata).map_err(str::to_owned)?;
    if crate::logging() {
        eprintln!(
            "jit: mapped {} install-time functions from {}",
            manifest.len(),
            path.display()
        );
    }
    let mut functions: FxHashMap<(u32, u32), SmallVec<[Mapped; 2]>> = FxHashMap::default();
    for specialization in manifest {
        let address = *loaded
            .functions
            .get(&specialization.function)
            .ok_or("the manifest names a function the code lacks")?;
        let mut checks = Vec::with_capacity(specialization.checks.len());
        for check in specialization.checks {
            let symbol = values::install(&check.symbol)
                .map_err(|_| "cannot intern a name the code looks up".to_owned())?;
            checks.push((symbol, check.expected));
        }
        functions
            .entry((specialization.entry, specialization.ordinal))
            .or_default()
            .push(Mapped {
                params: specialization.params,
                result: specialization.result,
                // SAFETY: every exported entry has exactly this signature.
                entry: std::mem::transmute::<usize, Entry>(address),
                checks,
            });
    }
    SECTIONS.with_borrow_mut(|sections| {
        sections.push(Section { functions });
        Ok(Some(sections.len() as u32 - 1))
    })
}

/// Marks the closures of a record that have native code, for [`NativeHooks::attach`].
unsafe fn attach(section: u32, entry: u32, closures: &[Sexp]) {
    let counts: Vec<(usize, usize)> = SECTIONS.with_borrow(|sections| {
        let Some(found) = sections.get(section as usize) else {
            return Vec::new();
        };
        closures
            .iter()
            .enumerate()
            .filter_map(|(ordinal, _)| {
                found
                    .functions
                    .get(&(entry, ordinal as u32))
                    .map(|functions| (ordinal, functions.len()))
            })
            .collect()
    });
    for (ordinal, count) in counts {
        let Some(&closure) = closures.get(ordinal) else {
            continue;
        };
        if closure.kind() != SexpType::Closure {
            continue;
        }
        ATTACHED.with_borrow_mut(|attached| {
            if attached.contains_key(&closure) {
                return;
            }
            interp::heap().preserve(closure);
            closure.set_gp_bits(gp::NATIVE_ENTRY, true);
            attached.insert(
                closure,
                Attached {
                    section,
                    entry,
                    ordinal: ordinal as u32,
                    checked: (0..count).map(|_| Cell::new(0)).collect(),
                    reported: Cell::new(false),
                },
            );
        });
    }
}

/// Calls `function` through its native code, for [`NativeHooks::call`].
unsafe fn call(function: Sexp, args: &[Arg]) -> Option<R<Sexp>> {
    let (section, entry, ordinal) = ATTACHED.with_borrow(|attached| {
        attached
            .get(&function)
            .map(|found| (found.section, found.entry, found.ordinal))
    })?;
    native::with_released_roots(|| call_attached(function, args, section, entry, ordinal))
}

unsafe fn call_attached(
    function: Sexp,
    args: &[Arg],
    section: u32,
    entry: u32,
    ordinal: u32,
) -> Option<R<Sexp>> {
    let (arguments, read) = native::gather(function, args)?;
    let (types, words) = native::argument_words(&arguments)?;
    let (index, result, entry_point) = SECTIONS.with_borrow(|sections| {
        let functions = sections.get(section as usize)?.functions.get(&(entry, ordinal))?;
        functions
            .iter()
            .enumerate()
            .find(|(_, mapped)| mapped.params == types)
            .map(|(index, mapped)| (index, mapped.result, mapped.entry))
    })?;
    if !checks_hold(function, section, entry, ordinal, index) {
        return None;
    }
    let value = native::run(result, &read, |out| {
        entry_point(runtime::jit_depth_budget(), words.as_ptr(), out)
    })?;
    if crate::logging() {
        let first = ATTACHED.with_borrow(|attached| {
            attached
                .get(&function)
                .is_some_and(|found| !found.reported.replace(true))
        });
        if first {
            eprintln!("jit: ran install-time function {entry}:{ordinal} for {types:?}");
        }
    }
    Some(value)
}

/// Whether every name function `index` of `function`'s native code looks up still finds what
/// the code was compiled against. A pass is remembered until [`interp::code_image::check_epoch`]
/// moves, once every binding it read and every frame a lookup walked is locked, since no binding
/// can then appear in front of the one found.
unsafe fn checks_hold(function: Sexp, section: u32, entry: u32, ordinal: u32, index: usize) -> bool {
    let stamp = interp::code_image::check_epoch() + 1;
    let remembered = ATTACHED.with_borrow(|attached| {
        attached
            .get(&function)
            .and_then(|found| found.checked.get(index))
            .is_some_and(|checked| checked.get() == stamp)
    });
    if remembered {
        return true;
    }
    let env = function.tag();
    let Some(checks) = SECTIONS.with_borrow(|sections| {
        sections
            .get(section as usize)
            .and_then(|found| found.functions.get(&(entry, ordinal)))
            .and_then(|functions| functions.get(index))
            .map(|mapped| mapped.checks.clone())
    }) else {
        return false;
    };
    let mut all_locked = true;
    for (symbol, expected) in &checks {
        match lookup(env, *symbol) {
            Some(Found {
                value,
                locked,
                in_first_frame,
                frames_locked,
            }) => {
                all_locked &= locked && frames_locked;
                let holds = match expected {
                    Expected::Builtin(name) => value
                        .is_some_and(|value| base_builtin(value, *symbol) == Some(&**name)),
                    Expected::Sibling {
                        entry: sibling_entry,
                        ordinal: sibling_ordinal,
                    } => {
                        in_first_frame
                            && match value {
                                // A promise the namespace still waits on makes the closure its
                                // record holds, which is the sibling, but only while the binding
                                // is locked: an unlocked one may hold a promise of anything.
                                None => locked,
                                // Native code calls the sibling directly, so a sibling flagged
                                // for the debugger or a trace must be called through R.
                                Some(value) => {
                                    !value.has_gp(
                                        gp::CLOSURE_DEBUG | gp::CLOSURE_STEP | gp::CLOSURE_TRACE,
                                    ) && ATTACHED.with_borrow(|attached| {
                                        attached.get(&value).is_some_and(|found| {
                                            found.section == section
                                                && found.entry == *sibling_entry
                                                && found.ordinal == *sibling_ordinal
                                        })
                                    })
                                }
                            }
                    }
                };
                if !holds {
                    return false;
                }
            }
            None => return false,
        }
    }
    if all_locked {
        ATTACHED.with_borrow(|attached| {
            if let Some(checked) = attached
                .get(&function)
                .and_then(|found| found.checked.get(index))
            {
                checked.set(stamp);
            }
        });
    }
    true
}

/// What a lookup found: the value (`None` for a promise not forced yet), whether its binding is
/// locked, whether it is bound in the environment the lookup started from, and whether every
/// frame the lookup walked, the one it found the binding in included, is locked.
struct Found {
    value: Option<Sexp>,
    locked: bool,
    in_first_frame: bool,
    frames_locked: bool,
}

/// The binding a variable lookup of `symbol` from `start` finds, without forcing or calling
/// anything. `None` when it finds none or an active binding.
unsafe fn lookup(start: Sexp, symbol: Sexp) -> Option<Found> {
    let heap = interp::heap();
    let empty = heap.known().empty_env;
    let mut env = start;
    let mut frames_locked = true;
    while env != empty {
        frames_locked &= env.has_gp(gp::FRAME_LOCKED);
        if let Some(cell) = heap.frame_cell(env, symbol) {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                return None;
            }
            let mut value = Some(cell.cdr());
            if let Some(promise) = value
                && promise.kind() == SexpType::Promise
            {
                value = (promise.car() != values::unbound()).then(|| promise.car());
            }
            return Some(Found {
                value,
                locked: cell.has_gp(gp::BINDING_LOCKED),
                in_first_frame: env == start,
                frames_locked,
            });
        }
        env = env.enclosure();
    }
    None
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        syntax::testing::{SplitMix64, for_each_seed},
    };

    const TYPES: [Ty; 6] = [Ty::Int, Ty::Dbl, Ty::Lgl, Ty::IntVec, Ty::DblVec, Ty::LglVec];

    fn random_name(rng: &mut SplitMix64) -> Box<str> {
        let names = ["+", "f", "my var", "[[", "\u{e9}t\u{e9}", ""];
        names[rng.below(names.len())].into()
    }

    fn random_manifest(rng: &mut SplitMix64) -> Vec<Specialization> {
        (0..rng.below(4))
            .map(|_| Specialization {
                entry: rng.next_u64() as u32,
                ordinal: rng.below(8) as u32,
                params: (0..rng.below(5))
                    .map(|_| TYPES[rng.below(TYPES.len())])
                    .collect(),
                result: TYPES[rng.below(3)],
                function: format!("entry{}", rng.below(100)).into(),
                checks: (0..rng.below(4))
                    .map(|_| Check {
                        symbol: random_name(rng),
                        expected: if rng.chance(1, 2) {
                            Expected::Builtin(random_name(rng))
                        } else {
                            Expected::Sibling {
                                entry: rng.next_u64() as u32,
                                ordinal: rng.below(8) as u32,
                            }
                        },
                    })
                    .collect(),
            })
            .collect()
    }

    /// A manifest reads back as written, and damaged bytes give a manifest or an error, never a
    /// panic or a read out of bounds.
    #[test]
    fn fuzz_manifest_round_trips_and_never_panics() {
        for_each_seed(2000, |seed| {
            let mut rng = SplitMix64(seed);
            let manifest = random_manifest(&mut rng);
            let mut bytes = encode_manifest(&manifest);
            assert_eq!(decode_manifest(&bytes), Ok(manifest));
            for _ in 0..1 + rng.below(6) {
                if bytes.is_empty() {
                    break;
                }
                match rng.below(4) {
                    0 => {
                        let index = rng.below(bytes.len());
                        bytes[index] = rng.next_u64() as u8;
                    }
                    1 => bytes.truncate(rng.below(bytes.len())),
                    2 => bytes.push(rng.next_u64() as u8),
                    _ => {
                        let index = rng.below(bytes.len());
                        bytes[index] ^= 0x80;
                    }
                }
            }
            if let Ok(decoded) = decode_manifest(&bytes) {
                for specialization in &decoded {
                    assert!(specialization.result.is_unboxed());
                }
                assert_eq!(decode_manifest(&encode_manifest(&decoded)), Ok(decoded));
            }
        });
    }
}
