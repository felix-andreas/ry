//! The one lowering from the IR to machine code, through Cranelift, and the code module every
//! compiled function lives in.

pub use cranelift_module::FuncId;
use {
    super::{
        Binary, BinaryImm, CallConv, Convert, FloatCC, Function, IntCC, Load, MemFlagsData, Op,
        Overflow, Signature, Type, Unary, Value,
    },
    cranelift_codegen::{
        Context, FinalizedMachReloc, FinalizedRelocTarget,
        binemit::Reloc,
        control::ControlPlane,
        ir::{
            self as clif, BlockArg, ExternalName, InstBuilder, UserExternalName, UserFuncName,
            condcodes::{FloatCC as ClifFloatCC, IntCC as ClifIntCC},
            immediates::{Ieee64, Imm64},
        },
        isa::{CallConv as ClifCallConv, OwnedTargetIsa, TargetIsa},
        settings::{self, Configurable},
    },
    cranelift_frontend::{FunctionBuilder, FunctionBuilderContext},
    cranelift_jit::{JITBuilder, JITModule},
    cranelift_module::{Module as _, default_libcall_names},
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::{
        cell::RefCell,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    },
};

/// Why the backend could not compile a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);

/// What the typed core compiles into: the JIT [`Module`] for code a session runs at once, or a
/// [`Relocatable`] object for code written to a file.
pub trait Target {
    /// Declares a function with `signature`, so bodies can call it before it is defined.
    fn declare(&mut self, signature: &Signature) -> Result<FuncId, Error>;
    /// Gives `id` its body. The JIT module returns the size of the machine code; a relocatable
    /// object compiles at [`Relocatable::finish`] and returns 0.
    fn define(&mut self, id: FuncId, function: &Function) -> Result<u32, Error>;
}

/// Where compiled functions live. Code is never freed: the backend can release a module's code
/// only all at once, and a discarded compilation's code stays mapped until the process exits.
pub struct Module {
    jit: JITModule,
    context: Context,
    builder_context: FunctionBuilderContext,
}

/// The register allocator a module compiles with. Cranelift fixes it per module, so each
/// allocator has its own pool of modules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allocator {
    /// One linear pass. Its code spills more, which costs little in code that mostly calls
    /// runtime helpers, and its compile time grows linearly with the function, where the
    /// backtracking allocator's grows faster: on a 38,000-instruction body it takes 17 ms
    /// against 100 ms.
    SinglePass,
    /// The backtracking allocator, for code whose speed rests on keeping values in registers.
    Backtracking,
}

thread_local! {
    /// The session's modules, one pool per allocator. One module per pool is enough; a
    /// compilation that starts while another holds the module, as when the optimizing tier
    /// compiles a typed callee, takes a second one.
    static MODULES: [RefCell<Vec<Module>>; 2] = const {
        [RefCell::new(Vec::new()), RefCell::new(Vec::new())]
    };
}

/// Runs `f` with a code module that allocates registers with `allocator`, creating one on
/// first use.
pub fn with_module<T, E: From<Error>>(
    allocator: Allocator,
    f: impl FnOnce(&mut Module) -> Result<T, E>,
) -> Result<T, E> {
    let pool = allocator as usize;
    let taken = MODULES.with(|pools| pools[pool].borrow_mut().pop());
    let mut module = match taken {
        Some(module) => module,
        None => Module::new(allocator)?,
    };
    let result = f(&mut module);
    MODULES.with(|pools| pools[pool].borrow_mut().push(module));
    result
}

impl Module {
    fn new(allocator: Allocator) -> Result<Self, Error> {
        let isa = host_isa(allocator, false)?;
        let jit = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
        let context = jit.make_context();
        Ok(Self {
            jit,
            context,
            builder_context: FunctionBuilderContext::new(),
        })
    }

    /// Declares a function with `signature`, so bodies can call it before it is defined.
    pub fn declare(&mut self, signature: &Signature) -> Result<FuncId, Error> {
        let signature = self.signature(signature);
        self.jit
            .declare_anonymous_function(&signature)
            .map_err(|_| Error("Cranelift rejected the function declaration"))
    }

    /// Compiles `function` as the body of `id` and returns the size of its machine code.
    pub fn define(&mut self, id: FuncId, function: &Function) -> Result<u32, Error> {
        let dump = std::env::var_os("RY_SSA_DUMP").is_some();
        if dump {
            eprintln!("{function}");
        }
        self.jit.clear_context(&mut self.context);
        self.context.func.signature = self.signature(&function.signature);
        lower(
            function,
            &mut self.jit,
            &mut self.context.func,
            &mut self.builder_context,
        )?;
        if dump {
            eprintln!("{}", self.context.func.display());
        }
        self.context.set_disasm(dump);
        if let Err(failure) = self.jit.define_function(id, &mut self.context) {
            if dump {
                eprintln!("{failure:?}");
            }
            self.jit.clear_context(&mut self.context);
            return Err(Error("Cranelift rejected the function"));
        }
        let compiled = self.context.compiled_code();
        if let Some(assembly) = compiled.and_then(|code| code.vcode.as_ref()) {
            eprintln!("{assembly}");
        }
        let size = compiled.map_or(0, |code| code.code_info().total_size);
        self.jit.clear_context(&mut self.context);
        Ok(size)
    }

    /// Makes every defined function callable.
    pub fn finalize(&mut self) -> Result<(), Error> {
        self.jit
            .finalize_definitions()
            .map_err(|_| Error("Cranelift could not finalize the code"))
    }

    /// The address of a finalized function.
    pub fn address(&self, id: FuncId) -> usize {
        self.jit.get_finalized_function(id) as usize
    }

    fn signature(&self, signature: &Signature) -> clif::Signature {
        clif_signature(signature, Linker::isa(&self.jit).default_call_conv())
    }
}

impl Target for Module {
    fn declare(&mut self, signature: &Signature) -> Result<FuncId, Error> {
        Module::declare(self, signature)
    }

    fn define(&mut self, id: FuncId, function: &Function) -> Result<u32, Error> {
        Module::define(self, id, function)
    }
}

/// The host's Cranelift backend. `pic` makes every external name a load from the offset table,
/// which relocatable code needs and the JIT module does not.
fn host_isa(allocator: Allocator, pic: bool) -> Result<OwnedTargetIsa, Error> {
    let mut flags = settings::builder();
    let configure = |flags: &mut settings::Builder| -> Result<(), settings::SetError> {
        // The mid-end rewrite pass is a third of a single-pass compile, and loop-free code
        // is mostly helper calls it cannot improve, so only loop bodies pay for it.
        flags.set(
            "opt_level",
            match allocator {
                Allocator::SinglePass => "none",
                Allocator::Backtracking => "speed",
            },
        )?;
        flags.set("use_colocated_libcalls", "false")?;
        flags.set("is_pic", if pic { "true" } else { "false" })?;
        flags.set(
            "regalloc_algorithm",
            match allocator {
                Allocator::SinglePass => "single_pass",
                Allocator::Backtracking => "backtracking",
            },
        )?;
        // The verifier rechecks every function and costs as much as the rest of compilation,
        // so only debug builds, where the tests run, pay for it.
        flags.set(
            "enable_verifier",
            if cfg!(debug_assertions) {
                "true"
            } else {
                "false"
            },
        )?;
        Ok(())
    };
    configure(&mut flags).map_err(|_| Error("Cranelift rejected the settings"))?;
    cranelift_native::builder()
        .map_err(|_| Error("this host has no Cranelift backend"))?
        .finish(settings::Flags::new(flags))
        .map_err(|_| Error("Cranelift could not configure the host"))
}

/// Functions compiled to position-independent code, for a file that many processes map. Every
/// process address the code needs is a load from an offset table that sits directly below the
/// code ([`Ins::table_word`](super::Ins::table_word)), and a call between two functions of the
/// object is a direct relative call, so linking leaves no relocation in the code.
///
/// Lowering to Cranelift IR runs in [`Relocatable::define`] on the caller's thread, because the
/// caller's emission reads thread-local state; [`Relocatable::finish`] compiles that IR to
/// machine code on worker threads.
pub struct Relocatable {
    isa: OwnedTargetIsa,
    signatures: Vec<clif::Signature>,
    bodies: Vec<Option<clif::Function>>,
    builder_context: FunctionBuilderContext,
}

/// A linked [`Relocatable`]: the code of every function that compiled, with no relocation left.
/// Slot `n` of the offset table is the word at `-8 * (n + 1)` from the start of `code`, so the
/// table grows down from the code and its size never moves a slot.
pub struct Object {
    pub code: Vec<u8>,
    /// Per declared function, its offset in `code`, or why it is left out: it did not compile,
    /// needs a relocation the linker has no slot for, was never defined, or calls a function
    /// that is left out.
    pub functions: Vec<Result<u32, Error>>,
}

impl Relocatable {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            isa: host_isa(Allocator::Backtracking, true)?,
            signatures: Vec::new(),
            bodies: Vec::new(),
            builder_context: FunctionBuilderContext::new(),
        })
    }

    /// A description of the target the code is for: the triple and every ISA flag, so a file
    /// is read only by a process whose backend would have compiled the same code.
    pub fn target(&self) -> String {
        let mut text = self.isa.triple().to_string();
        for flag in self.isa.flags().iter().chain(self.isa.isa_flags()) {
            text.push(' ');
            text.push_str(&flag.to_string());
        }
        text
    }

    /// Compiles every defined function on worker threads, lays the functions out in the order
    /// they were declared, and resolves every relocation. `libcall` names the table slot that
    /// holds the address of a Cranelift library routine, such as `floor` on a CPU without the
    /// instruction, or `None` when the caller has none for it.
    pub fn finish(self, libcall: &mut dyn FnMut(&str) -> Option<u32>) -> Object {
        let Self { isa, bodies, .. } = self;
        let count = bodies.len();
        let next = AtomicUsize::new(0);
        let pending: Vec<Mutex<Option<clif::Function>>> =
            bodies.into_iter().map(Mutex::new).collect();
        let results: Vec<Mutex<Option<Result<Compiled, Error>>>> =
            (0..count).map(|_| Mutex::new(None)).collect();
        let workers = std::thread::available_parallelism().map_or(1, usize::from).min(count.max(1));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(slot) = pending.get(index) else {
                            break;
                        };
                        let body = slot.lock().ok().and_then(|mut body| body.take());
                        let compiled = match body {
                            Some(body) => compile_relocatable(&*isa, body),
                            None => Err(Error("the function was declared and never defined")),
                        };
                        if let Some(result) = results.get(index)
                            && let Ok(mut result) = result.lock()
                        {
                            *result = Some(compiled);
                        }
                    }
                });
            }
        });
        let mut compiled: Vec<Result<Compiled, Error>> = results
            .into_iter()
            .map(|result| {
                result
                    .into_inner()
                    .ok()
                    .flatten()
                    .unwrap_or(Err(Error("a worker thread failed")))
            })
            .collect();
        for function in compiled.iter_mut() {
            let Ok(code) = function else {
                continue;
            };
            for relocation in code.relocations.iter_mut() {
                if let Link::LibCall(name) = &relocation.target {
                    relocation.target = match libcall(name) {
                        Some(slot) => Link::Slot(slot),
                        None => Link::Unsupported,
                    };
                }
            }
            if code.relocations.iter().any(|relocation| {
                matches!(relocation.target, Link::Unsupported)
                    || !supported(relocation.kind)
            }) {
                *function = Err(Error("the code needs a relocation the file has no slot for"));
            }
        }
        // A function that calls one left out is left out too, until nothing changes.
        loop {
            let left_out: Vec<bool> = compiled.iter().map(Result::is_err).collect();
            let mut changed = false;
            for function in compiled.iter_mut() {
                let Ok(code) = function else {
                    continue;
                };
                let calls_left_out = code.relocations.iter().any(|relocation| {
                    matches!(relocation.target, Link::Function(callee)
                        if left_out.get(callee as usize).copied().unwrap_or(true))
                });
                if calls_left_out {
                    *function = Err(Error("it calls a function that is left out"));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut code = Vec::new();
        let mut functions = Vec::with_capacity(count);
        for function in &compiled {
            functions.push(match function {
                Ok(function) => {
                    code.resize(code.len().next_multiple_of(16), 0);
                    let start = code.len() as u32;
                    code.extend_from_slice(&function.bytes);
                    Ok(start)
                }
                Err(error) => Err(*error),
            });
        }
        for (index, function) in compiled.iter().enumerate() {
            let (Ok(function), Some(Ok(start))) = (function, functions.get(index).copied()) else {
                continue;
            };
            for relocation in &function.relocations {
                let target = match relocation.target {
                    Link::Slot(slot) => -8 * (i64::from(slot) + 1),
                    Link::Function(callee) => match functions.get(callee as usize) {
                        Some(&Ok(offset)) => i64::from(offset),
                        _ => continue,
                    },
                    Link::LibCall(_) | Link::Unsupported => continue,
                };
                let at = start as usize + relocation.offset as usize;
                if patch(&mut code, at, relocation.kind, target + relocation.addend).is_err()
                    && let Some(entry) = functions.get_mut(index)
                {
                    *entry = Err(Error("a relocation is out of range"));
                }
            }
        }
        // A relocation out of range leaves out a function whose callers were already patched to
        // it, and a call to a function left out stays unpatched, so its callers go too.
        loop {
            let mut changed = false;
            for (index, function) in compiled.iter().enumerate() {
                let Ok(function) = function else {
                    continue;
                };
                if functions.get(index).is_none_or(Result::is_err) {
                    continue;
                }
                let calls_left_out = function.relocations.iter().any(|relocation| {
                    matches!(relocation.target, Link::Function(callee)
                        if functions.get(callee as usize).is_none_or(Result::is_err))
                });
                if calls_left_out && let Some(entry) = functions.get_mut(index) {
                    *entry = Err(Error("it calls a function that is left out"));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Object { code, functions }
    }
}

impl Target for Relocatable {
    fn declare(&mut self, signature: &Signature) -> Result<FuncId, Error> {
        let id = FuncId::from_u32(self.signatures.len() as u32);
        self.signatures
            .push(clif_signature(signature, self.isa.default_call_conv()));
        self.bodies.push(None);
        Ok(id)
    }

    fn define(&mut self, id: FuncId, function: &Function) -> Result<u32, Error> {
        let index = id.as_u32() as usize;
        let signature = self
            .signatures
            .get(index)
            .cloned()
            .ok_or(Error("a definition of an undeclared function"))?;
        let mut lowered =
            clif::Function::with_name_signature(UserFuncName::user(0, id.as_u32()), signature);
        let mut declarations = Declarations {
            isa: &*self.isa,
            signatures: &self.signatures,
        };
        lower(
            function,
            &mut declarations,
            &mut lowered,
            &mut self.builder_context,
        )?;
        if let Some(body) = self.bodies.get_mut(index) {
            *body = Some(lowered);
        }
        Ok(0)
    }
}

/// One function's machine code before layout.
struct Compiled {
    bytes: Vec<u8>,
    relocations: Vec<Relocation>,
}

struct Relocation {
    offset: u32,
    kind: Reloc,
    target: Link,
    addend: i64,
}

/// What a relocation refers to.
enum Link {
    Slot(u32),
    Function(u32),
    LibCall(String),
    Unsupported,
}

fn compile_relocatable(isa: &dyn TargetIsa, body: clif::Function) -> Result<Compiled, Error> {
    let mut context = Context::for_function(body);
    let code = context
        .compile(isa, &mut ControlPlane::default())
        .map_err(|_| Error("Cranelift rejected the function"))?;
    let bytes = code.code_buffer().to_vec();
    let relocations: Vec<FinalizedMachReloc> = code.buffer.relocs().to_vec();
    let names = context.func.params.user_named_funcs();
    let libcall_names = default_libcall_names();
    let relocations = relocations
        .into_iter()
        .map(|relocation| {
            let target = match &relocation.target {
                FinalizedRelocTarget::ExternalName(ExternalName::User(reference)) => {
                    match names.get(*reference) {
                        Some(UserExternalName {
                            namespace: TABLE_NAMESPACE,
                            index,
                        }) => Link::Slot(*index),
                        Some(UserExternalName {
                            namespace: FUNCTION_NAMESPACE,
                            index,
                        }) => Link::Function(*index),
                        _ => Link::Unsupported,
                    }
                }
                FinalizedRelocTarget::ExternalName(ExternalName::LibCall(call)) => {
                    Link::LibCall(libcall_names(*call))
                }
                _ => Link::Unsupported,
            };
            Relocation {
                offset: relocation.offset,
                kind: relocation.kind,
                target,
                addend: relocation.addend,
            }
        })
        .collect();
    Ok(Compiled { bytes, relocations })
}

fn supported(kind: Reloc) -> bool {
    matches!(
        kind,
        Reloc::X86GOTPCRel4
            | Reloc::X86CallPCRel4
            | Reloc::Aarch64AdrGotPage21
            | Reloc::Aarch64Ld64GotLo12Nc
            | Reloc::Arm64Call
    )
}

/// Resolves the relocation of `kind` at `at` against `target`, both offsets from the start of
/// the code, which is mapped at a 64 KiB boundary, so page arithmetic on offsets equals page
/// arithmetic on addresses.
fn patch(code: &mut [u8], at: usize, kind: Reloc, target: i64) -> Result<(), Error> {
    let place = at as i64;
    let out_of_range = Error("a relocation is out of range");
    let word = |code: &[u8]| -> Result<u32, Error> {
        let bytes = code.get(at..at + 4).ok_or(out_of_range)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };
    let value = match kind {
        Reloc::X86GOTPCRel4 | Reloc::X86CallPCRel4 => {
            i32::try_from(target - place).map_err(|_| out_of_range)? as u32
        }
        Reloc::Aarch64AdrGotPage21 => {
            let pages = (target >> 12) - (place >> 12);
            if !(-(1 << 20)..(1 << 20)).contains(&pages) {
                return Err(out_of_range);
            }
            let pages = pages as u32;
            let instruction = word(code)? & !((0b11 << 29) | (0x7ffff << 5));
            instruction | ((pages & 0b11) << 29) | (((pages >> 2) & 0x7ffff) << 5)
        }
        Reloc::Aarch64Ld64GotLo12Nc => {
            let offset = ((target & 0xfff) >> 3) as u32;
            (word(code)? & !(0xfff << 10)) | (offset << 10)
        }
        Reloc::Arm64Call => {
            let distance = target - place;
            if distance % 4 != 0 || !(-(1 << 27)..(1 << 27)).contains(&distance) {
                return Err(out_of_range);
            }
            (word(code)? & !0x03ff_ffff) | (((distance >> 2) as u32) & 0x03ff_ffff)
        }
        _ => return Err(Error("an unsupported relocation")),
    };
    code.get_mut(at..at + 4)
        .ok_or(out_of_range)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// The external-name namespaces of lowered code: functions of the same module, and slots of the
/// offset table.
const FUNCTION_NAMESPACE: u32 = 0;
const TABLE_NAMESPACE: u32 = 1;

/// What lowering needs from the module it lowers for.
trait Linker {
    fn isa(&self) -> &dyn TargetIsa;
    /// A reference in `lowered` to the module's function `id`, for a direct call.
    fn function_ref(&mut self, id: FuncId, lowered: &mut clif::Function) -> clif::FuncRef;
    /// Whether the code may load words from an offset table.
    fn table(&self) -> bool;
}

impl Linker for JITModule {
    fn isa(&self) -> &dyn TargetIsa {
        cranelift_module::Module::isa(self)
    }

    fn function_ref(&mut self, id: FuncId, lowered: &mut clif::Function) -> clif::FuncRef {
        self.declare_func_in_func(id, lowered)
    }

    fn table(&self) -> bool {
        false
    }
}

/// The declarations of a [`Relocatable`], borrowed while one body lowers.
struct Declarations<'a> {
    isa: &'a dyn TargetIsa,
    signatures: &'a [clif::Signature],
}

impl Linker for Declarations<'_> {
    fn isa(&self) -> &dyn TargetIsa {
        self.isa
    }

    fn function_ref(&mut self, id: FuncId, lowered: &mut clif::Function) -> clif::FuncRef {
        let signature = self
            .signatures
            .get(id.as_u32() as usize)
            .cloned()
            .unwrap_or_else(|| clif::Signature::new(self.isa.default_call_conv()));
        let signature = lowered.import_signature(signature);
        let name = lowered.declare_imported_user_function(UserExternalName {
            namespace: FUNCTION_NAMESPACE,
            index: id.as_u32(),
        });
        lowered.import_function(clif::ExtFuncData {
            name: ExternalName::user(name),
            signature,
            colocated: true,
            patchable: false,
        })
    }

    fn table(&self) -> bool {
        true
    }
}

impl From<Error> for &'static str {
    fn from(error: Error) -> Self {
        error.0
    }
}

/// `signature` for Cranelift, where `host` is the platform's C convention.
fn clif_signature(signature: &Signature, host: ClifCallConv) -> clif::Signature {
    let mut lowered = clif::Signature::new(match signature.call_conv {
        CallConv::Host => host,
        CallConv::Tail => ClifCallConv::Tail,
        CallConv::Preserving => ClifCallConv::PreserveAll,
    });
    lowered.params.extend(
        signature
            .params
            .iter()
            .map(|param| clif::AbiParam::new(clif_type(param.value_type))),
    );
    lowered.returns.extend(
        signature
            .returns
            .iter()
            .map(|param| clif::AbiParam::new(clif_type(param.value_type))),
    );
    lowered
}

fn clif_type(ty: Type) -> clif::Type {
    match ty {
        Type::I8 => clif::types::I8,
        Type::I16 => clif::types::I16,
        Type::I32 => clif::types::I32,
        Type::I64 => clif::types::I64,
        Type::F64 => clif::types::F64,
    }
}

fn flags(flags: MemFlagsData) -> clif::MemFlagsData {
    if flags.trusted {
        clif::MemFlagsData::trusted()
    } else {
        clif::MemFlagsData::new()
    }
}

fn int_cc(condition: IntCC) -> ClifIntCC {
    match condition {
        IntCC::Equal => ClifIntCC::Equal,
        IntCC::NotEqual => ClifIntCC::NotEqual,
        IntCC::SignedLessThan => ClifIntCC::SignedLessThan,
        IntCC::SignedGreaterThanOrEqual => ClifIntCC::SignedGreaterThanOrEqual,
        IntCC::SignedGreaterThan => ClifIntCC::SignedGreaterThan,
        IntCC::SignedLessThanOrEqual => ClifIntCC::SignedLessThanOrEqual,
        IntCC::UnsignedLessThan => ClifIntCC::UnsignedLessThan,
        IntCC::UnsignedGreaterThanOrEqual => ClifIntCC::UnsignedGreaterThanOrEqual,
        IntCC::UnsignedGreaterThan => ClifIntCC::UnsignedGreaterThan,
        IntCC::UnsignedLessThanOrEqual => ClifIntCC::UnsignedLessThanOrEqual,
    }
}

fn float_cc(condition: FloatCC) -> ClifFloatCC {
    match condition {
        FloatCC::Ordered => ClifFloatCC::Ordered,
        FloatCC::Unordered => ClifFloatCC::Unordered,
        FloatCC::Equal => ClifFloatCC::Equal,
        FloatCC::NotEqual => ClifFloatCC::NotEqual,
        FloatCC::OrderedNotEqual => ClifFloatCC::OrderedNotEqual,
        FloatCC::UnorderedOrEqual => ClifFloatCC::UnorderedOrEqual,
        FloatCC::LessThan => ClifFloatCC::LessThan,
        FloatCC::LessThanOrEqual => ClifFloatCC::LessThanOrEqual,
        FloatCC::GreaterThan => ClifFloatCC::GreaterThan,
        FloatCC::GreaterThanOrEqual => ClifFloatCC::GreaterThanOrEqual,
        FloatCC::UnorderedOrLessThan => ClifFloatCC::UnorderedOrLessThan,
        FloatCC::UnorderedOrLessThanOrEqual => ClifFloatCC::UnorderedOrLessThanOrEqual,
        FloatCC::UnorderedOrGreaterThan => ClifFloatCC::UnorderedOrGreaterThan,
        FloatCC::UnorderedOrGreaterThanOrEqual => ClifFloatCC::UnorderedOrGreaterThanOrEqual,
    }
}

/// Lowers `function` into `lowered`. Blocks go in reverse postorder, so every value is lowered
/// before the blocks it dominates use it, and blocks the entry cannot reach are dropped.
fn lower(
    function: &Function,
    module: &mut impl Linker,
    lowered: &mut clif::Function,
    builder_context: &mut FunctionBuilderContext,
) -> Result<(), Error> {
    let functions: Vec<clif::FuncRef> = function
        .functions
        .iter()
        .map(|&(id, _)| module.function_ref(id, lowered))
        .collect();
    let host = module.isa().default_call_conv();
    let config = module.isa().frontend_config();
    let table = module.table();
    let mut builder = FunctionBuilder::new(lowered, builder_context);
    let signatures: Vec<clif::SigRef> = function
        .signatures
        .iter()
        .map(|signature| builder.import_signature(clif_signature(signature, host)))
        .collect();
    let slots: Vec<clif::StackSlot> = function
        .slots
        .iter()
        .map(|slot| {
            builder.create_sized_stack_slot(clif::StackSlotData::new(
                clif::StackSlotKind::ExplicitSlot,
                slot.size,
                slot.align_shift,
            ))
        })
        .collect();
    let order = function.reverse_postorder();
    let mut blocks: Vec<Option<clif::Block>> = vec![None; function.blocks.len()];
    let mut values: Vec<Option<clif::Value>> = vec![None; function.values.len()];
    for &block in &order {
        let Some(data) = function.blocks.get(block.index()) else {
            continue;
        };
        let target = builder.create_block();
        if data.cold {
            builder.set_cold_block(target);
        }
        let params = data.params.iter().copied().chain(
            data.variable_params
                .iter()
                .filter(|param| param.live)
                .map(|param| param.value),
        );
        for param in params {
            let ty = function
                .values
                .get(param.index())
                .map_or(Type::I64, |value| value.ty);
            let lowered = builder.append_block_param(target, clif_type(ty));
            if let Some(slot) = values.get_mut(param.index()) {
                *slot = Some(lowered);
            }
        }
        if let Some(slot) = blocks.get_mut(block.index()) {
            *slot = Some(target);
        }
    }
    let mut lowering = Lowering {
        function,
        builder,
        blocks,
        values,
        table,
        table_words: FxHashMap::default(),
    };
    for &block in &order {
        let target = lowering.block(block)?;
        lowering.builder.switch_to_block(target);
        let Some(data) = function.blocks.get(block.index()) else {
            continue;
        };
        for &inst in &data.insts {
            let Some(&op) = function.ops.get(inst.index()) else {
                continue;
            };
            lowering.inst(inst, op, &functions, &signatures, &slots)?;
        }
    }
    lowering.builder.seal_all_blocks();
    lowering.builder.finalize(config);
    Ok(())
}

struct Lowering<'f, 'b> {
    function: &'f Function,
    builder: FunctionBuilder<'b>,
    blocks: Vec<Option<clif::Block>>,
    values: Vec<Option<clif::Value>>,
    table: bool,
    /// The external name of each table slot the function loads, made once per slot so that
    /// Cranelift sees every load of one slot as the same pure value.
    table_words: FxHashMap<u32, clif::GlobalValue>,
}

impl Lowering<'_, '_> {
    fn value(&self, value: Value) -> Result<clif::Value, Error> {
        let value = self.function.resolve(value);
        self.values
            .get(value.index())
            .copied()
            .flatten()
            .ok_or(Error("a value is used where its definition does not reach"))
    }

    fn block(&self, block: super::Block) -> Result<clif::Block, Error> {
        self.blocks
            .get(block.index())
            .copied()
            .flatten()
            .ok_or(Error("a branch targets a block that was not lowered"))
    }

    fn values(&self, list: super::List) -> Result<SmallVec<[clif::Value; 8]>, Error> {
        self.function
            .list(list)
            .iter()
            .map(|&value| self.value(value))
            .collect()
    }

    /// The arguments of edge `edge` of `branch`: the explicit ones, then those for the target's
    /// live variable parameters.
    fn edge(
        &self,
        branch: super::Inst,
        edge: u8,
        target: super::Block,
        list: super::List,
    ) -> Result<SmallVec<[BlockArg; 8]>, Error> {
        let mut args: SmallVec<[BlockArg; 8]> =
            self.values(list)?.into_iter().map(BlockArg::from).collect();
        let variable_args = self.function.variable_args.get(&(branch, edge));
        if let Some(data) = self.function.blocks.get(target.index()) {
            for (index, param) in data.variable_params.iter().enumerate() {
                if !param.live {
                    continue;
                }
                let argument = variable_args
                    .and_then(|variable_args| variable_args.get(index))
                    .copied()
                    .ok_or(Error("a branch lacks an argument for a variable"))?;
                args.push(BlockArg::from(self.value(argument)?));
            }
        }
        Ok(args)
    }

    fn define(&mut self, inst: super::Inst, lowered: &[clif::Value]) {
        for (&result, &value) in self.function.results(inst).iter().zip(lowered) {
            if let Some(slot) = self.values.get_mut(result.index()) {
                *slot = Some(value);
            }
        }
    }

    fn inst(
        &mut self,
        inst: super::Inst,
        op: Op,
        functions: &[clif::FuncRef],
        signatures: &[clif::SigRef],
        slots: &[clif::StackSlot],
    ) -> Result<(), Error> {
        let slot = |slot: super::StackSlot| {
            slots
                .get(slot.index())
                .copied()
                .ok_or(Error("an unknown stack slot"))
        };
        let single = match op {
            Op::Iconst(ty, value) => self.builder.ins().iconst(clif_type(ty), value),
            Op::F64const(bits) => self.builder.ins().f64const(Ieee64::with_bits(bits)),
            Op::Binary(binary, left, right) => {
                let (left, right) = (self.value(left)?, self.value(right)?);
                let ins = self.builder.ins();
                match binary {
                    Binary::Iadd => ins.iadd(left, right),
                    Binary::Isub => ins.isub(left, right),
                    Binary::Imul => ins.imul(left, right),
                    Binary::Sdiv => ins.sdiv(left, right),
                    Binary::Srem => ins.srem(left, right),
                    Binary::Band => ins.band(left, right),
                    Binary::Bor => ins.bor(left, right),
                    Binary::Bxor => ins.bxor(left, right),
                    Binary::Ishl => ins.ishl(left, right),
                    Binary::Sshr => ins.sshr(left, right),
                    Binary::Ushr => ins.ushr(left, right),
                    Binary::Smin => ins.smin(left, right),
                    Binary::Smax => ins.smax(left, right),
                    Binary::Fadd => ins.fadd(left, right),
                    Binary::Fsub => ins.fsub(left, right),
                    Binary::Fmul => ins.fmul(left, right),
                    Binary::Fdiv => ins.fdiv(left, right),
                }
            }
            Op::BinaryImm(binary, value, immediate) => {
                let value = self.value(value)?;
                let ins = self.builder.ins();
                match binary {
                    BinaryImm::Iadd => ins.iadd_imm_s(value, immediate),
                    BinaryImm::Imul => ins.imul_imm_s(value, immediate),
                    BinaryImm::Band => ins.band_imm_u(value, immediate),
                    BinaryImm::BxorSigned => ins.bxor_imm_s(value, immediate),
                    BinaryImm::BxorUnsigned => ins.bxor_imm_u(value, immediate),
                    BinaryImm::Ushr => ins.ushr_imm_u(value, immediate),
                }
            }
            Op::Unary(unary, value) => {
                let value = self.value(value)?;
                let ins = self.builder.ins();
                match unary {
                    Unary::Ineg => ins.ineg(value),
                    Unary::Iabs => ins.iabs(value),
                    Unary::Fneg => ins.fneg(value),
                    Unary::Fabs => ins.fabs(value),
                    Unary::Sqrt => ins.sqrt(value),
                    Unary::Floor => ins.floor(value),
                    Unary::Ceil => ins.ceil(value),
                    Unary::Trunc => ins.trunc(value),
                }
            }
            Op::Icmp(condition, left, right) => {
                let (left, right) = (self.value(left)?, self.value(right)?);
                self.builder.ins().icmp(int_cc(condition), left, right)
            }
            Op::IcmpImm(condition, signed, value, immediate) => {
                let value = self.value(value)?;
                if signed {
                    self.builder
                        .ins()
                        .icmp_imm_s(int_cc(condition), value, immediate)
                } else {
                    self.builder
                        .ins()
                        .icmp_imm_u(int_cc(condition), value, immediate)
                }
            }
            Op::Fcmp(condition, left, right) => {
                let (left, right) = (self.value(left)?, self.value(right)?);
                self.builder.ins().fcmp(float_cc(condition), left, right)
            }
            Op::Select(condition, then, otherwise) => {
                let (condition, then, otherwise) = (
                    self.value(condition)?,
                    self.value(then)?,
                    self.value(otherwise)?,
                );
                self.builder.ins().select(condition, then, otherwise)
            }
            Op::Load(load, ty, memory, address, offset) => {
                let address = self.value(address)?;
                let ins = self.builder.ins();
                match load {
                    Load::Full => ins.load(clif_type(ty), flags(memory), address, offset),
                    Load::Unsigned8 => ins.uload8(clif_type(ty), flags(memory), address, offset),
                    Load::Unsigned16 => ins.uload16(clif_type(ty), flags(memory), address, offset),
                }
            }
            Op::Store(narrow, memory, value, address, offset) => {
                let (value, address) = (self.value(value)?, self.value(address)?);
                if narrow {
                    self.builder
                        .ins()
                        .istore16(flags(memory), value, address, offset);
                } else {
                    self.builder
                        .ins()
                        .store(flags(memory), value, address, offset);
                }
                return Ok(());
            }
            Op::StackAddr(ty, stack, offset) => {
                self.builder
                    .ins()
                    .stack_addr(clif_type(ty), slot(stack)?, offset)
            }
            Op::TableWord(slot) => {
                if !self.table {
                    return Err(Error("code for this module cannot load from an offset table"));
                }
                let global = match self.table_words.get(&slot) {
                    Some(&global) => global,
                    None => {
                        let name =
                            self.builder
                                .func
                                .declare_imported_user_function(UserExternalName {
                                    namespace: TABLE_NAMESPACE,
                                    index: slot,
                                });
                        let global =
                            self.builder
                                .create_global_value(clif::GlobalValueData::Symbol {
                                    name: ExternalName::user(name),
                                    offset: Imm64::new(0),
                                    colocated: false,
                                    tls: false,
                                });
                        self.table_words.insert(slot, global);
                        global
                    }
                };
                self.builder.ins().symbol_value(clif::types::I64, global)
            }
            Op::StackLoad(ty, stack, offset) => {
                self.builder
                    .ins()
                    .stack_load(clif::types::I64, clif_type(ty), slot(stack)?, offset)
            }
            Op::StackStore(value, stack, offset) => {
                let value = self.value(value)?;
                self.builder
                    .ins()
                    .stack_store(clif::types::I64, value, slot(stack)?, offset);
                return Ok(());
            }
            Op::Convert(convert, ty, value) => {
                let value = self.value(value)?;
                let ty = clif_type(ty);
                let ins = self.builder.ins();
                match convert {
                    Convert::Sextend => ins.sextend(ty, value),
                    Convert::Uextend => ins.uextend(ty, value),
                    Convert::Ireduce => ins.ireduce(ty, value),
                    Convert::FromSint => ins.fcvt_from_sint(ty, value),
                    Convert::ToSint => ins.fcvt_to_sint(ty, value),
                    Convert::ToSintSat => ins.fcvt_to_sint_sat(ty, value),
                }
            }
            Op::Bitcast(ty, value) => {
                let value = self.value(value)?;
                self.builder
                    .ins()
                    .bitcast(clif_type(ty), clif::MemFlagsData::new(), value)
            }
            Op::Overflow(overflow, left, right) => {
                let (left, right) = (self.value(left)?, self.value(right)?);
                let ins = self.builder.ins();
                let (value, flag) = match overflow {
                    Overflow::Add => ins.sadd_overflow(left, right),
                    Overflow::Sub => ins.ssub_overflow(left, right),
                    Overflow::Mul => ins.smul_overflow(left, right),
                };
                self.define(inst, &[value, flag]);
                return Ok(());
            }
            Op::Call(function, args) => {
                let callee = functions
                    .get(function.index())
                    .copied()
                    .ok_or(Error("a call of an unknown function"))?;
                let args = self.values(args)?;
                let call = self.builder.ins().call(callee, &args);
                let results: SmallVec<[clif::Value; 4]> =
                    self.builder.inst_results(call).iter().copied().collect();
                self.define(inst, &results);
                return Ok(());
            }
            Op::CallIndirect(signature, callee, args) => {
                let signature = signatures
                    .get(signature.index())
                    .copied()
                    .ok_or(Error("a call with an unknown signature"))?;
                let callee = self.value(callee)?;
                let args = self.values(args)?;
                let call = self.builder.ins().call_indirect(signature, callee, &args);
                let results: SmallVec<[clif::Value; 4]> =
                    self.builder.inst_results(call).iter().copied().collect();
                self.define(inst, &results);
                return Ok(());
            }
            Op::Jump(target, args) => {
                let args = self.edge(inst, 0, target, args)?;
                let target = self.block(target)?;
                self.builder.ins().jump(target, &args);
                return Ok(());
            }
            Op::Brif(condition, then, then_args, otherwise, otherwise_args) => {
                let condition = self.value(condition)?;
                let then_args = self.edge(inst, 0, then, then_args)?;
                let otherwise_args = self.edge(inst, 1, otherwise, otherwise_args)?;
                let (then, otherwise) = (self.block(then)?, self.block(otherwise)?);
                self.builder
                    .ins()
                    .brif(condition, then, &then_args, otherwise, &otherwise_args);
                return Ok(());
            }
            Op::Guard(condition, next, exit, args) => {
                let condition = self.value(condition)?;
                let next_args = self.edge(inst, 0, next, super::List { start: 0, len: 0 })?;
                let exit_args = self.edge(inst, 1, exit, args)?;
                let (next, exit) = (self.block(next)?, self.block(exit)?);
                self.builder
                    .ins()
                    .brif(condition, next, &next_args, exit, &exit_args);
                return Ok(());
            }
            Op::Return(values) => {
                let values = self.values(values)?;
                self.builder.ins().return_(&values);
                return Ok(());
            }
        };
        self.define(inst, &[single]);
        Ok(())
    }
}
