//! ry's one mid-level SSA IR. The optimizing tier lowers its plan of tier-0 bytecode into it, and
//! the typed core lowers its typed IR into it, and one lowering turns it into machine code. Typed
//! facts enter as plain instructions; an untyped speculation enters as a [`Ins::guard`], whose
//! exit block is a deoptimization path that hands the frame back to the interpreter.
//!
//! The front ends build a function with [`Builder`], which reads like a small assembler: blocks,
//! block parameters, instructions, and mutable variables. Variables are a construction aid, not
//! part of the IR: [`Builder::seal_all_blocks`] rewrites every read of one into a definition or a
//! block parameter (Braun et al., "Simple and Efficient Construction of Static Single Assignment
//! Form") and removes the parameters that turn out trivial, so the IR the lowering sees is pure
//! SSA with block arguments.

mod lower;
pub mod section;

pub use lower::{Error, FuncId, Module, ObjectModule, with_module};
use {
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    std::fmt::{self, Display, Write},
};

/// The machine type of a value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Type {
    I8,
    I16,
    I32,
    I64,
    F64,
}

/// The machine types as constants, so a front end reads `I64` rather than `Type::I64`.
pub mod types {
    use super::Type;

    pub const I8: Type = Type::I8;
    pub const I16: Type = Type::I16;
    pub const I32: Type = Type::I32;
    pub const I64: Type = Type::I64;
    pub const F64: Type = Type::F64;
}

impl Type {
    pub fn bits(self) -> u32 {
        match self {
            Type::I8 => 8,
            Type::I16 => 16,
            Type::I32 => 32,
            Type::I64 | Type::F64 => 64,
        }
    }
}

macro_rules! entities {
    ($($(#[$meta:meta])* $name:ident,)*) => {$(
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub struct $name(u32);

        impl $name {
            pub fn from_u32(index: u32) -> Self {
                Self(index)
            }

            fn index(self) -> usize {
                self.0 as usize
            }
        }
    )*};
}

entities! {
    /// An SSA value: an instruction's result or a block parameter.
    Value,
    /// A basic block.
    Block,
    /// An instruction.
    Inst,
    /// A mutable variable, which exists only while the function is built.
    Variable,
    /// A slot in the function's stack frame.
    StackSlot,
    /// A signature for indirect calls.
    SigRef,
    /// A function the body calls directly.
    FuncRef,
    /// A named address the code takes from its link table, which the loader fills when it maps
    /// code written ahead of time (see [`section`]).
    SymbolRef,
}

/// An integer comparison.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IntCC {
    Equal,
    NotEqual,
    SignedLessThan,
    SignedGreaterThanOrEqual,
    SignedGreaterThan,
    SignedLessThanOrEqual,
    UnsignedLessThan,
    UnsignedGreaterThanOrEqual,
    UnsignedGreaterThan,
    UnsignedLessThanOrEqual,
}

/// A floating-point comparison. The unordered forms are also true when either side is NaN.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FloatCC {
    Ordered,
    Unordered,
    Equal,
    NotEqual,
    OrderedNotEqual,
    UnorderedOrEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    UnorderedOrLessThan,
    UnorderedOrLessThanOrEqual,
    UnorderedOrGreaterThan,
    UnorderedOrGreaterThanOrEqual,
}

/// What a memory access may assume. A trusted access is aligned and never traps, which holds for
/// every heap object and frame the front ends address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MemFlagsData {
    trusted: bool,
}

impl MemFlagsData {
    pub fn new() -> Self {
        Self { trusted: false }
    }

    pub fn trusted() -> Self {
        Self { trusted: true }
    }
}

impl Default for MemFlagsData {
    fn default() -> Self {
        Self::new()
    }
}

/// The bits of a double constant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ieee64(u64);

impl Ieee64 {
    pub fn with_bits(bits: u64) -> Self {
        Self(bits)
    }
}

impl From<f64> for Ieee64 {
    fn from(value: f64) -> Self {
        Self(value.to_bits())
    }
}

/// How a function receives its arguments. `Host` is the platform's C convention, which every
/// entry point the runtime calls uses; `Tail` is the convention compiled code calls itself with,
/// which allows tail calls.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CallConv {
    Host,
    Tail,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct AbiParam {
    pub value_type: Type,
}

impl AbiParam {
    pub fn new(value_type: Type) -> Self {
        Self { value_type }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Signature {
    pub call_conv: CallConv,
    pub params: Vec<AbiParam>,
    pub returns: Vec<AbiParam>,
}

impl Signature {
    pub fn new(call_conv: CallConv) -> Self {
        Self {
            call_conv,
            params: Vec::new(),
            returns: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StackSlotKind {
    ExplicitSlot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StackSlotData {
    pub size: u32,
    pub align_shift: u8,
}

impl StackSlotData {
    pub fn new(_kind: StackSlotKind, size: u32, align_shift: u8) -> Self {
        Self { size, align_shift }
    }
}

/// A function in the IR.
pub struct Function {
    pub signature: Signature,
    ops: Vec<Op>,
    /// Each instruction's results, as a run in the pool.
    result_lists: Vec<List>,
    /// The block each instruction belongs to.
    owners: Vec<Block>,
    values: Vec<ValueData>,
    /// The operand lists of calls, branches and returns.
    pool: Vec<Value>,
    blocks: Vec<BlockData>,
    slots: Vec<StackSlotData>,
    signatures: Vec<Signature>,
    functions: Vec<(FuncId, Signature)>,
    symbols: Vec<Box<str>>,
    entry: Option<Block>,
    /// The arguments a branch passes for its target's variable parameters, by branch and target.
    variable_args: FxHashMap<(Inst, u8), SmallVec<[Value; 4]>>,
}

#[derive(Clone, Copy)]
struct ValueData {
    ty: Type,
    definition: Definition,
}

#[derive(Clone, Copy)]
enum Definition {
    /// An instruction's result or a block parameter.
    Own,
    /// A trivial parameter that SSA construction replaced by the value it always receives.
    Alias(Value),
}

#[derive(Default)]
struct BlockData {
    params: SmallVec<[Value; 4]>,
    /// The parameters SSA construction added for variables, after the explicit ones.
    variable_params: Vec<VariableParam>,
    insts: Vec<Inst>,
    cold: bool,
}

#[derive(Clone, Copy)]
struct VariableParam {
    variable: Variable,
    value: Value,
    live: bool,
}

/// A run of operands in the function's pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct List {
    start: u32,
    len: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Iconst(Type, i64),
    F64const(u64),
    Binary(Binary, Value, Value),
    BinaryImm(BinaryImm, Value, i64),
    Unary(Unary, Value),
    Icmp(IntCC, Value, Value),
    IcmpImm(IntCC, bool, Value, i64),
    Fcmp(FloatCC, Value, Value),
    Select(Value, Value, Value),
    Load(Load, Type, MemFlagsData, Value, i32),
    /// A store of the value's low 16 bits when the flag is set.
    Store(bool, MemFlagsData, Value, Value, i32),
    StackAddr(Type, StackSlot, i32),
    /// The address the link table holds for a symbol.
    SymbolAddr(SymbolRef),
    StackLoad(Type, StackSlot, i32),
    StackStore(Value, StackSlot, i32),
    Convert(Convert, Type, Value),
    Bitcast(Type, Value),
    Overflow(Overflow, Value, Value),
    Call(FuncRef, List),
    CallIndirect(SigRef, Value, List),
    Jump(Block, List),
    Brif(Value, Block, List, Block, List),
    /// Continues to the first block when the condition is nonzero, and otherwise leaves through
    /// the second, a cold deoptimization exit.
    Guard(Value, Block, Block, List),
    Return(List),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Binary {
    Iadd,
    Isub,
    Imul,
    Sdiv,
    Srem,
    Band,
    Bor,
    Bxor,
    Smin,
    Smax,
    Fadd,
    Fsub,
    Fmul,
    Fdiv,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BinaryImm {
    Iadd,
    Imul,
    Band,
    BxorSigned,
    BxorUnsigned,
    Ushr,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unary {
    Ineg,
    Iabs,
    Fneg,
    Fabs,
    Sqrt,
    Floor,
    Ceil,
    Trunc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Load {
    Full,
    Unsigned8,
    Unsigned16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Convert {
    Sextend,
    Uextend,
    Ireduce,
    FromSint,
    ToSint,
    ToSintSat,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Overflow {
    Add,
    Sub,
    Mul,
}

impl Function {
    fn new(signature: Signature) -> Self {
        Self {
            signature,
            ops: Vec::new(),
            result_lists: Vec::new(),
            owners: Vec::new(),
            values: Vec::new(),
            pool: Vec::new(),
            blocks: Vec::new(),
            slots: Vec::new(),
            signatures: Vec::new(),
            functions: Vec::new(),
            symbols: Vec::new(),
            entry: None,
            variable_args: FxHashMap::default(),
        }
    }

    /// The value `value` stands for once trivial parameters are gone.
    fn resolve(&self, mut value: Value) -> Value {
        while let Some(ValueData {
            definition: Definition::Alias(target),
            ..
        }) = self.values.get(value.index())
        {
            value = *target;
        }
        value
    }

    fn list(&self, list: List) -> &[Value] {
        self.pool
            .get(list.start as usize..(list.start + list.len) as usize)
            .unwrap_or(&[])
    }

    fn results(&self, inst: Inst) -> &[Value] {
        self.result_lists
            .get(inst.index())
            .map_or(&[], |&list| self.list(list))
    }

    /// The branch targets of a terminator, each with its position among the branch's edges.
    fn targets(&self, inst: Inst) -> SmallVec<[(u8, Block, List); 2]> {
        match self.ops.get(inst.index()) {
            Some(&Op::Jump(block, args)) => SmallVec::from_slice(&[(0, block, args)]),
            Some(&Op::Brif(_, then, then_args, otherwise, otherwise_args)) => {
                SmallVec::from_slice(&[(0, then, then_args), (1, otherwise, otherwise_args)])
            }
            Some(&Op::Guard(_, next, exit, args)) => {
                SmallVec::from_slice(&[(0, next, List { start: 0, len: 0 }), (1, exit, args)])
            }
            _ => SmallVec::new(),
        }
    }

    fn successors(&self, block: Block) -> SmallVec<[Block; 2]> {
        self.blocks
            .get(block.index())
            .and_then(|data| data.insts.last())
            .map(|&last| {
                self.targets(last)
                    .into_iter()
                    .map(|(_, target, _)| target)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The blocks reachable from the entry, each after the blocks that dominate it.
    fn reverse_postorder(&self) -> Vec<Block> {
        let Some(entry) = self.entry else {
            return Vec::new();
        };
        let mut visited = vec![0u64; self.blocks.len().div_ceil(64)];
        let mut order = Vec::with_capacity(self.blocks.len());
        let mut stack: Vec<(Block, SmallVec<[Block; 2]>)> = Vec::new();
        let mark = |visited: &mut Vec<u64>, block: Block| -> bool {
            let (word, bit) = (block.index() / 64, block.index() % 64);
            match visited.get_mut(word) {
                Some(bits) if *bits & (1 << bit) == 0 => {
                    *bits |= 1 << bit;
                    true
                }
                _ => false,
            }
        };
        mark(&mut visited, entry);
        stack.push((entry, self.successors(entry)));
        while let Some((block, successors)) = stack.last_mut() {
            if let Some(next) = successors.pop() {
                if mark(&mut visited, next) {
                    let successors = self.successors(next);
                    stack.push((next, successors));
                }
            } else {
                order.push(*block);
                stack.pop();
            }
        }
        order.reverse();
        order
    }

    /// The number of instructions, for budgets and logs.
    pub fn instructions(&self) -> usize {
        self.ops.len()
    }
}

/// Builds a [`Function`]. The method names follow the instruction names of the lowering's
/// backend, so a front end reads like a direct emitter.
pub struct Builder {
    function: Function,
    current: Option<Block>,
    variables: Vec<Type>,
    /// The latest definition of each variable in each block.
    definitions: FxHashMap<(Variable, Block), Value>,
    /// A zero of each type at the top of the entry block, which a variable read before any
    /// definition yields.
    zeros: FxHashMap<Type, Value>,
}

impl Builder {
    pub fn new(signature: Signature) -> Self {
        Self {
            function: Function::new(signature),
            current: None,
            variables: Vec::new(),
            definitions: FxHashMap::default(),
            zeros: FxHashMap::default(),
        }
    }

    pub fn create_block(&mut self) -> Block {
        let block = Block(self.function.blocks.len() as u32);
        self.function.blocks.push(BlockData::default());
        block
    }

    pub fn switch_to_block(&mut self, block: Block) {
        if self.function.entry.is_none() {
            self.function.entry = Some(block);
        }
        self.current = Some(block);
    }

    pub fn current_block(&self) -> Option<Block> {
        self.current
    }

    /// Marks `block` as rarely taken, so the lowering places it out of the hot path.
    pub fn set_cold_block(&mut self, block: Block) {
        if let Some(data) = self.function.blocks.get_mut(block.index()) {
            data.cold = true;
        }
    }

    pub fn append_block_param(&mut self, block: Block, ty: Type) -> Value {
        let value = self.new_value(ty, Definition::Own);
        if let Some(data) = self.function.blocks.get_mut(block.index()) {
            data.params.push(value);
        }
        value
    }

    /// Gives `block` one parameter per function parameter and makes it the entry block.
    pub fn append_block_params_for_function_params(&mut self, block: Block) {
        self.function.entry = Some(block);
        let types: SmallVec<[Type; 8]> = self
            .function
            .signature
            .params
            .iter()
            .map(|param| param.value_type)
            .collect();
        for ty in types {
            self.append_block_param(block, ty);
        }
    }

    pub fn block_params(&self, block: Block) -> &[Value] {
        self.function
            .blocks
            .get(block.index())
            .map_or(&[], |data| &data.params)
    }

    pub fn declare_var(&mut self, ty: Type) -> Variable {
        let variable = Variable(self.variables.len() as u32);
        self.variables.push(ty);
        variable
    }

    pub fn def_var(&mut self, variable: Variable, value: Value) {
        if let Some(block) = self.current {
            self.definitions.insert((variable, block), value);
        }
    }

    pub fn use_var(&mut self, variable: Variable) -> Value {
        let block = self.current_or_new();
        let mut pending = Vec::new();
        let value = self.read_variable(variable, block, &mut pending);
        // Parameters created while a block is open get their arguments when the blocks are
        // sealed, because only then are all of the block's predecessors known.
        drop(pending);
        value
    }

    pub fn create_sized_stack_slot(&mut self, data: StackSlotData) -> StackSlot {
        let slot = StackSlot(self.function.slots.len() as u32);
        self.function.slots.push(data);
        slot
    }

    pub fn import_signature(&mut self, signature: Signature) -> SigRef {
        let reference = SigRef(self.function.signatures.len() as u32);
        self.function.signatures.push(signature);
        reference
    }

    /// Lets the body call the module's function `id` directly.
    pub fn import_function(&mut self, id: FuncId, signature: Signature) -> FuncRef {
        let reference = FuncRef(self.function.functions.len() as u32);
        self.function.functions.push((id, signature));
        reference
    }

    /// Names an address the code takes from its link table, once per name.
    pub fn import_symbol(&mut self, name: &str) -> SymbolRef {
        if let Some(index) = self
            .function
            .symbols
            .iter()
            .position(|symbol| &**symbol == name)
        {
            return SymbolRef(index as u32);
        }
        self.function.symbols.push(name.into());
        SymbolRef(self.function.symbols.len() as u32 - 1)
    }

    pub fn inst_results(&self, inst: Inst) -> &[Value] {
        self.function.results(inst)
    }

    pub fn ins(&mut self) -> Ins<'_> {
        Ins { builder: self }
    }

    /// Resolves every variable read into a definition or a block parameter, then removes the
    /// parameters that always receive the same value. Call it once, after the last instruction.
    pub fn seal_all_blocks(&mut self) {
        let mut predecessors: Vec<SmallVec<[(Inst, u8); 2]>> =
            vec![SmallVec::new(); self.function.blocks.len()];
        for block in &self.function.blocks {
            let Some(&last) = block.insts.last() else {
                continue;
            };
            for (edge, target, _) in self.function.targets(last) {
                if let Some(list) = predecessors.get_mut(target.index()) {
                    list.push((last, edge));
                }
            }
        }
        let mut pending: Vec<(Block, usize)> = Vec::new();
        for (index, block) in self.function.blocks.iter().enumerate() {
            pending
                .extend((0..block.variable_params.len()).map(|param| (Block(index as u32), param)));
        }
        let mut next = 0;
        while let Some(&(block, param)) = pending.get(next) {
            next += 1;
            let Some(variable) = self
                .function
                .blocks
                .get(block.index())
                .and_then(|data| data.variable_params.get(param))
                .map(|param| param.variable)
            else {
                continue;
            };
            let edges = predecessors.get(block.index()).cloned().unwrap_or_default();
            for (branch, edge) in edges {
                let Some(&owner) = self.function.owners.get(branch.index()) else {
                    continue;
                };
                let value = self.read_variable(variable, owner, &mut pending);
                let args = self
                    .function
                    .variable_args
                    .entry((branch, edge))
                    .or_default();
                if args.len() <= param {
                    args.resize(param + 1, value);
                }
                if let Some(slot) = args.get_mut(param) {
                    *slot = value;
                }
            }
        }
        self.remove_trivial_params(&predecessors);
    }

    /// Ends construction and hands over the function.
    pub fn finalize(&mut self) -> Function {
        let signature = self.function.signature.clone();
        self.current = None;
        self.definitions.clear();
        self.zeros.clear();
        std::mem::replace(&mut self.function, Function::new(signature))
    }

    fn current_or_new(&mut self) -> Block {
        match self.current {
            Some(block) => block,
            None => {
                let block = self.create_block();
                self.switch_to_block(block);
                block
            }
        }
    }

    fn new_value(&mut self, ty: Type, definition: Definition) -> Value {
        let value = Value(self.function.values.len() as u32);
        self.function.values.push(ValueData { ty, definition });
        value
    }

    /// The value `variable` holds at the end of `block`, adding a parameter to `block` when no
    /// definition there decides it. A new parameter goes on `pending` for its arguments.
    fn read_variable(
        &mut self,
        variable: Variable,
        block: Block,
        pending: &mut Vec<(Block, usize)>,
    ) -> Value {
        if let Some(&value) = self.definitions.get(&(variable, block)) {
            return value;
        }
        let ty = self
            .variables
            .get(variable.index())
            .copied()
            .unwrap_or(Type::I64);
        if Some(block) == self.function.entry {
            let zero = self.zero(ty);
            self.definitions.insert((variable, block), zero);
            return zero;
        }
        let value = self.new_value(ty, Definition::Own);
        if let Some(data) = self.function.blocks.get_mut(block.index()) {
            pending.push((block, data.variable_params.len()));
            data.variable_params.push(VariableParam {
                variable,
                value,
                live: true,
            });
        }
        self.definitions.insert((variable, block), value);
        value
    }

    fn zero(&mut self, ty: Type) -> Value {
        if let Some(&zero) = self.zeros.get(&ty) {
            return zero;
        }
        let Some(entry) = self.function.entry else {
            return self.new_value(ty, Definition::Alias(Value(u32::MAX)));
        };
        let op = match ty {
            Type::F64 => Op::F64const(0),
            _ => Op::Iconst(ty, 0),
        };
        let inst = self.record(op, entry, &[ty]);
        if let Some(data) = self.function.blocks.get_mut(entry.index()) {
            data.insts.pop();
            data.insts.insert(0, inst);
        }
        let zero = self
            .function
            .results(inst)
            .first()
            .copied()
            .unwrap_or(Value(0));
        self.zeros.insert(ty, zero);
        zero
    }

    /// Replaces each variable parameter whose arguments are all one value (or itself) by that
    /// value, until none is left. A parameter no branch reaches becomes a zero.
    fn remove_trivial_params(&mut self, predecessors: &[SmallVec<[(Inst, u8); 2]>]) {
        let mut changed = true;
        while changed {
            changed = false;
            for block in 0..self.function.blocks.len() {
                let count = self
                    .function
                    .blocks
                    .get(block)
                    .map_or(0, |data| data.variable_params.len());
                for param in 0..count {
                    let Some(&VariableParam { value, live, .. }) = self
                        .function
                        .blocks
                        .get(block)
                        .and_then(|data| data.variable_params.get(param))
                    else {
                        continue;
                    };
                    if !live {
                        continue;
                    }
                    let mut same: Option<Value> = None;
                    let mut trivial = true;
                    for edge in predecessors.get(block).map_or(&[][..], |edges| edges) {
                        let Some(&argument) = self
                            .function
                            .variable_args
                            .get(edge)
                            .and_then(|args| args.get(param))
                        else {
                            continue;
                        };
                        let argument = self.function.resolve(argument);
                        if argument == value || Some(argument) == same {
                            continue;
                        }
                        if same.is_some() {
                            trivial = false;
                            break;
                        }
                        same = Some(argument);
                    }
                    if !trivial {
                        continue;
                    }
                    let ty = self
                        .function
                        .values
                        .get(value.index())
                        .map_or(Type::I64, |data| data.ty);
                    let replacement = match same {
                        Some(same) => same,
                        None => self.zero(ty),
                    };
                    if let Some(data) = self.function.values.get_mut(value.index()) {
                        data.definition = Definition::Alias(replacement);
                    }
                    if let Some(param) = self
                        .function
                        .blocks
                        .get_mut(block)
                        .and_then(|data| data.variable_params.get_mut(param))
                    {
                        param.live = false;
                    }
                    changed = true;
                }
            }
        }
    }

    fn list(&mut self, values: &[Value]) -> List {
        let start = self.function.pool.len() as u32;
        self.function.pool.extend_from_slice(values);
        List {
            start,
            len: values.len() as u32,
        }
    }

    fn record(&mut self, op: Op, block: Block, results: &[Type]) -> Inst {
        let inst = Inst(self.function.ops.len() as u32);
        self.function.ops.push(op);
        self.function.owners.push(block);
        let values: SmallVec<[Value; 4]> = results
            .iter()
            .map(|&ty| self.new_value(ty, Definition::Own))
            .collect();
        let list = self.list(&values);
        self.function.result_lists.push(list);
        if let Some(data) = self.function.blocks.get_mut(block.index()) {
            data.insts.push(inst);
        }
        inst
    }

    fn push(&mut self, op: Op, results: &[Type]) -> Inst {
        let block = self.current_or_new();
        self.record(op, block, results)
    }

    pub fn value_type(&self, value: Value) -> Type {
        self.function
            .values
            .get(value.index())
            .map_or(Type::I64, |data| data.ty)
    }
}

/// Appends instructions to the current block.
pub struct Ins<'a> {
    builder: &'a mut Builder,
}

macro_rules! binaries {
    ($($name:ident => $op:ident,)*) => {$(
        pub fn $name(self, left: Value, right: Value) -> Value {
            let ty = self.builder.value_type(left);
            self.single(Op::Binary(Binary::$op, left, right), ty)
        }
    )*};
}

macro_rules! immediates {
    ($($name:ident => $op:ident,)*) => {$(
        pub fn $name(self, value: Value, immediate: i64) -> Value {
            let ty = self.builder.value_type(value);
            self.single(Op::BinaryImm(BinaryImm::$op, value, immediate), ty)
        }
    )*};
}

macro_rules! unaries {
    ($($name:ident => $op:ident,)*) => {$(
        pub fn $name(self, value: Value) -> Value {
            let ty = self.builder.value_type(value);
            self.single(Op::Unary(Unary::$op, value), ty)
        }
    )*};
}

macro_rules! conversions {
    ($($name:ident => $op:ident,)*) => {$(
        pub fn $name(self, ty: Type, value: Value) -> Value {
            self.single(Op::Convert(Convert::$op, ty, value), ty)
        }
    )*};
}

macro_rules! loads {
    ($($name:ident => $op:ident,)*) => {$(
        pub fn $name(self, ty: Type, flags: MemFlagsData, address: Value, offset: i32) -> Value {
            self.single(Op::Load(Load::$op, ty, flags, address, offset), ty)
        }
    )*};
}

macro_rules! overflows {
    ($($name:ident => $op:ident,)*) => {$(
        pub fn $name(self, left: Value, right: Value) -> (Value, Value) {
            let ty = self.builder.value_type(left);
            let inst = self.builder.push(Op::Overflow(Overflow::$op, left, right), &[ty, Type::I8]);
            match *self.builder.function.results(inst) {
                [value, flag] => (value, flag),
                _ => (Value(u32::MAX), Value(u32::MAX)),
            }
        }
    )*};
}

impl Ins<'_> {
    binaries! {
        iadd => Iadd, isub => Isub, imul => Imul, sdiv => Sdiv, srem => Srem, band => Band,
        bor => Bor, bxor => Bxor, smin => Smin, smax => Smax, fadd => Fadd, fsub => Fsub,
        fmul => Fmul, fdiv => Fdiv,
    }

    immediates! {
        iadd_imm_s => Iadd, imul_imm_s => Imul, band_imm_u => Band, bxor_imm_s => BxorSigned,
        bxor_imm_u => BxorUnsigned, ushr_imm_u => Ushr,
    }

    unaries! {
        ineg => Ineg, iabs => Iabs, fneg => Fneg, fabs => Fabs, sqrt => Sqrt, floor => Floor,
        ceil => Ceil, trunc => Trunc,
    }

    conversions! {
        sextend => Sextend, uextend => Uextend, ireduce => Ireduce, fcvt_from_sint => FromSint,
        fcvt_to_sint => ToSint, fcvt_to_sint_sat => ToSintSat,
    }

    loads! { load => Full, uload8 => Unsigned8, uload16 => Unsigned16, }

    overflows! { sadd_overflow => Add, ssub_overflow => Sub, smul_overflow => Mul, }

    pub fn iconst(self, ty: Type, value: i64) -> Value {
        self.single(Op::Iconst(ty, value), ty)
    }

    /// The address the link table holds for `symbol`, a 64-bit value.
    pub fn symbol_addr(self, symbol: SymbolRef) -> Value {
        self.single(Op::SymbolAddr(symbol), Type::I64)
    }

    pub fn f64const(self, value: impl Into<Ieee64>) -> Value {
        self.single(Op::F64const(value.into().0), Type::F64)
    }

    pub fn icmp(self, condition: IntCC, left: Value, right: Value) -> Value {
        self.single(Op::Icmp(condition, left, right), Type::I8)
    }

    pub fn icmp_imm_s(self, condition: IntCC, value: Value, immediate: i64) -> Value {
        self.single(Op::IcmpImm(condition, true, value, immediate), Type::I8)
    }

    pub fn icmp_imm_u(self, condition: IntCC, value: Value, immediate: i64) -> Value {
        self.single(Op::IcmpImm(condition, false, value, immediate), Type::I8)
    }

    pub fn fcmp(self, condition: FloatCC, left: Value, right: Value) -> Value {
        self.single(Op::Fcmp(condition, left, right), Type::I8)
    }

    pub fn select(self, condition: Value, then: Value, otherwise: Value) -> Value {
        let ty = self.builder.value_type(then);
        self.single(Op::Select(condition, then, otherwise), ty)
    }

    pub fn store(self, flags: MemFlagsData, value: Value, address: Value, offset: i32) -> Inst {
        self.builder
            .push(Op::Store(false, flags, value, address, offset), &[])
    }

    pub fn istore16(self, flags: MemFlagsData, value: Value, address: Value, offset: i32) -> Inst {
        self.builder
            .push(Op::Store(true, flags, value, address, offset), &[])
    }

    pub fn stack_addr(self, ty: Type, slot: StackSlot, offset: i32) -> Value {
        self.single(Op::StackAddr(ty, slot, offset), ty)
    }

    pub fn stack_load(self, _pointer: Type, ty: Type, slot: StackSlot, offset: i32) -> Value {
        self.single(Op::StackLoad(ty, slot, offset), ty)
    }

    pub fn stack_store(self, _pointer: Type, value: Value, slot: StackSlot, offset: i32) -> Inst {
        self.builder.push(Op::StackStore(value, slot, offset), &[])
    }

    pub fn bitcast(self, ty: Type, _flags: MemFlagsData, value: Value) -> Value {
        self.single(Op::Bitcast(ty, value), ty)
    }

    pub fn call(self, function: FuncRef, args: &[Value]) -> Inst {
        let returns: SmallVec<[Type; 4]> = self
            .builder
            .function
            .functions
            .get(function.index())
            .map(|(_, signature)| {
                signature
                    .returns
                    .iter()
                    .map(|param| param.value_type)
                    .collect()
            })
            .unwrap_or_default();
        let args = self.builder.list(args);
        self.builder.push(Op::Call(function, args), &returns)
    }

    pub fn call_indirect(self, signature: SigRef, callee: Value, args: &[Value]) -> Inst {
        let returns: SmallVec<[Type; 4]> = self
            .builder
            .function
            .signatures
            .get(signature.index())
            .map(|signature| {
                signature
                    .returns
                    .iter()
                    .map(|param| param.value_type)
                    .collect()
            })
            .unwrap_or_default();
        let args = self.builder.list(args);
        self.builder
            .push(Op::CallIndirect(signature, callee, args), &returns)
    }

    pub fn jump(self, block: Block, args: &[Value]) -> Inst {
        let args = self.builder.list(args);
        self.builder.push(Op::Jump(block, args), &[])
    }

    pub fn brif(
        self,
        condition: Value,
        then: Block,
        then_args: &[Value],
        otherwise: Block,
        otherwise_args: &[Value],
    ) -> Inst {
        let then_args = self.builder.list(then_args);
        let otherwise_args = self.builder.list(otherwise_args);
        self.builder.push(
            Op::Brif(condition, then, then_args, otherwise, otherwise_args),
            &[],
        )
    }

    /// Continues in a new block when `condition` holds and otherwise leaves through `exit`,
    /// which becomes cold. The exit reads variables as they are at the guard.
    pub fn guard(self, condition: Value, exit: Block, args: &[Value]) -> Inst {
        let builder = self.builder;
        let next = builder.create_block();
        builder.set_cold_block(exit);
        let args = builder.list(args);
        let inst = builder.push(Op::Guard(condition, next, exit, args), &[]);
        builder.switch_to_block(next);
        inst
    }

    pub fn return_(self, values: &[Value]) -> Inst {
        let values = self.builder.list(values);
        self.builder.push(Op::Return(values), &[])
    }

    fn single(self, op: Op, ty: Type) -> Value {
        let inst = self.builder.push(op, &[ty]);
        self.builder
            .function
            .results(inst)
            .first()
            .copied()
            .unwrap_or(Value(u32::MAX))
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = |value: Value| format!("v{}", self.resolve(value).0);
        let values = |values: &[Value]| {
            values
                .iter()
                .map(|&argument| value(argument))
                .collect::<Vec<_>>()
                .join(", ")
        };
        writeln!(f, "function {:?} {{", self.signature.call_conv)?;
        for block in self.reverse_postorder() {
            let Some(data) = self.blocks.get(block.index()) else {
                continue;
            };
            let mut params: Vec<Value> = data.params.to_vec();
            params.extend(
                data.variable_params
                    .iter()
                    .filter(|param| param.live)
                    .map(|param| param.value),
            );
            write!(f, "block{}({})", block.0, values(&params))?;
            writeln!(f, "{}:", if data.cold { " cold" } else { "" })?;
            for &inst in &data.insts {
                let mut line = String::new();
                let results = self.results(inst);
                if !results.is_empty() {
                    write!(line, "{} = ", values(results))?;
                }
                let Some(op) = self.ops.get(inst.index()) else {
                    continue;
                };
                write!(line, "{op:?}")?;
                for (edge, target, _) in self.targets(inst) {
                    if let Some(args) = self.variable_args.get(&(inst, edge)) {
                        write!(line, " block{}+[{}]", target.0, values(args))?;
                    }
                }
                writeln!(f, "    {line}")?;
            }
        }
        writeln!(f, "}}")
    }
}
