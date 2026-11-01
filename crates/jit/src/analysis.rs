//! The speculation plan: what compiled code does at each tier-0 instruction.
//!
//! The analysis decodes a body's bytecode into blocks, computes which registers are live, and
//! runs an abstract interpretation over the blocks whose values are [`Ty`]s: what kind of R value
//! each register and each variable holds, which decides whether compiled code keeps it unboxed in
//! a machine register. Type feedback from tier 0 seeds the types wherever a value comes from code
//! the JIT does not see through (a variable read from the environment, a call), and every such
//! place becomes a guard that deoptimizes when the speculation is wrong. The result is one
//! [`Action`] per instruction plus the abstract state before it, which is everything code
//! generation and deoptimization need: the typed plan is the JIT's intermediate representation,
//! and Cranelift's frontend turns it into SSA.
//!
//! Variables live in *slots*, one per symbol the body reads or assigns, instead of in the
//! environment. A slot's runtime status says where its value is: not loaded yet (the environment
//! is the truth), loaded from the activation's own frame, loaded from an enclosing environment,
//! or assigned by compiled code and not yet written back (dirty). The environment is brought up
//! to date before anything that can observe it runs, and slots are forgotten after anything that
//! can change it, so code outside the compiled body never sees the difference.

use heap::object::{Sexp, SexpType, gp};
use interp::{
    bytecode::{
        self, ARITHS, Arith, FEEDBACK, OPS, Op, feedback, operand_count,
    },
    values,
};
use rustc_hash::{FxHashMap, FxHashSet};

/// What compiled code knows about a value, which decides its machine representation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    /// No value: the register is dead or not yet written.
    Bottom,
    /// An attribute-free integer scalar, unboxed as an `i32` (`NA` included).
    Int,
    /// An attribute-free double scalar, unboxed as an `f64`.
    Dbl,
    /// An attribute-free logical scalar, unboxed as an `i32`.
    Lgl,
    /// A boxed, attribute-free, non-ALTREP integer vector of any length.
    IntVec,
    DblVec,
    LglVec,
    /// A boxed, attribute-free list.
    List,
    /// A boxed function known to be exactly this object.
    Fun(Sexp),
    /// A closure made by a `function` expression of this body, in this activation's
    /// environment: its formals and its body as source.
    Closure(Sexp, Sexp),
    /// Any boxed value.
    Boxed,
}

impl Ty {
    pub fn is_unboxed(self) -> bool {
        matches!(self, Ty::Int | Ty::Dbl | Ty::Lgl)
    }

    /// Whether every value of this type is a plain object, which no builtin dispatches on.
    pub fn is_plain(self) -> bool {
        !matches!(self, Ty::Boxed | Ty::Bottom | Ty::Closure(..))
    }

    pub fn is_numeric_scalar(self) -> bool {
        matches!(self, Ty::Int | Ty::Dbl | Ty::Lgl)
    }

    pub fn is_vector(self) -> bool {
        matches!(self, Ty::IntVec | Ty::DblVec | Ty::LglVec)
    }

    /// The scalar type of one element.
    pub fn element(self) -> Ty {
        match self {
            Ty::IntVec | Ty::Int => Ty::Int,
            Ty::DblVec | Ty::Dbl => Ty::Dbl,
            Ty::LglVec | Ty::Lgl => Ty::Lgl,
            _ => Ty::Boxed,
        }
    }

    /// The vector type a boxed scalar of this type is an instance of.
    fn vector(self) -> Ty {
        match self {
            Ty::Int => Ty::IntVec,
            Ty::Dbl => Ty::DblVec,
            Ty::Lgl => Ty::LglVec,
            other => other,
        }
    }

    pub fn join(self, other: Ty) -> Ty {
        match (self, other) {
            (a, b) if a == b => a,
            (Ty::Bottom, b) => b,
            (a, Ty::Bottom) => a,
            (a, b) if a.vector() == b.vector() && a.vector().is_vector() => a.vector(),
            _ => Ty::Boxed,
        }
    }
}

/// The type tier 0's feedback bits speculate on.
pub fn from_feedback(bits: u8) -> Ty {
    let within = |mask: u8| bits & !mask == 0;
    match bits {
        0 => Ty::Boxed,
        feedback::INTEGER_SCALAR => Ty::Int,
        feedback::DOUBLE_SCALAR => Ty::Dbl,
        feedback::LOGICAL_SCALAR => Ty::Lgl,
        _ if within(feedback::INTEGER_SCALAR | feedback::INTEGER_VECTOR) => Ty::IntVec,
        _ if within(feedback::DOUBLE_SCALAR | feedback::DOUBLE_VECTOR) => Ty::DblVec,
        _ if within(feedback::LOGICAL_SCALAR | feedback::LOGICAL_VECTOR) => Ty::LglVec,
        feedback::LIST => Ty::List,
        _ => Ty::Boxed,
    }
}

/// Slot statuses, as bits of [`Slot::status`]. At run time a slot's status is the bit's index.
pub mod status {
    pub const NOT_LOADED: u8 = 1 << 0;
    pub const LOCAL: u8 = 1 << 1;
    pub const ENCLOSING: u8 = 1 << 2;
    pub const DIRTY: u8 = 1 << 3;
    pub const LOADED: u8 = LOCAL | ENCLOSING | DIRTY;

    pub const RUNTIME_LOCAL: i64 = 1;
    pub const RUNTIME_ENCLOSING: i64 = 2;
    pub const RUNTIME_DIRTY: i64 = 3;
}

/// What compiled code knows about a variable: the statuses it may have at run time, and the
/// type of its value when it has one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot {
    pub status: u8,
    pub ty: Ty,
}

impl Slot {
    const UNLOADED: Slot = Slot { status: status::NOT_LOADED, ty: Ty::Bottom };

    fn join(self, other: Slot) -> Slot {
        let ty = if self.status & status::LOADED == 0 {
            other.ty
        } else if other.status & status::LOADED == 0 {
            self.ty
        } else {
            self.ty.join(other.ty)
        };
        Slot { status: self.status | other.status, ty }
    }

    pub fn may_be(self, bits: u8) -> bool {
        self.status & bits != 0
    }
}

/// The abstract state before an instruction.
#[derive(Clone, PartialEq, Debug)]
pub struct State {
    pub registers: Vec<Ty>,
    pub slots: Vec<Slot>,
    /// What `R_Visible` holds, when known, so redundant stores can be skipped.
    pub visible: Option<bool>,
}

impl State {
    fn join(&self, other: &State) -> State {
        State {
            registers: self.registers.iter().zip(&other.registers).map(|(a, b)| a.join(*b)).collect(),
            slots: self.slots.iter().zip(&other.slots).map(|(a, b)| a.join(*b)).collect(),
            visible: if self.visible == other.visible { self.visible } else { None },
        }
    }

    /// Forgets every variable, after code that may have rebound any of them ran.
    fn forget_slots(&mut self) {
        for slot in &mut self.slots {
            *slot = Slot::UNLOADED;
        }
    }

    /// Marks every dirty variable as written back.
    fn flushed(&mut self) {
        for slot in &mut self.slots {
            if slot.may_be(status::DIRTY) {
                slot.status = (slot.status & !status::DIRTY) | status::LOCAL;
            }
        }
    }
}

/// One decoded instruction.
#[derive(Clone, Debug)]
pub struct Inst {
    pub pc: usize,
    pub op: Op,
    pub operands: Vec<usize>,
}

/// How an instruction the JIT does not specialize affects the compiled activation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Reads and writes only its registers, and never signals a condition.
    Pure,
    /// Runs no R code unless one of these registers holds an object or a calling handler is
    /// established, since a warning runs the handler; in either case it is opaque.
    PureUnlessObserved,
    /// May run arbitrary R code, which may read or rebind the frame's variables.
    Opaque,
}

/// Scalar arithmetic the JIT does inline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Numeric {
    /// On `i32`s with R's `NA` and overflow rules.
    Int,
    /// On `f64`s, converting integer and logical operands.
    Dbl,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intrinsic {
    /// `x[i]` or `x[[i]]` on a plain vector with a scalar index.
    Element,
    Length,
    Negate(Numeric),
    Not,
    IsNa,
    Sqrt,
    Exp,
    Log,
    Abs(Numeric),
    Floor,
    Ceiling,
    /// `%%`
    Modulo(Numeric),
    /// `%/%`
    IntegerDivide(Numeric),
}

/// What compiled code does at one instruction.
#[derive(Clone, Debug)]
pub enum Action {
    /// Nothing: the instruction is unreachable or folds away.
    Skip,
    Constant { dst: usize, value: Sexp, ty: Ty },
    /// Reads a slot; `load` when the slot may not be loaded, with the type to refine to.
    /// `forcing` marks a load that may evaluate a default argument, which runs in this frame.
    GetVar { dst: usize, slot: usize, load: Option<Ty>, forcing: bool },
    SetVar { slot: usize, src: usize },
    /// A function lookup resolved at compile time; the speculations check it still holds.
    GetFun { dst: usize, function: Sexp },
    /// A function lookup that finds a closure in a slot, loading the slot first when `load`.
    GetFunSlot { dst: usize, slot: usize, load: Option<Ty> },
    /// A call of a closure whose body was inlined: each step with the state before it, on
    /// registers past tier 0's own. The steps have no effect but loading slots, so every check
    /// that fails in them hands the call itself back to tier 0.
    Inline { steps: Vec<(Action, State)> },
    /// Loads a slot without forcing a promise, handing the instruction to tier 0 when the
    /// variable is not a plain value. Only inlined calls use it.
    Peek { slot: usize, ty: Ty },
    /// Copies an unboxed scalar between registers.
    Move { dst: usize, src: usize, ty: Ty },
    /// Runs the instruction in tier 0, then refines its destination register.
    Step { effect: Effect, objects: Vec<usize>, dst: Option<(usize, Ty)> },
    /// The instruction's check always passes, so control falls through.
    FallThrough,
    /// Control always goes to the jump target.
    Branch,
    BranchUnlessBuiltin { function: usize },
    Jump,
    JumpUnless { src: usize },
    Arithmetic { arith: Arith, numeric: Numeric, dst: usize, left: usize, right: usize },
    Comparison { arith: Arith, numeric: Numeric, dst: usize, left: usize, right: usize },
    Intrinsic { intrinsic: Intrinsic, dst: usize, args: Vec<usize>, result: Ty },
    Return { src: usize },
    ForInit { index: usize, src: usize, refine: Option<Ty> },
    ForStep { index: usize, src: usize, slot: usize },
    Visible(bool),
    LogicLeft { dst: usize, src: usize, is_and: bool },
    LogicRight { dst: usize, left: usize, right: usize, is_and: bool },
    /// `x[i] <- value` in place on a plain vector, with a generic path that copies or grows.
    SubAssign { dst: usize, slot: usize, index: usize, value: usize },
}

/// A function lookup compiled code assumes still resolves as it did at compile time.
#[derive(Clone, Debug)]
pub struct Speculation {
    pub symbol: Sexp,
    /// The binding cell the lookup found.
    pub cell: Sexp,
    pub function: Sexp,
    /// Function frames the lookup passed through, whose bindings come and go without moving
    /// the heap's binding epoch.
    pub frames: Vec<Sexp>,
    /// The function frame holding `cell`, whose bindings can be removed without moving the epoch.
    pub home: Option<Sexp>,
}

pub struct Block {
    /// Index of the first instruction.
    pub start: usize,
    /// One past the last instruction.
    pub end: usize,
    pub entry: Option<State>,
}

pub struct Plan {
    pub insts: Vec<Inst>,
    /// Registers past tier 0's that hold the unboxed values of inlined calls. They never live
    /// across an instruction and never reach tier 0.
    pub temporaries: usize,
    pub blocks: Vec<Block>,
    /// The block each leader pc starts.
    pub block_at: FxHashMap<usize, usize>,
    pub actions: Vec<Action>,
    /// The state before each instruction, `None` when it is unreachable.
    pub before: Vec<Option<State>>,
    /// Live registers before each instruction.
    pub live: Vec<Vec<bool>>,
    pub slots: Vec<Sexp>,
    pub registers: usize,
    pub loops: usize,
    pub speculations: Vec<Speculation>,
    /// The state along each edge leaving each instruction, by target pc, the fallthrough edge
    /// included.
    pub edges: Vec<Vec<(usize, State)>>,
    /// Blocks a backward jump targets, where on-stack replacement may enter.
    pub loop_headers: Vec<usize>,
}

/// The formals whose default is an expression rather than a constant or nothing.
unsafe fn defaulted_formals(formals: Sexp) -> FxHashSet<Sexp> {
    let mut defaulted = FxHashSet::default();
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        let default = formal.car();
        if matches!(default.kind(), SexpType::Language | SexpType::Symbol | SexpType::Promise) && default != values::missing_arg() {
            defaulted.insert(formal.tag());
        }
        formal = formal.cdr();
    }
    defaulted
}

/// Registers past tier 0's for the values of inlined calls.
const TEMPORARIES: usize = 32;

/// How deeply an inlined call may itself inline calls.
const MAX_INLINE_DEPTH: usize = 3;

/// An inlined call being lowered.
struct Lowering {
    /// The state as of the next step.
    state: State,
    steps: Vec<(Action, State)>,
    /// The next free temporary register.
    next: usize,
    /// The formals of the closure being inlined and the registers holding their arguments.
    scope: Vec<(Sexp, usize)>,
}

impl Lowering {
    fn push(&mut self, action: Action) {
        self.steps.push((action, self.state.clone()));
    }

    fn temporary(&mut self) -> Option<usize> {
        (self.next < self.state.registers.len()).then(|| {
            self.next += 1;
            self.next - 1
        })
    }
}

/// Why a body is not compiled.
#[derive(Debug)]
pub struct Refusal(pub &'static str);

/// The refusal for a body that has not run far enough to have the type feedback it needs,
/// which a later attempt may well get past.
pub const COLD: &str = "the body has no type feedback yet";

/// Builds the plan for `code`, a closure body whose environment's enclosure is `enclosure`.
pub unsafe fn plan(code: Sexp, formals: Sexp, enclosure: Sexp) -> Result<Plan, Refusal> {
    let constants = code.cdr();
    let source = constants.element(bytecode::SOURCE);
    // A body that reflects on its frame can bind any symbol, which slots cannot model.
    if bytecode::local_symbols(source, formals).is_none() {
        return Err(Refusal("the body reflects on its frame"));
    }
    let insts = decode(code)?;
    let registers = values::ints(constants.element(bytecode::REGISTERS))[0] as usize;
    let loops = bytecode::loop_count(code);
    let mut builder = Builder {
        code,
        enclosure,
        feedback: values::raws(constants.element(FEEDBACK)),
        insts,
        slot_of: FxHashMap::default(),
        slots: Vec::new(),
        speculated: FxHashMap::default(),
        speculations: Vec::new(),
        registers,
        defaulted: defaulted_formals(formals),
    };
    builder.collect_slots();
    let (mut blocks, block_at, successors, loop_headers) = builder.blocks();
    let live = builder.liveness(&blocks, &successors);
    builder.fixpoint(&mut blocks, &block_at, &live, &loop_headers)?;
    let mut actions = Vec::with_capacity(builder.insts.len());
    let mut before = vec![None; builder.insts.len()];
    let mut edges = vec![Vec::new(); builder.insts.len()];
    for block in &blocks {
        let Some(entry) = &block.entry else {
            actions.extend((block.start..block.end).map(|_| Action::Skip));
            continue;
        };
        let mut state = entry.clone();
        for position in block.start..block.end {
            before[position] = Some(state.clone());
            let (action, out) = builder.transfer(position, &mut state)?;
            actions.push(action);
            edges[position] = out;
        }
    }
    Ok(Plan {
        temporaries: TEMPORARIES,
        insts: builder.insts,
        blocks,
        block_at,
        actions,
        before,
        live,
        slots: builder.slots,
        registers,
        loops,
        speculations: builder.speculations,
        edges,
        loop_headers,
    })
}

/// Decodes the instruction words.
pub unsafe fn decode(code: Sexp) -> Result<Vec<Inst>, Refusal> {
    let words = values::ints(code.car());
    let mut insts = Vec::new();
    let mut pc = 0;
    while pc < words.len() {
        let op = *OPS.get(words[pc] as usize).ok_or(Refusal("an unknown opcode"))?;
        let count = operand_count(op);
        let operands = words.get(pc + 1..pc + 1 + count).ok_or(Refusal("a truncated instruction"))?;
        insts.push(Inst { pc, op, operands: operands.iter().map(|&word| word as usize).collect() });
        pc += 1 + count;
    }
    Ok(insts)
}

/// The base builtins that run no R code given plain arguments, so calling them leaves the
/// frame's variables alone. Internal generics among them dispatch on objects, which is why calls
/// check their boxed arguments first.
const PURE_BUILTINS: &[&str] = &[
    "+", "-", "*", "/", "^", "%%", "%/%", "==", "!=", "<", ">", "<=", ">=", "!", "&", "|", "sqrt", "exp", "log",
    "abs", "floor", "ceiling", "trunc", "round", "signif", "sin", "cos", "tan", "asin", "acos", "atan", "log2",
    "log10", "log1p", "expm1", "length", "[", "[[", "c", "is.na", "is.null", "sum", "prod", "max", "min",
    "cumsum", "cumprod", "seq_len", "seq_along", ":", "rnorm", "runif", "rexp", "rbinom", "rpois", "vector",
    "numeric", "integer", "double", "logical", "character", "rep", "rep.int", "rep_len", "nchar", "paste",
    "paste0", "unlist", "names", "any", "all", "which", "rev", "as.integer", "as.numeric", "as.double",
    "as.character", "as.logical", "is.numeric", "is.character", "is.logical", "is.function", "identical",
    "range", "dim", "list", "abs", "mean", "var", "order", "sort", "max", "pmin", "pmax",
];

struct Builder<'a> {
    code: Sexp,
    enclosure: Sexp,
    feedback: &'a [u8],
    insts: Vec<Inst>,
    slot_of: FxHashMap<Sexp, usize>,
    slots: Vec<Sexp>,
    /// Speculation results by symbol, so each lookup resolves once.
    speculated: FxHashMap<Sexp, Option<Sexp>>,
    speculations: Vec<Speculation>,
    registers: usize,
    /// Formals whose default is an expression. Their first load may evaluate it in this frame,
    /// where it reads and may assign the body's variables.
    defaulted: FxHashSet<Sexp>,
}

impl Builder<'_> {
    unsafe fn constant(&self, index: usize) -> Sexp {
        self.code.cdr().element(index)
    }

    unsafe fn collect_slots(&mut self) {
        for position in 0..self.insts.len() {
            let inst = &self.insts[position];
            let symbol_operand = match inst.op {
                Op::GetVar => Some(inst.operands[1]),
                Op::SetVar => Some(inst.operands[0]),
                Op::ForStep => Some(inst.operands[2]),
                Op::SubAssign => Some(inst.operands[1]),
                _ => None,
            };
            if let Some(operand) = symbol_operand {
                let symbol = self.constant(operand);
                if !self.slot_of.contains_key(&symbol) {
                    self.slot_of.insert(symbol, self.slots.len());
                    self.slots.push(symbol);
                }
            }
        }
    }

    fn slot(&self, symbol: Sexp) -> usize {
        self.slot_of[&symbol]
    }

    fn feedback_at(&self, pc: usize) -> Ty {
        from_feedback(self.feedback.get(pc).copied().unwrap_or(0))
    }

    /// Splits the instructions into blocks and finds each block's successors and the loop
    /// headers (the blocks a backward jump targets).
    #[allow(clippy::type_complexity)]
    fn blocks(&self) -> (Vec<Block>, FxHashMap<usize, usize>, Vec<Vec<usize>>, Vec<usize>) {
        let mut leaders = FxHashSet::default();
        leaders.insert(0);
        for (position, inst) in self.insts.iter().enumerate() {
            if let Some(target) = jump_target(inst) {
                leaders.insert(target);
            }
            if ends_block(inst.op)
                && let Some(next) = self.insts.get(position + 1)
            {
                leaders.insert(next.pc);
            }
        }
        let mut blocks = Vec::new();
        let mut block_at = FxHashMap::default();
        for (position, inst) in self.insts.iter().enumerate() {
            if leaders.contains(&inst.pc) {
                if let Some(last) = blocks.last_mut() {
                    let last: &mut Block = last;
                    last.end = position;
                }
                block_at.insert(inst.pc, blocks.len());
                blocks.push(Block { start: position, end: position + 1, entry: None });
            }
        }
        if let Some(last) = blocks.last_mut() {
            last.end = self.insts.len();
        }
        let successors = blocks
            .iter()
            .map(|block| {
                let last = &self.insts[block.end - 1];
                let mut next = Vec::new();
                if falls_through(last.op) && block.end < self.insts.len() {
                    next.push(block_at[&self.insts[block.end].pc]);
                }
                if let Some(target) = jump_target(last)
                    && let Some(&target) = block_at.get(&target)
                {
                    next.push(target);
                }
                next
            })
            .collect();
        let mut loop_headers: Vec<usize> = self
            .insts
            .iter()
            .filter(|inst| inst.op == Op::Jump && inst.operands[0] <= inst.pc)
            .filter_map(|inst| block_at.get(&inst.operands[0]).copied())
            .collect();
        loop_headers.sort_unstable();
        loop_headers.dedup();
        (blocks, block_at, successors, loop_headers)
    }

    /// Registers live before each instruction, as tier 0 sees them (every path, including the
    /// fallback paths compiled code never takes, since deoptimization can reach them).
    fn liveness(&self, blocks: &[Block], successors: &[Vec<usize>]) -> Vec<Vec<bool>> {
        let count = self.registers;
        let mut live_in: Vec<Vec<bool>> = vec![vec![false; count]; blocks.len()];
        let mut changed = true;
        while changed {
            changed = false;
            for index in (0..blocks.len()).rev() {
                let mut live = vec![false; count];
                for &successor in &successors[index] {
                    for (register, flag) in live_in[successor].iter().enumerate() {
                        live[register] |= flag;
                    }
                }
                for position in (blocks[index].start..blocks[index].end).rev() {
                    apply_liveness(&self.insts[position], &mut live);
                }
                if live != live_in[index] {
                    live_in[index] = live;
                    changed = true;
                }
            }
        }
        let mut per_inst = vec![Vec::new(); self.insts.len()];
        for (index, block) in blocks.iter().enumerate() {
            let mut live = vec![false; count];
            for &successor in &successors[index] {
                for (register, flag) in live_in[successor].iter().enumerate() {
                    live[register] |= flag;
                }
            }
            for position in (block.start..block.end).rev() {
                apply_liveness(&self.insts[position], &mut live);
                per_inst[position] = live.clone();
            }
        }
        per_inst
    }

    /// Computes every reachable block's entry state. A loop header is also where on-stack
    /// replacement enters with every variable still in the environment, so its entry state
    /// allows every slot to be unloaded.
    unsafe fn fixpoint(
        &mut self,
        blocks: &mut [Block],
        block_at: &FxHashMap<usize, usize>,
        live: &[Vec<bool>],
        loop_headers: &[usize],
    ) -> Result<(), Refusal> {
        let initial = State {
            registers: vec![Ty::Bottom; self.registers + TEMPORARIES],
            slots: vec![Slot::UNLOADED; self.slots.len()],
            visible: None,
        };
        blocks[0].entry = Some(initial);
        let mut work = vec![0usize];
        let mut rounds = 0usize;
        while let Some(index) = work.pop() {
            rounds += 1;
            if rounds > 100_000 {
                return Err(Refusal("the type analysis did not converge"));
            }
            let Some(mut state) = blocks[index].entry.clone() else { continue };
            let (start, end) = (blocks[index].start, blocks[index].end);
            let mut edges = Vec::new();
            for position in start..end {
                let (_, out) = self.transfer(position, &mut state)?;
                if position + 1 == end {
                    edges = out;
                }
            }
            for (target_pc, mut out) in edges {
                let Some(&target) = block_at.get(&target_pc) else { continue };
                let target_live = &live[blocks[target].start];
                for (register, ty) in out.registers.iter_mut().enumerate() {
                    if !target_live.get(register).copied().unwrap_or(false) {
                        *ty = Ty::Bottom;
                    }
                }
                if loop_headers.contains(&target) {
                    for slot in &mut out.slots {
                        *slot = slot.join(Slot::UNLOADED);
                    }
                }
                let joined = match &blocks[target].entry {
                    Some(existing) => existing.join(&out),
                    None => out,
                };
                if blocks[target].entry.as_ref() != Some(&joined) {
                    blocks[target].entry = Some(joined);
                    if !work.contains(&target) {
                        work.push(target);
                    }
                }
            }
        }
        Ok(())
    }

    /// The action for the instruction at `position` and its effect on `state`. For the last
    /// instruction of a block it also returns the state along each outgoing edge, by target pc.
    unsafe fn transfer(&mut self, position: usize, state: &mut State) -> Result<(Action, Vec<(usize, State)>), Refusal> {
        let inst = self.insts[position].clone();
        let operand = |index: usize| inst.operands[index];
        let next_pc = self.insts.get(position + 1).map(|next| next.pc);
        let mut edges = Vec::new();
        let action = match inst.op {
            Op::Constant => {
                let value = self.constant(operand(1));
                let ty = constant_type(value);
                state.registers[operand(0)] = ty;
                state.visible = Some(true);
                Action::Constant { dst: operand(0), value, ty }
            }
            Op::GetVar => {
                let slot = self.slot(self.constant(operand(1)));
                let current = state.slots[slot];
                let load = if current.may_be(status::NOT_LOADED) {
                    let ty = if current.may_be(status::LOADED) { current.ty } else { self.feedback_at(inst.pc) };
                    Some(if ty == Ty::Bottom { Ty::Boxed } else { ty })
                } else {
                    None
                };
                let forcing = load.is_some() && self.defaulted.contains(&self.slots[slot]);
                if let Some(ty) = load {
                    let mut status_bits = current.status & status::LOADED;
                    status_bits |= status::LOCAL | status::ENCLOSING;
                    let loaded = Slot { status: status_bits, ty };
                    // Code generation writes the variables back before the load and forgets them
                    // after it, but only when the load really happens.
                    let mut forced = state.clone();
                    forced.flushed();
                    forced.forget_slots();
                    forced.slots[slot] = loaded;
                    state.slots[slot] = loaded;
                    if forcing {
                        *state = if current.may_be(status::LOADED) { state.join(&forced) } else { forced };
                    }
                }
                state.registers[operand(0)] = state.slots[slot].ty;
                state.visible = Some(true);
                Action::GetVar { dst: operand(0), slot, load, forcing }
            }
            Op::SetVar => {
                let slot = self.slot(self.constant(operand(0)));
                let ty = match state.registers[operand(1)] {
                    Ty::Bottom => Ty::Boxed,
                    ty => ty,
                };
                state.slots[slot] = Slot { status: status::DIRTY, ty };
                state.visible = Some(false);
                Action::SetVar { slot, src: operand(1) }
            }
            Op::GetFun => {
                let symbol = self.constant(operand(1));
                if let Some(&slot) = self.slot_of.get(&symbol)
                    && let Ty::Closure(..) = state.slots[slot].ty
                    && !self.defaulted.contains(&symbol)
                {
                    let current = state.slots[slot];
                    let load = current.may_be(status::NOT_LOADED).then_some(current.ty);
                    if load.is_some() {
                        state.slots[slot] = Slot { status: (current.status & status::LOADED) | status::LOCAL | status::ENCLOSING, ty: current.ty };
                    }
                    state.registers[operand(0)] = current.ty;
                    return self.finish(Action::GetFunSlot { dst: operand(0), slot, load }, inst.op, next_pc, state, edges);
                }
                match self.speculate(symbol) {
                    Some(function) if !self.slot_of.contains_key(&symbol) => {
                        state.registers[operand(0)] = Ty::Fun(function);
                        Action::GetFun { dst: operand(0), function }
                    }
                    _ => self.opaque_step(state, Some((operand(0), Ty::Boxed)), Vec::new(), Effect::Opaque),
                }
            }
            Op::BranchUnlessBuiltin => match state.registers[operand(0)] {
                Ty::Fun(function) if function.kind() == SexpType::Builtin => Action::FallThrough,
                Ty::Fun(_) | Ty::Closure(..) => {
                    edges.push((operand(1), state.clone()));
                    return Ok((Action::Branch, edges));
                }
                _ => {
                    edges.push((operand(1), state.clone()));
                    Action::BranchUnlessBuiltin { function: operand(0) }
                }
            },
            Op::CallBuiltin => self.call_builtin(&inst, state)?,
            Op::CallGeneric => {
                let call = self.constant(operand(2));
                let inlined = match state.registers[operand(1)] {
                    Ty::Closure(formals, body) => self.inline(state, call, formals, body, operand(0)),
                    _ => None,
                };
                match inlined {
                    Some(action) => action,
                    None => self.opaque_step(state, Some((operand(0), self.feedback_at(inst.pc))), Vec::new(), Effect::Opaque),
                }
            }
            Op::Ast => self.opaque_step(state, Some((operand(0), self.feedback_at(inst.pc))), Vec::new(), Effect::Opaque),
            Op::Guard => {
                let symbol = self.constant(operand(0));
                let expected = self.constant(operand(1));
                if operand(3) != 0 || self.slot_of.contains_key(&symbol) {
                    return Err(Refusal("the frame may rebind a syntax function"));
                }
                if self.speculate(symbol) != Some(expected) {
                    return Err(Refusal("a syntax function is redefined"));
                }
                Action::FallThrough
            }
            Op::Jump => {
                edges.push((operand(0), state.clone()));
                return Ok((Action::Jump, edges));
            }
            Op::JumpUnless => {
                edges.push((operand(2), state.clone()));
                Action::JumpUnless { src: operand(0) }
            }
            Op::Arithmetic => self.arithmetic(&inst, state),
            Op::Return => Action::Return { src: operand(0) },
            Op::ForInit => {
                let src = operand(1);
                let ty = state.registers[src];
                let refine = match ty {
                    // Code generation boxes an unboxed scalar sequence before the loop.
                    Ty::IntVec | Ty::DblVec | Ty::LglVec | Ty::List | Ty::Int | Ty::Dbl | Ty::Lgl => None,
                    _ => {
                        if self.feedback.get(inst.pc).copied().unwrap_or(0) == 0 {
                            return Err(Refusal(COLD));
                        }
                        let speculated = self.feedback_at(inst.pc);
                        if !matches!(speculated, Ty::IntVec | Ty::DblVec | Ty::LglVec | Ty::List | Ty::Int | Ty::Dbl | Ty::Lgl) {
                            return Err(Refusal("a for loop over something other than a plain vector"));
                        }
                        Some(speculated.vector())
                    }
                };
                state.registers[src] = refine.unwrap_or(ty.vector());
                Action::ForInit { index: operand(0), src, refine }
            }
            Op::ForStep => {
                let slot = self.slot(self.constant(operand(2)));
                edges.push((operand(3), state.clone()));
                let element = state.registers[operand(1)].element();
                state.slots[slot] = Slot { status: status::DIRTY, ty: element };
                Action::ForStep { index: operand(0), src: operand(1), slot }
            }
            Op::Invisible => {
                state.visible = Some(false);
                Action::Visible(false)
            }
            Op::Visible => {
                state.visible = Some(true);
                Action::Visible(true)
            }
            Op::LogicLeft => {
                let mut decided = state.clone();
                decided.registers[operand(0)] = Ty::Lgl;
                decided.visible = Some(true);
                edges.push((operand(4), decided));
                state.registers[operand(1)] = Ty::Lgl;
                Action::LogicLeft { dst: operand(0), src: operand(1), is_and: operand(3) == 1 }
            }
            Op::LogicRight => {
                state.registers[operand(0)] = Ty::Lgl;
                state.visible = Some(true);
                Action::LogicRight { dst: operand(0), left: operand(1), right: operand(2), is_and: operand(4) == 1 }
            }
            Op::Closure => {
                state.visible = Some(true);
                let ty = Ty::Closure(self.constant(operand(1)), self.constant(operand(2)));
                state.registers[operand(0)] = ty;
                Action::Step { effect: Effect::Pure, objects: Vec::new(), dst: Some((operand(0), ty)) }
            }
            Op::SubAssign => {
                let slot = self.slot(self.constant(operand(1)));
                let (index, value) = (state.registers[operand(2)], state.registers[operand(3)]);
                let target = state.slots[slot].ty;
                let fits = match target {
                    Ty::DblVec => value.is_numeric_scalar(),
                    Ty::IntVec => matches!(value, Ty::Int | Ty::Lgl),
                    Ty::LglVec => value == Ty::Lgl,
                    _ => false,
                };
                if fits && matches!(index, Ty::Int | Ty::Dbl) && state.slots[slot].may_be(status::LOADED)
                    && !(self.defaulted.contains(&self.slots[slot]) && state.slots[slot].may_be(status::NOT_LOADED))
                {
                    // The generic path writes the variable back, runs the assignment in tier 0
                    // and forgets the variable; the fast path may make it dirty by copying it.
                    let mut forgotten = state.slots[slot];
                    forgotten.status = status::NOT_LOADED;
                    let mut fast = state.slots[slot];
                    fast.status |= status::DIRTY;
                    fast.status &= !status::NOT_LOADED;
                    state.slots[slot] = fast.join(forgotten);
                    state.registers[operand(0)] = value;
                    state.visible = Some(false);
                    Action::SubAssign { dst: operand(0), slot, index: operand(2), value: operand(3) }
                } else {
                    let refine = match value {
                        Ty::Bottom => Ty::Boxed,
                        ty => ty,
                    };
                    let action = self.opaque_step(state, Some((operand(0), refine)), Vec::new(), Effect::Opaque);
                    state.visible = Some(false);
                    action
                }
            }
        };
        self.finish(action, inst.op, next_pc, state, edges)
    }

    /// Adds the fallthrough edge. Every instruction reports it: the fixpoint reads the edges of a
    /// block's last instruction, and code generation reads the state after any instruction.
    #[allow(clippy::type_complexity)]
    fn finish(&self, action: Action, op: Op, next_pc: Option<usize>, state: &State, mut edges: Vec<(usize, State)>) -> Result<(Action, Vec<(usize, State)>), Refusal> {
        if falls_through(op)
            && let Some(next) = next_pc
            && !matches!(action, Action::Branch)
        {
            edges.push((next, state.clone()));
        }
        Ok((action, edges))
    }

    /// Inlines a call of the closure `formals`/`body` made in this activation, when the call and
    /// the body compute an unboxed scalar from scalars without any effect: arithmetic,
    /// comparisons, inline builtins and further such calls. The arguments are evaluated before
    /// the body, which R's lazy evaluation cannot observe because nothing in between has an
    /// effect, and an argument that would fail or is not a plain scalar hands the whole call
    /// back to tier 0, which then evaluates it lazily.
    unsafe fn inline(&mut self, state: &mut State, call: Sexp, formals: Sexp, body: Sexp, dst: usize) -> Option<Action> {
        let mut lowering = Lowering { state: state.clone(), steps: Vec::new(), next: self.registers, scope: Vec::new() };
        let result = self.lower_call(&mut lowering, call, formals, body, 0)?;
        let ty = lowering.state.registers[result];
        lowering.push(Action::Move { dst, src: result, ty });
        let mut after = lowering.state;
        for register in self.registers..after.registers.len() {
            after.registers[register] = Ty::Bottom;
        }
        after.registers[dst] = ty;
        after.visible = Some(true);
        *state = after;
        Some(Action::Inline { steps: lowering.steps })
    }

    unsafe fn lower_call(&mut self, lowering: &mut Lowering, call: Sexp, formals: Sexp, body: Sexp, depth: usize) -> Option<usize> {
        if depth > MAX_INLINE_DEPTH {
            return None;
        }
        let args = values::pairlist_items(call.cdr());
        let formals = values::pairlist_items(formals);
        let positional = args.iter().all(|(tag, value)| *tag == values::nil() && *value != values::missing_arg() && *value != values::symbols().dots);
        // Every formal needs its argument: a default is evaluated in the callee's frame, which
        // the inlined body does not have.
        if !positional || args.len() != formals.len() || formals.iter().any(|(symbol, _)| *symbol == values::symbols().dots) {
            return None;
        }
        let outer = std::mem::take(&mut lowering.scope);
        let mut scope = Vec::with_capacity(args.len());
        for (_, expression) in &args {
            let register = self.lower_expression(lowering, *expression, depth);
            match register {
                Some(register) => scope.push(register),
                None => {
                    lowering.scope = outer;
                    return None;
                }
            }
        }
        lowering.scope = formals.iter().map(|(symbol, _)| *symbol).zip(scope).collect();
        let result = self.lower_expression(lowering, body, depth);
        lowering.scope = outer;
        result
    }

    /// Lowers an expression of an inlined call into steps, returning the register that holds
    /// its unboxed value. Symbols resolve in `lowering.scope` (the formals of the closure being
    /// inlined) and then in this activation's slots, which is where both the call's arguments
    /// and the free variables of a closure made in this activation are found.
    unsafe fn lower_expression(&mut self, lowering: &mut Lowering, expression: Sexp, depth: usize) -> Option<usize> {
        match expression.kind() {
            SexpType::Integer | SexpType::Real | SexpType::Logical => {
                let ty = constant_type(expression);
                if !ty.is_unboxed() {
                    return None;
                }
                let dst = lowering.temporary()?;
                lowering.push(Action::Constant { dst, value: expression, ty });
                lowering.state.registers[dst] = ty;
                Some(dst)
            }
            SexpType::Symbol => {
                if let Some(&(_, register)) = lowering.scope.iter().find(|(symbol, _)| *symbol == expression) {
                    return Some(register);
                }
                let slot = self.peek_slot(lowering, expression)?;
                let ty = lowering.state.slots[slot].ty;
                if !ty.is_unboxed() {
                    return None;
                }
                let dst = lowering.temporary()?;
                lowering.push(Action::GetVar { dst, slot, load: None, forcing: false });
                lowering.state.registers[dst] = ty;
                Some(dst)
            }
            SexpType::Language => {
                let head = expression.car();
                if head.kind() != SexpType::Symbol {
                    return None;
                }
                let args: Vec<Sexp> = values::pairlist_items(expression.cdr()).iter().map(|(tag, value)| {
                    if *tag == values::nil() { *value } else { values::missing_arg() }
                }).collect();
                if args.contains(&values::missing_arg()) {
                    return None;
                }
                if !lowering.scope.iter().any(|(symbol, _)| *symbol == head)
                    && let Some(&slot) = self.slot_of.get(&head)
                {
                    // A closure this activation made, called from inlined code: inline it too.
                    self.peek_slot(lowering, head)?;
                    let Ty::Closure(formals, body) = lowering.state.slots[slot].ty else { return None };
                    return self.lower_call(lowering, expression, formals, body, depth + 1);
                }
                let name = self.base_builtin(lowering, head)?;
                let registers = args.iter().map(|&arg| self.lower_expression(lowering, arg, depth)).collect::<Option<Vec<usize>>>()?;
                let types: Vec<Ty> = registers.iter().map(|&register| lowering.state.registers[register]).collect();
                match (name.as_str(), registers.as_slice()) {
                    ("(" | "{", [inner]) => Some(*inner),
                    (operator, [left, right]) if let Some(&(arith, _)) = ARITHS.iter().find(|(_, symbol)| *symbol == operator) => {
                        let (a, b) = (types[0], types[1]);
                        let dst = lowering.temporary()?;
                        let integers = a != Ty::Dbl && b != Ty::Dbl;
                        let comparison = matches!(arith, Arith::Equal | Arith::NotEqual | Arith::Less | Arith::Greater | Arith::LessEqual | Arith::GreaterEqual);
                        let (action, ty) = if comparison {
                            let numeric = if integers { Numeric::Int } else { Numeric::Dbl };
                            (Action::Comparison { arith, numeric, dst, left: *left, right: *right }, Ty::Lgl)
                        } else {
                            let numeric = match arith {
                                Arith::Add | Arith::Subtract | Arith::Multiply if integers => Numeric::Int,
                                _ => Numeric::Dbl,
                            };
                            let ty = if numeric == Numeric::Int { Ty::Int } else { Ty::Dbl };
                            (Action::Arithmetic { arith, numeric, dst, left: *left, right: *right }, ty)
                        };
                        lowering.push(action);
                        lowering.state.registers[dst] = ty;
                        Some(dst)
                    }
                    (name, _) => {
                        let (intrinsic, result) = intrinsic_for(name, &types)?;
                        if !result.is_unboxed() {
                            return None;
                        }
                        let dst = lowering.temporary()?;
                        lowering.push(Action::Intrinsic { intrinsic, dst, args: registers, result });
                        lowering.state.registers[dst] = result;
                        Some(dst)
                    }
                }
            }
            _ => None,
        }
    }

    /// Makes sure `symbol`'s slot is loaded in inlined code, peeking at it when it may not be,
    /// and returns the slot.
    unsafe fn peek_slot(&mut self, lowering: &mut Lowering, symbol: Sexp) -> Option<usize> {
        let slot = *self.slot_of.get(&symbol)?;
        if self.defaulted.contains(&symbol) {
            return None;
        }
        let current = lowering.state.slots[slot];
        if current.may_be(status::NOT_LOADED) {
            if !current.may_be(status::LOADED) || current.ty == Ty::Bottom || current.ty == Ty::Boxed {
                return None;
            }
            lowering.push(Action::Peek { slot, ty: current.ty });
            lowering.state.slots[slot] = Slot { status: (current.status & status::LOADED) | status::LOCAL | status::ENCLOSING, ty: current.ty };
        }
        Some(slot)
    }

    /// The name of the base builtin `symbol` calls from inlined code, which must not be
    /// shadowed by a formal of an inlined closure or a variable of this activation.
    unsafe fn base_builtin(&mut self, lowering: &Lowering, symbol: Sexp) -> Option<String> {
        if lowering.scope.iter().any(|(formal, _)| *formal == symbol) || self.slot_of.contains_key(&symbol) {
            return None;
        }
        let function = self.speculate(symbol)?;
        if !matches!(function.kind(), SexpType::Builtin | SexpType::Special) {
            return None;
        }
        let name = interp::eval::builtin_def(function).ok()?.name;
        (values::symbol_name(symbol) == name).then(|| name.to_string())
    }

    /// A tier-0 step whose destination is refined to `dst`'s type afterwards.
    fn opaque_step(&mut self, state: &mut State, dst: Option<(usize, Ty)>, objects: Vec<usize>, effect: Effect) -> Action {
        match effect {
            Effect::Pure => {}
            // Only a call that dispatches or warns into a handler writes the variables back and
            // forgets them.
            Effect::PureUnlessObserved => {
                for slot in &mut state.slots {
                    slot.status |= status::NOT_LOADED;
                }
            }
            Effect::Opaque => {
                state.flushed();
                state.forget_slots();
            }
        }
        state.visible = None;
        if let Some((register, ty)) = dst {
            state.registers[register] = ty;
        }
        Action::Step { effect, objects, dst }
    }

    unsafe fn arithmetic(&mut self, inst: &Inst, state: &mut State) -> Action {
        let operand = |index: usize| inst.operands[index];
        let arith = ARITHS[operand(0)].0;
        let (dst, left, right) = (operand(1), operand(2), operand(3));
        let (a, b) = (state.registers[left], state.registers[right]);
        state.visible = Some(true);
        if a.is_numeric_scalar() && b.is_numeric_scalar() {
            let integers = !matches!(a, Ty::Dbl) && !matches!(b, Ty::Dbl);
            let comparison = matches!(
                arith,
                Arith::Equal | Arith::NotEqual | Arith::Less | Arith::Greater | Arith::LessEqual | Arith::GreaterEqual
            );
            if comparison {
                state.registers[dst] = Ty::Lgl;
                let numeric = if integers { Numeric::Int } else { Numeric::Dbl };
                return Action::Comparison { arith, numeric, dst, left, right };
            }
            let numeric = match arith {
                Arith::Add | Arith::Subtract | Arith::Multiply if integers => Numeric::Int,
                _ => Numeric::Dbl,
            };
            state.registers[dst] = if numeric == Numeric::Int { Ty::Int } else { Ty::Dbl };
            return Action::Arithmetic { arith, numeric, dst, left, right };
        }
        let effect = if a.is_plain() && b.is_plain() { Effect::PureUnlessObserved } else { Effect::Opaque };
        let ty = self.feedback_at(inst.pc);
        let action = self.opaque_step(state, Some((dst, ty)), Vec::new(), effect);
        state.visible = Some(true);
        action
    }

    unsafe fn call_builtin(&mut self, inst: &Inst, state: &mut State) -> Result<Action, Refusal> {
        let operand = |index: usize| inst.operands[index];
        let dst = operand(0);
        let call = self.constant(operand(2));
        let (first, count) = (operand(3), operand(4));
        let args: Vec<usize> = (first..first + count).collect();
        let feedback = self.feedback_at(inst.pc);
        let Ty::Fun(function) = state.registers[operand(1)] else {
            return Ok(self.opaque_step(state, Some((dst, feedback)), Vec::new(), Effect::Opaque));
        };
        let definition = interp::eval::builtin_def(function).map_err(|_| Refusal("a builtin without a definition"))?;
        let name = definition.name;
        let untagged = values::pairlist_items(call.cdr()).iter().all(|(tag, _)| *tag == values::nil());
        let types: Vec<Ty> = args.iter().map(|&register| state.registers[register]).collect();
        if untagged && let Some((intrinsic, result)) = intrinsic_for(name, &types) {
            state.registers[dst] = result;
            state.visible = Some(true);
            return Ok(Action::Intrinsic { intrinsic, dst, args, result });
        }
        if PURE_BUILTINS.contains(&name) {
            let objects: Vec<usize> = args.iter().copied().filter(|&register| !state.registers[register].is_plain()).collect();
            return Ok(self.opaque_step(state, Some((dst, feedback)), objects, Effect::PureUnlessObserved));
        }
        Ok(self.opaque_step(state, Some((dst, feedback)), Vec::new(), Effect::Opaque))
    }

    /// The function `symbol` resolves to from the enclosure, found without forcing promises,
    /// recorded as a speculation.
    unsafe fn speculate(&mut self, symbol: Sexp) -> Option<Sexp> {
        if let Some(found) = self.speculated.get(&symbol) {
            return *found;
        }
        let found = resolve_function(symbol, self.enclosure);
        let function = found.as_ref().map(|speculation| speculation.function);
        if let Some(speculation) = found {
            self.speculations.push(speculation);
        }
        self.speculated.insert(symbol, function);
        function
    }
}

/// Walks from `enclosure` to the binding a function lookup of `symbol` finds, as tier 0 does,
/// but gives up instead of forcing a promise or calling an active binding.
pub unsafe fn resolve_function(symbol: Sexp, mut env: Sexp) -> Option<Speculation> {
    let heap = interp::heap();
    let empty = heap.known().empty_env;
    let mut frames = Vec::new();
    while env != empty {
        if let Some(cell) = heap.frame_cell(env, symbol) {
            if cell.has_gp(gp::ACTIVE_BINDING) {
                return None;
            }
            let mut value = cell.cdr();
            if value.kind() == SexpType::Promise {
                if value.car() == values::unbound() {
                    return None;
                }
                value = value.car();
            }
            if values::is_function(value) {
                let home = env.has_gp(gp::FUNCTION_FRAME).then_some(env);
                return Some(Speculation { symbol, cell, function: value, frames, home });
            }
        }
        if env.has_gp(gp::FUNCTION_FRAME) {
            frames.push(env);
        }
        env = env.enclosure();
    }
    None
}

unsafe fn constant_type(value: Sexp) -> Ty {
    match bytecode::value_kind(value) {
        feedback::INTEGER_SCALAR => Ty::Int,
        feedback::DOUBLE_SCALAR => Ty::Dbl,
        feedback::LOGICAL_SCALAR => Ty::Lgl,
        feedback::INTEGER_VECTOR => Ty::IntVec,
        feedback::DOUBLE_VECTOR => Ty::DblVec,
        feedback::LOGICAL_VECTOR => Ty::LglVec,
        feedback::LIST => Ty::List,
        _ if values::is_function(value) => Ty::Fun(value),
        _ => Ty::Boxed,
    }
}

/// The intrinsic for a call of builtin `name` with arguments of these types, and its result.
fn intrinsic_for(name: &str, types: &[Ty]) -> Option<(Intrinsic, Ty)> {
    let numeric = |ty: Ty| if ty == Ty::Dbl { Numeric::Dbl } else { Numeric::Int };
    Some(match (name, types) {
        ("[" | "[[", [vector, index]) if vector.is_vector() && matches!(index, Ty::Int | Ty::Dbl) => {
            (Intrinsic::Element, vector.element())
        }
        ("[[", [Ty::List, index]) if matches!(index, Ty::Int | Ty::Dbl) => (Intrinsic::Element, Ty::Boxed),
        ("length", [ty]) if ty.is_vector() || ty.is_numeric_scalar() || *ty == Ty::List => (Intrinsic::Length, Ty::Int),
        ("-", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Negate(numeric(*ty)), if *ty == Ty::Dbl { Ty::Dbl } else { Ty::Int }),
        ("!", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Not, Ty::Lgl),
        ("is.na", [ty]) if ty.is_numeric_scalar() => (Intrinsic::IsNa, Ty::Lgl),
        ("sqrt", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Sqrt, Ty::Dbl),
        ("exp", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Exp, Ty::Dbl),
        ("log", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Log, Ty::Dbl),
        ("abs", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Abs(numeric(*ty)), if *ty == Ty::Dbl { Ty::Dbl } else { Ty::Int }),
        ("floor", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Floor, Ty::Dbl),
        ("ceiling", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Ceiling, Ty::Dbl),
        ("%%", [a, b]) if a.is_numeric_scalar() && b.is_numeric_scalar() => {
            let numeric = if *a == Ty::Dbl || *b == Ty::Dbl { Numeric::Dbl } else { Numeric::Int };
            (Intrinsic::Modulo(numeric), if numeric == Numeric::Int { Ty::Int } else { Ty::Dbl })
        }
        ("%/%", [a, b]) if a.is_numeric_scalar() && b.is_numeric_scalar() => {
            let numeric = if *a == Ty::Dbl || *b == Ty::Dbl { Numeric::Dbl } else { Numeric::Int };
            (Intrinsic::IntegerDivide(numeric), if numeric == Numeric::Int { Ty::Int } else { Ty::Dbl })
        }
        _ => return None,
    })
}

pub fn jump_target(inst: &Inst) -> Option<usize> {
    match inst.op {
        Op::Jump => Some(inst.operands[0]),
        Op::JumpUnless => Some(inst.operands[2]),
        Op::BranchUnlessBuiltin => Some(inst.operands[1]),
        Op::ForStep => Some(inst.operands[3]),
        Op::LogicLeft => Some(inst.operands[4]),
        Op::Guard => Some(inst.operands[4]),
        _ => None,
    }
}

fn ends_block(op: Op) -> bool {
    matches!(
        op,
        Op::Jump | Op::JumpUnless | Op::BranchUnlessBuiltin | Op::ForStep | Op::LogicLeft | Op::Guard | Op::Return
    )
}

fn falls_through(op: Op) -> bool {
    !matches!(op, Op::Jump | Op::Return)
}

/// Updates `live` from after `inst` to before it.
fn apply_liveness(inst: &Inst, live: &mut [bool]) {
    let operand = |index: usize| inst.operands[index];
    let kill = |register: usize, live: &mut [bool]| {
        if let Some(flag) = live.get_mut(register) {
            *flag = false;
        }
    };
    let gen_ = |register: usize, live: &mut [bool]| {
        if let Some(flag) = live.get_mut(register) {
            *flag = true;
        }
    };
    match inst.op {
        Op::Constant | Op::GetVar | Op::GetFun | Op::Ast | Op::Closure => kill(operand(0), live),
        Op::SetVar => gen_(operand(1), live),
        Op::BranchUnlessBuiltin => gen_(operand(0), live),
        Op::CallBuiltin => {
            kill(operand(0), live);
            gen_(operand(1), live);
            for register in operand(3)..operand(3) + operand(4) {
                gen_(register, live);
            }
        }
        Op::CallGeneric => {
            kill(operand(0), live);
            gen_(operand(1), live);
        }
        Op::Guard | Op::Jump | Op::Invisible | Op::Visible => {}
        Op::JumpUnless | Op::Return => gen_(operand(0), live),
        Op::Arithmetic => {
            kill(operand(1), live);
            gen_(operand(2), live);
            gen_(operand(3), live);
        }
        Op::ForInit | Op::ForStep => gen_(operand(1), live),
        Op::LogicLeft => gen_(operand(1), live),
        Op::LogicRight => {
            kill(operand(0), live);
            gen_(operand(1), live);
            gen_(operand(2), live);
        }
        Op::SubAssign => {
            kill(operand(0), live);
            gen_(operand(2), live);
            gen_(operand(3), live);
        }
    }
}
