//! The one lowering from the IR to machine code, through Cranelift, and the code module every
//! compiled function lives in.

pub use cranelift_module::FuncId;
use {
    super::{
        Binary, BinaryImm, CallConv, Convert, FloatCC, Function, IntCC, Load, MemFlagsData, Op,
        Overflow, Signature, Type, Unary, Value,
    },
    cranelift_codegen::{
        Context,
        ir::{
            self as clif, BlockArg, InstBuilder,
            condcodes::{FloatCC as ClifFloatCC, IntCC as ClifIntCC},
            immediates::Ieee64,
        },
        isa::CallConv as ClifCallConv,
        settings::{self, Configurable},
    },
    cranelift_frontend::{FunctionBuilder, FunctionBuilderContext},
    cranelift_jit::{JITBuilder, JITModule},
    cranelift_module::{Linkage, Module as _, default_libcall_names},
    cranelift_object::{ObjectBuilder, ObjectModule as ClifObjectModule},
    smallvec::SmallVec,
    std::cell::RefCell,
};

/// Why the backend could not compile a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);

/// Where compiled functions live. Code is never freed: the backend can release a module's code
/// only all at once, and a discarded compilation's code stays mapped until the process exits.
pub struct Module {
    jit: JITModule,
    context: Context,
    builder_context: FunctionBuilderContext,
}

thread_local! {
    /// The session's modules. One is enough; a compilation that starts while another holds the
    /// module, as when the optimizing tier compiles a typed callee, takes a second one.
    static MODULES: RefCell<Vec<Module>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` with a code module, creating one on first use.
pub fn with_module<T, E: From<Error>>(f: impl FnOnce(&mut Module) -> Result<T, E>) -> Result<T, E> {
    let taken = MODULES.with_borrow_mut(Vec::pop);
    let mut module = match taken {
        Some(module) => module,
        None => Module::new()?,
    };
    let result = f(&mut module);
    MODULES.with_borrow_mut(|modules| modules.push(module));
    result
}

impl Module {
    fn new() -> Result<Self, Error> {
        let mut flags = settings::builder();
        let configure = |flags: &mut settings::Builder| -> Result<(), settings::SetError> {
            flags.set("opt_level", "speed")?;
            flags.set("use_colocated_libcalls", "false")?;
            flags.set("is_pic", "false")?;
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
        let isa = cranelift_native::builder()
            .map_err(|_| Error("this host has no Cranelift backend"))?
            .finish(settings::Flags::new(flags))
            .map_err(|_| Error("Cranelift could not configure the host"))?;
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
        if !function.symbols.is_empty() {
            return Err(Error("code for this process addresses nothing by name"));
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
        if let Err(failure) = self.jit.define_function(id, &mut self.context) {
            if dump {
                eprintln!("{failure:?}");
            }
            self.jit.clear_context(&mut self.context);
            return Err(Error("Cranelift rejected the function"));
        }
        let size = self
            .context
            .compiled_code()
            .map_or(0, |code| code.code_info().total_size);
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
        let call_conv = match signature.call_conv {
            CallConv::Host => self.jit.isa().default_call_conv(),
            CallConv::Tail => ClifCallConv::Tail,
        };
        clif_signature(signature, call_conv)
    }
}

/// Where code written ahead of time is compiled: a relocatable object for the host's
/// architecture at a fixed baseline of CPU features, linked into a [`section::Code`] that the
/// loader maps from a file. Code built here never calls by absolute address: it calls the
/// module's own functions relative to itself and takes every outside address from its link
/// table, which [`Builder::import_symbol`] names.
///
/// [`section::Code`]: crate::section::Code
/// [`Builder::import_symbol`]: crate::Builder::import_symbol
pub struct ObjectModule {
    object: ClifObjectModule,
    context: Context,
    builder_context: FunctionBuilderContext,
    names: Vec<Box<str>>,
}

impl ObjectModule {
    pub fn new() -> Result<Self, Error> {
        let mut flags = settings::builder();
        let configure = |flags: &mut settings::Builder| -> Result<(), settings::SetError> {
            flags.set("opt_level", "speed")?;
            flags.set("is_pic", "true")?;
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
        let isa = crate::section::isa_builder()?
            .finish(settings::Flags::new(flags))
            .map_err(|_| Error("Cranelift could not configure the target"))?;
        let builder = ObjectBuilder::new(isa, "ry", default_libcall_names())
            .map_err(|_| Error("Cranelift could not start an object"))?;
        let object = ClifObjectModule::new(builder);
        let context = object.make_context();
        Ok(Self {
            object,
            context,
            builder_context: FunctionBuilderContext::new(),
            names: Vec::new(),
        })
    }

    /// Declares a function the section exports under `name`, so bodies can call it before it is
    /// defined.
    pub fn declare(&mut self, name: &str, signature: &Signature) -> Result<FuncId, Error> {
        let call_conv = match signature.call_conv {
            CallConv::Host => self.object.isa().default_call_conv(),
            CallConv::Tail => ClifCallConv::Tail,
        };
        let id = self
            .object
            .declare_function(name, Linkage::Export, &clif_signature(signature, call_conv))
            .map_err(|_| Error("Cranelift rejected the function declaration"))?;
        self.names.push(name.into());
        Ok(id)
    }

    /// Compiles `function` as the body of `id` and returns the size of its machine code.
    pub fn define(&mut self, id: FuncId, function: &Function) -> Result<u32, Error> {
        self.object.clear_context(&mut self.context);
        let call_conv = match function.signature.call_conv {
            CallConv::Host => self.object.isa().default_call_conv(),
            CallConv::Tail => ClifCallConv::Tail,
        };
        self.context.func.signature = clif_signature(&function.signature, call_conv);
        lower(
            function,
            &mut self.object,
            &mut self.context.func,
            &mut self.builder_context,
        )?;
        let defined = self.object.define_function(id, &mut self.context);
        let size = self
            .context
            .compiled_code()
            .map_or(0, |code| code.code_info().total_size);
        self.object.clear_context(&mut self.context);
        defined.map_err(|_| Error("Cranelift rejected the function"))?;
        Ok(size)
    }

    /// Links every defined function into one block of code and its link table. `known` says
    /// whether the loader will be able to resolve a symbol.
    pub fn finish(self, known: &dyn Fn(&str) -> bool) -> Result<crate::section::Code, Error> {
        let names = self.names;
        let product = self.object.finish();
        let bytes = product
            .emit()
            .map_err(|_| Error("Cranelift could not write the object"))?;
        crate::section::link(&bytes, &names, known)
    }
}

impl From<Error> for &'static str {
    fn from(error: Error) -> Self {
        error.0
    }
}

fn clif_signature(signature: &Signature, call_conv: ClifCallConv) -> clif::Signature {
    let mut lowered = clif::Signature::new(call_conv);
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
    module: &mut impl cranelift_module::Module,
    lowered: &mut clif::Function,
    builder_context: &mut FunctionBuilderContext,
) -> Result<(), Error> {
    let functions: Vec<clif::FuncRef> = function
        .functions
        .iter()
        .map(|&(id, _)| module.declare_func_in_func(id, lowered))
        .collect();
    let mut symbols: Vec<clif::GlobalValue> = Vec::with_capacity(function.symbols.len());
    for name in &function.symbols {
        let data = module
            .declare_data(name, Linkage::Import, false, false)
            .map_err(|_| Error("Cranelift rejected a symbol"))?;
        symbols.push(module.declare_data_in_func(data, lowered));
    }
    let host = module.isa().default_call_conv();
    let config = module.isa().frontend_config();
    let mut builder = FunctionBuilder::new(lowered, builder_context);
    let signatures: Vec<clif::SigRef> = function
        .signatures
        .iter()
        .map(|signature| {
            let call_conv = match signature.call_conv {
                CallConv::Host => host,
                CallConv::Tail => ClifCallConv::Tail,
            };
            builder.import_signature(clif_signature(signature, call_conv))
        })
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
            lowering.inst(inst, op, &functions, &signatures, &slots, &symbols)?;
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
        symbols: &[clif::GlobalValue],
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
            Op::SymbolAddr(symbol) => {
                let symbol = symbols
                    .get(symbol.index())
                    .copied()
                    .ok_or(Error("an unknown symbol"))?;
                self.builder.ins().symbol_value(clif::types::I64, symbol)
            }
            Op::StackAddr(ty, stack, offset) => {
                self.builder
                    .ins()
                    .stack_addr(clif_type(ty), slot(stack)?, offset)
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
