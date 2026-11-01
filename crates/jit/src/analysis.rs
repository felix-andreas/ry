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
//! The plan is data-oriented and lives in one bump arena per compilation, dropped once code
//! generation is done: the abstract states are rows of one flat matrix addressed by
//! [`StateId`], liveness is a bitset per instruction, and instructions borrow their operands from
//! the bytecode words instead of copying them.
//!
//! Variables live in *slots*, one per symbol the body reads or assigns, instead of in the
//! environment. A slot's runtime status says where its value is: not loaded yet (the environment
//! is the truth), loaded from the activation's own frame, loaded from an enclosing environment,
//! or assigned by compiled code and not yet written back (dirty). The environment is brought up
//! to date before anything that can observe it runs, and slots are forgotten after anything that
//! can change it, so code outside the compiled body never sees the difference.

use {
    bumpalo::Bump,
    heap::object::{Sexp, SexpType, gp},
    interp::{
        annotations::{self, Entry},
        bytecode::{self, ARITHS, Arith, FEEDBACK, OPS, Op, feedback, operand_count},
        values,
    },
    rustc_hash::{FxBuildHasher, FxHashSet},
    smallvec::SmallVec,
    std::{
        alloc::{AllocError, Allocator, Layout},
        ptr::NonNull,
    },
};

/// A growable array in the compilation's arena.
pub type List<'a, T> = Vec<T, Arena<'a>>;
type Map<'a, K, V> = hashbrown::HashMap<K, V, FxBuildHasher, Arena<'a>>;

/// A compilation's bump arena as a std allocator. Freeing is a no-op, because the whole arena
/// is reset once the compilation is done. (bumpalo's own `allocator_api` feature would replace
/// the `allocator-api2` implementation Cranelift's register allocator relies on.)
#[derive(Clone, Copy)]
pub struct Arena<'a>(pub &'a Bump);

unsafe impl Allocator for Arena<'_> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let pointer = self.0.try_alloc_layout(layout).map_err(|_| AllocError)?;
        Ok(NonNull::slice_from_raw_parts(pointer, layout.size()))
    }

    unsafe fn deallocate(&self, _pointer: NonNull<u8>, _layout: Layout) {}
}

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
    /// A boxed `Array<integer>`, `Array<double>` or `Array<logical>` of any length: a vector with
    /// the Array bit, which promises no attributes and no NA.
    IntArr,
    DblArr,
    LglArr,
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

    pub fn is_array(self) -> bool {
        matches!(self, Ty::IntArr | Ty::DblArr | Ty::LglArr)
    }

    /// The scalar type of one element.
    pub fn element(self) -> Ty {
        match self {
            Ty::IntVec | Ty::IntArr | Ty::Int => Ty::Int,
            Ty::DblVec | Ty::DblArr | Ty::Dbl => Ty::Dbl,
            Ty::LglVec | Ty::LglArr | Ty::Lgl => Ty::Lgl,
            _ => Ty::Boxed,
        }
    }

    /// The storage type of an unboxed scalar of this type.
    pub fn storage(self) -> Option<SexpType> {
        match self {
            Ty::Dbl => Some(SexpType::Real),
            Ty::Int => Some(SexpType::Integer),
            Ty::Lgl => Some(SexpType::Logical),
            _ => None,
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
pub fn from_feedback(bits: u16) -> Ty {
    let within = |mask: u16| bits & !mask == 0;
    match bits {
        0 => Ty::Boxed,
        feedback::INTEGER_SCALAR => Ty::Int,
        feedback::DOUBLE_SCALAR => Ty::Dbl,
        feedback::LOGICAL_SCALAR => Ty::Lgl,
        _ if within(feedback::INTEGER_SCALAR | feedback::INTEGER_VECTOR) => Ty::IntVec,
        _ if within(feedback::DOUBLE_SCALAR | feedback::DOUBLE_VECTOR) => Ty::DblVec,
        _ if within(feedback::LOGICAL_SCALAR | feedback::LOGICAL_VECTOR) => Ty::LglVec,
        feedback::LIST => Ty::List,
        feedback::INTEGER_ARRAY => Ty::IntArr,
        feedback::DOUBLE_ARRAY => Ty::DblArr,
        feedback::LOGICAL_ARRAY => Ty::LglArr,
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
    const UNLOADED: Slot = Slot {
        status: status::NOT_LOADED,
        ty: Ty::Bottom,
    };

    fn join(self, other: Slot) -> Slot {
        let ty = if self.status & status::LOADED == 0 && other.status & status::LOADED == 0 {
            // Both forgotten: the types are what a reload speculates on.
            self.ty.join(other.ty)
        } else if self.status & status::LOADED == 0 {
            other.ty
        } else if other.status & status::LOADED == 0 {
            self.ty
        } else {
            self.ty.join(other.ty)
        };
        // An unboxed local boxed on the way into a join is a fresh object the frame does not
        // hold, so from there on it is a value to write back, not the frame's own (see
        // `box_slot` in codegen).
        let boxed = |side: Slot| {
            side.ty.is_unboxed()
                && side.may_be(status::LOCAL)
                && ty != Ty::Bottom
                && !ty.is_unboxed()
        };
        let mut status = self.status | other.status;
        if boxed(self) || boxed(other) {
            status |= status::DIRTY;
        }
        Slot { status, ty }
    }

    pub fn may_be(self, bits: u8) -> bool {
        self.status & bits != 0
    }
}

/// The abstract state the analysis works on: one mutable row, copied into [`States`] wherever
/// the plan keeps it.
#[derive(Clone, PartialEq, Debug)]
pub struct State<'a> {
    pub registers: List<'a, Ty>,
    pub slots: List<'a, Slot>,
    /// What `R_Visible` holds, when known, so redundant stores can be skipped.
    pub visible: Option<bool>,
}

impl<'a> State<'a> {
    fn bottom(arena: Arena<'a>, registers: usize, slots: usize) -> State<'a> {
        let mut state = State {
            registers: List::with_capacity_in(registers, arena),
            slots: List::with_capacity_in(slots, arena),
            visible: None,
        };
        state.registers.resize(registers, Ty::Bottom);
        state.slots.resize(slots, Slot::UNLOADED);
        state
    }

    /// Forgets every variable, after code that may have rebound any of them ran.
    /// Forgets every variable's value but keeps its type, which a reload whose instruction
    /// has no feedback yet speculates on (and checks).
    fn forget_slots(&mut self) {
        for slot in &mut self.slots {
            slot.status = status::NOT_LOADED;
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

    fn join(&mut self, other: &State) {
        for (mine, theirs) in self.registers.iter_mut().zip(&other.registers) {
            *mine = mine.join(*theirs);
        }
        for (mine, theirs) in self.slots.iter_mut().zip(&other.slots) {
            *mine = mine.join(*theirs);
        }
        if self.visible != other.visible {
            self.visible = None;
        }
    }
}

/// A row of [`States`].
pub type StateId = u32;

/// Every abstract state a plan keeps, as rows of one matrix: a row's registers, slots and
/// visibility sit at `id * width` in three parallel arrays.
pub struct States<'a> {
    registers: usize,
    slots: usize,
    types: List<'a, Ty>,
    slot_rows: List<'a, Slot>,
    visible: List<'a, Option<bool>>,
}

/// One row of [`States`].
#[derive(Clone, Copy, Debug)]
pub struct StateRef<'p> {
    pub id: StateId,
    pub registers: &'p [Ty],
    pub slots: &'p [Slot],
    pub visible: Option<bool>,
}

impl<'a> States<'a> {
    fn new_in(arena: Arena<'a>, registers: usize, slots: usize) -> States<'a> {
        States {
            registers,
            slots,
            types: List::new_in(arena),
            slot_rows: List::new_in(arena),
            visible: List::new_in(arena),
        }
    }

    fn rows(&self) -> usize {
        self.visible.len()
    }

    /// Makes room for `rows` more rows. Growing a list in the arena copies it and abandons the
    /// old buffer, so a pass that knows roughly how many rows it adds reserves them first.
    fn reserve(&mut self, rows: usize) {
        self.types.reserve(rows * self.registers);
        self.slot_rows.reserve(rows * self.slots);
        self.visible.reserve(rows);
    }

    fn push(&mut self, state: &State) -> StateId {
        let id = self.rows() as StateId;
        self.types.extend_from_slice(&state.registers);
        self.slot_rows.extend_from_slice(&state.slots);
        self.visible.push(state.visible);
        id
    }

    fn truncate(&mut self, rows: usize) {
        self.types.truncate(rows * self.registers);
        self.slot_rows.truncate(rows * self.slots);
        self.visible.truncate(rows);
    }

    pub fn get(&self, id: StateId) -> StateRef<'_> {
        let row = id as usize;
        StateRef {
            id,
            registers: &self.types[row * self.registers..(row + 1) * self.registers],
            slots: &self.slot_rows[row * self.slots..(row + 1) * self.slots],
            visible: self.visible[row],
        }
    }

    fn load(&self, id: StateId, into: &mut State) {
        let row = self.get(id);
        into.registers.copy_from_slice(row.registers);
        into.slots.copy_from_slice(row.slots);
        into.visible = row.visible;
    }

    fn store(&mut self, id: StateId, state: &State) {
        let row = id as usize;
        self.types[row * self.registers..(row + 1) * self.registers]
            .copy_from_slice(&state.registers);
        self.slot_rows[row * self.slots..(row + 1) * self.slots].copy_from_slice(&state.slots);
        self.visible[row] = state.visible;
    }

    /// Joins row `from` into row `into`, returning whether `into` changed.
    fn join_into(&mut self, into: StateId, from: StateId) -> bool {
        let (into, from) = (into as usize, from as usize);
        let mut changed = false;
        for register in 0..self.registers {
            let (target, source) = (
                into * self.registers + register,
                from * self.registers + register,
            );
            let joined = self.types[target].join(self.types[source]);
            changed |= joined != self.types[target];
            self.types[target] = joined;
        }
        for slot in 0..self.slots {
            let (target, source) = (into * self.slots + slot, from * self.slots + slot);
            let joined = self.slot_rows[target].join(self.slot_rows[source]);
            changed |= joined != self.slot_rows[target];
            self.slot_rows[target] = joined;
        }
        if self.visible[into] != self.visible[from] && self.visible[into].is_some() {
            self.visible[into] = None;
            changed = true;
        }
        changed
    }

    /// Prepares an edge's state for joining into the block it enters: registers dead there are
    /// dropped, and at a loop header every variable may also be unloaded, since on-stack
    /// replacement enters there with the environment as the truth.
    fn adapt_edge(&mut self, id: StateId, live: &[u64], loop_header: bool) {
        let row = id as usize;
        for register in 0..self.registers {
            if !bit(live, register) {
                self.types[row * self.registers + register] = Ty::Bottom;
            }
        }
        if loop_header {
            for slot in &mut self.slot_rows[row * self.slots..(row + 1) * self.slots] {
                *slot = slot.join(Slot::UNLOADED);
            }
        }
    }
}

/// Live registers before each instruction, one bitset row per instruction.
pub struct Live<'a> {
    words: usize,
    bits: List<'a, u64>,
}

impl Live<'_> {
    fn row(&self, position: usize) -> &[u64] {
        self.bits
            .get(position * self.words..(position + 1) * self.words)
            .unwrap_or(&[])
    }

    /// Whether `register` is live before the instruction at `position`; nothing is live past the
    /// last instruction.
    pub fn get(&self, position: usize, register: usize) -> bool {
        bit(self.row(position), register)
    }
}

fn bit(words: &[u64], index: usize) -> bool {
    words
        .get(index / 64)
        .is_some_and(|word| word & (1 << (index % 64)) != 0)
}

fn set_bit(words: &mut [u64], index: usize, value: bool) {
    if let Some(word) = words.get_mut(index / 64) {
        if value {
            *word |= 1 << (index % 64);
        } else {
            *word &= !(1 << (index % 64));
        }
    }
}

/// One decoded instruction, whose operands are the bytecode's own words.
#[derive(Clone, Copy, Debug)]
pub struct Inst<'a> {
    pub pc: usize,
    pub op: Op,
    operands: &'a [i32],
}

impl Inst<'_> {
    pub fn operand(&self, index: usize) -> usize {
        self.operands[index] as usize
    }
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

/// Where a directly called closure's argument comes from.
#[derive(Clone, Copy, Debug)]
pub enum Argument {
    Unboxed {
        register: usize,
        ty: Ty,
    },
    /// A loaded variable whose boxed value is in its slot cell.
    Slot(usize),
}

/// What compiled code does at one instruction.
#[derive(Clone, Copy, Debug)]
pub enum Action<'a> {
    /// Nothing: the instruction is unreachable or folds away.
    Skip,
    Constant {
        dst: usize,
        value: Sexp,
        ty: Ty,
    },
    /// Reads a slot; `load` when the slot may not be loaded, with the type to refine to.
    /// `forcing` marks a load that may evaluate a default argument, which runs in this frame.
    GetVar {
        dst: usize,
        slot: usize,
        load: Option<Ty>,
        forcing: bool,
    },
    SetVar {
        slot: usize,
        src: usize,
    },
    /// A function lookup resolved at compile time; the speculations check it still holds.
    GetFun {
        dst: usize,
        function: Sexp,
    },
    /// A function lookup that finds a closure in a slot, loading the slot first when `load`.
    GetFunSlot {
        dst: usize,
        slot: usize,
        load: Option<Ty>,
    },
    /// A call of a closure whose body was inlined: each step with the state before it, on
    /// registers past tier 0's own. The steps have no effect but loading slots, so every check
    /// that fails in them hands the call itself back to tier 0.
    Inline {
        steps: &'a [(Action<'a>, StateId)],
    },
    /// Loads a slot without forcing a promise, handing the instruction to tier 0 when the
    /// variable is not a plain value. Only inlined calls use it.
    Peek {
        slot: usize,
        ty: Ty,
    },
    /// Copies an unboxed scalar between registers.
    Move {
        dst: usize,
        src: usize,
        ty: Ty,
    },
    /// Calls a closure known at compile time with arguments evaluated by `steps`, binding each
    /// to a promise that already holds its value, then refines the result to `dst`'s type.
    Call {
        steps: &'a [(Action<'a>, StateId)],
        function: Sexp,
        call: Sexp,
        args: &'a [Argument],
        /// The callee's annotation checks, decided from the arguments' compile-time types.
        entry: Entry,
        dst: (usize, Ty),
    },
    /// Runs the instruction in tier 0, then refines its destination register.
    Step {
        effect: Effect,
        objects: &'a [usize],
        dst: Option<(usize, Ty)>,
    },
    /// The instruction's check always passes, so control falls through.
    FallThrough,
    /// Control always goes to the jump target.
    Branch,
    BranchUnlessBuiltin {
        function: usize,
    },
    BranchUnlessPrimitive {
        function: usize,
        expected: Sexp,
    },
    BranchIfObject {
        src: usize,
    },
    Jump,
    JumpUnless {
        src: usize,
    },
    Arithmetic {
        arith: Arith,
        numeric: Numeric,
        dst: usize,
        left: usize,
        right: usize,
    },
    Comparison {
        arith: Arith,
        numeric: Numeric,
        dst: usize,
        left: usize,
        right: usize,
    },
    Intrinsic {
        intrinsic: Intrinsic,
        dst: usize,
        args: &'a [usize],
        result: Ty,
    },
    Return {
        src: usize,
    },
    ForInit {
        index: usize,
        src: usize,
        refine: Option<Ty>,
    },
    ForStep {
        index: usize,
        src: usize,
        slot: usize,
    },
    /// Releases the reference `ForInit` took on the sequence.
    ForEnd {
        src: usize,
    },
    Visible(bool),
    LogicLeft {
        dst: usize,
        src: usize,
        is_and: bool,
    },
    LogicRight {
        dst: usize,
        left: usize,
        right: usize,
        is_and: bool,
    },
    /// `x[i] <- value` in place on a plain vector, with a generic path that copies or grows.
    SubAssign {
        dst: usize,
        slot: usize,
        index: usize,
        value: usize,
    },
}

/// A function lookup compiled code assumes still resolves as it did at compile time.
#[derive(Clone, Debug)]
pub struct Speculation {
    pub symbol: Sexp,
    /// The environment the lookup starts from, which differs from the compiled body's enclosure
    /// for a function an inlined or directly called closure looks up from its own environment.
    pub env: Sexp,
    /// The binding cell the lookup found.
    pub cell: Sexp,
    pub function: Sexp,
}

pub struct Block {
    /// Index of the first instruction.
    pub start: usize,
    /// One past the last instruction.
    pub end: usize,
    pub entry: Option<StateId>,
}

/// No block starts at this pc.
const NO_BLOCK: u32 = u32::MAX;

pub struct Plan<'a> {
    pub insts: List<'a, Inst<'a>>,
    /// Registers past tier 0's that hold the unboxed values of inlined calls. They never live
    /// across an instruction and never reach tier 0.
    pub temporaries: usize,
    pub blocks: List<'a, Block>,
    /// The block each pc starts, by pc, or [`NO_BLOCK`].
    block_at: List<'a, u32>,
    pub actions: List<'a, Action<'a>>,
    /// The state before each instruction, `None` when it is unreachable.
    before: List<'a, Option<StateId>>,
    pub live: Live<'a>,
    pub slots: List<'a, Sexp>,
    pub registers: usize,
    pub loops: usize,
    pub speculations: List<'a, Speculation>,
    /// The edges leaving instruction `position` are `edges[edge_ranges[position]..edge_ranges[position + 1]]`:
    /// the state along each, by target pc, the fallthrough edge included.
    edge_ranges: List<'a, u32>,
    edges: List<'a, (usize, StateId)>,
    pub states: States<'a>,
    /// Blocks a backward jump targets, where on-stack replacement may enter.
    pub loop_headers: List<'a, usize>,
}

impl Plan<'_> {
    pub fn block_at(&self, pc: usize) -> Option<usize> {
        self.block_at
            .get(pc)
            .copied()
            .filter(|&block| block != NO_BLOCK)
            .map(|block| block as usize)
    }

    pub fn before(&self, position: usize) -> Option<StateRef<'_>> {
        self.before[position].map(|id| self.states.get(id))
    }

    pub fn edges(&self, position: usize) -> &[(usize, StateId)] {
        let (start, end) = (
            self.edge_ranges[position] as usize,
            self.edge_ranges[position + 1] as usize,
        );
        &self.edges[start..end]
    }

    pub fn state(&self, id: StateId) -> StateRef<'_> {
        self.states.get(id)
    }
}

/// The formals whose default is an expression rather than a constant or nothing.
unsafe fn defaulted_formals(formals: Sexp, arena: Arena<'_>) -> Map<'_, Sexp, ()> {
    let mut defaulted = Map::with_hasher_in(FxBuildHasher, arena);
    let mut formal = formals;
    while formal.kind() == SexpType::Pairlist {
        let default = formal.car();
        if matches!(
            default.kind(),
            SexpType::Language | SexpType::Symbol | SexpType::Promise
        ) && default != values::missing_arg()
        {
            defaulted.insert(formal.tag(), ());
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
struct Lowering<'a> {
    /// The state as of the next step.
    state: State<'a>,
    steps: List<'a, (Action<'a>, StateId)>,
    /// The next free temporary register.
    next: usize,
    /// The formals of the closure being inlined and the registers holding their arguments.
    scope: SmallVec<[(Sexp, usize); 4]>,
    /// The environment of the closure being inlined when it is not this activation's, in
    /// which case its body sees only its formals and the functions of that environment.
    closed: Option<Sexp>,
}

impl Lowering<'_> {
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

/// The refusal for a body without a loop whose compiled code would hand every instruction that
/// does real work back to tier 0, such as a wrapper around one `.Internal` call. Such code saves
/// tier 0 only its dispatch, which repays the compilation after far more calls than most such
/// bodies ever get.
pub const NOTHING_NATIVE: &str = "compiled code would run nothing natively";

impl Plan<'_> {
    /// Whether compiled code would do some of the body's work itself: a loop, or arithmetic,
    /// a comparison, an intrinsic, an element assignment, `&&` or `||`, or a call it inlines or
    /// makes directly.
    pub fn runs_natively(&self) -> bool {
        !self.loop_headers.is_empty()
            || self.actions.iter().any(|action| {
                matches!(
                    action,
                    Action::Inline { .. }
                        | Action::Call { .. }
                        | Action::Arithmetic { .. }
                        | Action::Comparison { .. }
                        | Action::Intrinsic { .. }
                        | Action::SubAssign { .. }
                        | Action::LogicLeft { .. }
                        | Action::LogicRight { .. }
                )
            })
    }
}

/// Builds the plan for `code`, a closure body whose environment's enclosure is `enclosure`, in
/// `arena`. A `patient` plan refuses as [`COLD`] when an instruction whose result type matters
/// has never run, because code compiled without that type runs the instruction in tier 0 for
/// as long as it lives.
pub unsafe fn plan<'a>(
    arena: Arena<'a>,
    code: Sexp,
    formals: Sexp,
    enclosure: Sexp,
    patient: bool,
) -> Result<Plan<'a>, Refusal> {
    let constants = code.cdr();
    let source = constants.element(bytecode::SOURCE);
    // A body that reflects on its frame can bind any symbol, which slots cannot model.
    let Some(locals) = bytecode::local_symbols(source, formals) else {
        return Err(Refusal("the body reflects on its frame"));
    };
    let insts = decode(arena, code)?;
    let registers = values::ints(constants.element(bytecode::REGISTERS))[0] as usize;
    let loops = bytecode::loop_count(code);
    let mut builder = Builder {
        arena,
        code,
        enclosure,
        feedback: values::ints(constants.element(FEEDBACK)),
        insts,
        slot_of: Map::with_hasher_in(FxBuildHasher, arena),
        slots: List::new_in(arena),
        speculated: Map::with_hasher_in(FxBuildHasher, arena),
        speculations: List::new_in(arena),
        registers,
        defaulted: defaulted_formals(formals, arena),
        locals,
        hints: List::new_in(arena),
        frame_safe: Map::with_hasher_in(FxBuildHasher, arena),
        states: States::new_in(arena, registers + TEMPORARIES, 0),
        edges: List::new_in(arena),
        patient,
    };
    builder.collect_slots();
    builder.states = States::new_in(arena, registers + TEMPORARIES, builder.slots.len());
    let (mut blocks, block_at, successors, loop_headers) = builder.blocks();
    let live = builder.liveness(&blocks, &successors);
    builder.fixpoint(&mut blocks, &block_at, &live, &loop_headers)?;
    let count = builder.insts.len();
    let mut actions = List::with_capacity_in(count, arena);
    let mut before = List::with_capacity_in(count, arena);
    before.resize(count, None);
    let mut edge_ranges = List::with_capacity_in(count + 1, arena);
    let mut edges = List::new_in(arena);
    let mut state = State::bottom(arena, registers + TEMPORARIES, builder.slots.len());
    // Each instruction adds the row before it and usually a fallthrough edge.
    builder.states.reserve(2 * count + blocks.len());
    for block in &blocks {
        let Some(entry) = block.entry else {
            for _ in block.start..block.end {
                actions.push(Action::Skip);
                edge_ranges.push(edges.len() as u32);
            }
            continue;
        };
        builder.states.load(entry, &mut state);
        for position in block.start..block.end {
            before[position] = Some(builder.states.push(&state));
            builder.edges.clear();
            actions.push(builder.transfer(position, &mut state)?);
            edge_ranges.push(edges.len() as u32);
            edges.extend_from_slice(&builder.edges);
        }
    }
    edge_ranges.push(edges.len() as u32);
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
        edge_ranges,
        edges,
        states: builder.states,
        loop_headers,
    })
}

/// Decodes the instruction words.
pub unsafe fn decode(arena: Arena<'_>, code: Sexp) -> Result<List<'_, Inst<'_>>, Refusal> {
    let words = values::ints(code.car());
    let mut insts = List::new_in(arena);
    let mut pc = 0;
    while pc < words.len() {
        let op = *OPS
            .get(words[pc] as usize)
            .ok_or(Refusal("an unknown opcode"))?;
        let count = operand_count(op);
        let operands = words
            .get(pc + 1..pc + 1 + count)
            .ok_or(Refusal("a truncated instruction"))?;
        insts.push(Inst { pc, op, operands });
        pc += 1 + count;
    }
    Ok(insts)
}

/// The base builtins that run no R code given plain arguments, so calling them leaves the
/// frame's variables alone. Internal generics among them dispatch on objects, which is why calls
/// check their boxed arguments first.
const PURE_BUILTINS: &[&str] = &[
    "+",
    "-",
    "*",
    "/",
    "^",
    "%%",
    "%/%",
    "==",
    "!=",
    "<",
    ">",
    "<=",
    ">=",
    "!",
    "&",
    "|",
    "sqrt",
    "exp",
    "log",
    "abs",
    "floor",
    "ceiling",
    "trunc",
    "round",
    "signif",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "log2",
    "log10",
    "log1p",
    "expm1",
    "length",
    "[",
    "[[",
    "c",
    "is.na",
    "is.null",
    "sum",
    "prod",
    "max",
    "min",
    "cumsum",
    "cumprod",
    "seq_len",
    "seq_along",
    ":",
    "rnorm",
    "runif",
    "rexp",
    "rbinom",
    "rpois",
    "vector",
    "numeric",
    "integer",
    "double",
    "logical",
    "character",
    "rep",
    "rep.int",
    "rep_len",
    "nchar",
    "paste",
    "paste0",
    "unlist",
    "names",
    "any",
    "all",
    "which",
    "rev",
    "as.integer",
    "as.numeric",
    "as.double",
    "as.character",
    "as.logical",
    "is.numeric",
    "is.character",
    "is.logical",
    "is.function",
    "identical",
    "range",
    "dim",
    "list",
    "abs",
    "mean",
    "var",
    "order",
    "sort",
    "max",
    "pmin",
    "pmax",
];

struct Builder<'a> {
    arena: Arena<'a>,
    code: Sexp,
    enclosure: Sexp,
    feedback: &'a [i32],
    insts: List<'a, Inst<'a>>,
    slot_of: Map<'a, Sexp, usize>,
    slots: List<'a, Sexp>,
    /// Speculation results by symbol, so each lookup resolves once.
    speculated: Map<'a, Sexp, Option<Sexp>>,
    speculations: List<'a, Speculation>,
    registers: usize,
    /// Formals whose default is an expression. Their first load may evaluate it in this frame,
    /// where it reads and may assign the body's variables.
    defaulted: Map<'a, Sexp, ()>,
    /// The formals and every symbol the body binds. A call through one of them may find a local
    /// function first, so its head cannot be speculated from the enclosure.
    locals: FxHashSet<Sexp>,
    /// Each slot's type as tier 0 saw it at its loads, for inlined code that reads a variable
    /// compiled code has not loaded.
    hints: List<'a, Ty>,
    /// Whether each closure called directly is frame-safe, so each body is walked once.
    frame_safe: Map<'a, Sexp, bool>,
    patient: bool,
    /// Every state the plan keeps, and the scratch rows of the fixpoint.
    states: States<'a>,
    /// The edges leaving the instruction [`Builder::transfer`] last ran, by target pc.
    edges: List<'a, (usize, StateId)>,
}

impl<'a> Builder<'a> {
    unsafe fn constant(&self, index: usize) -> Sexp {
        self.code.cdr().element(index)
    }

    unsafe fn collect_slots(&mut self) {
        for position in 0..self.insts.len() {
            let inst = &self.insts[position];
            let symbol_operand = match inst.op {
                Op::GetVar => Some(inst.operand(1)),
                Op::SetVar => Some(inst.operand(0)),
                Op::ForStep => Some(inst.operand(2)),
                Op::SubAssign => Some(inst.operand(1)),
                _ => None,
            };
            if let Some(operand) = symbol_operand {
                let symbol = self.constant(operand);
                if !self.slot_of.contains_key(&symbol) {
                    self.slot_of.insert(symbol, self.slots.len());
                    self.slots.push(symbol);
                    self.hints.push(Ty::Bottom);
                }
                let bits = self.feedback.get(inst.pc).map_or(0, |&bits| bits as u16);
                if inst.op == Op::GetVar && bits != 0 {
                    let slot = self.slot_of[&symbol];
                    self.hints[slot] = self.hints[slot].join(from_feedback(bits));
                }
            }
        }
    }

    fn slot(&self, symbol: Sexp) -> usize {
        self.slot_of[&symbol]
    }

    fn feedback_at(&self, pc: usize) -> Ty {
        from_feedback(self.feedback.get(pc).map_or(0, |&bits| bits as u16))
    }

    /// Whether the instruction at `pc` has never recorded a value.
    fn never_ran(&self, pc: usize) -> bool {
        self.feedback.get(pc).copied().unwrap_or(0) == 0
    }

    /// The type a call of `function` at `pc` is expected to return: what the call site has
    /// returned so far, or before it has ever returned, the type the callee declares.
    unsafe fn call_result_at(&self, pc: usize, function: Sexp) -> Ty {
        if !self.never_ran(pc) {
            return self.feedback_at(pc);
        }
        match annotations::declared_return_type(function) {
            Some(SexpType::Real) => Ty::Dbl,
            Some(SexpType::Integer) => Ty::Int,
            Some(SexpType::Logical) => Ty::Lgl,
            _ => Ty::Boxed,
        }
    }

    /// Splits the instructions into blocks and finds each block's successors and the loop
    /// headers (the blocks a backward jump targets).
    #[allow(clippy::type_complexity)]
    fn blocks(
        &self,
    ) -> (
        List<'a, Block>,
        List<'a, u32>,
        List<'a, [Option<usize>; 2]>,
        List<'a, usize>,
    ) {
        let words = self
            .insts
            .last()
            .map_or(0, |last| last.pc + 1 + last.operands.len());
        let mut leaders = List::new_in(self.arena);
        leaders.resize(words.div_ceil(64).max(1), 0u64);
        set_bit(&mut leaders, 0, true);
        for (position, inst) in self.insts.iter().enumerate() {
            if let Some(target) = jump_target(inst) {
                set_bit(&mut leaders, target, true);
            }
            if ends_block(inst.op)
                && let Some(next) = self.insts.get(position + 1)
            {
                set_bit(&mut leaders, next.pc, true);
            }
        }
        let mut blocks: List<'a, Block> = List::new_in(self.arena);
        let mut block_at = List::with_capacity_in(words, self.arena);
        block_at.resize(words, NO_BLOCK);
        for (position, inst) in self.insts.iter().enumerate() {
            if bit(&leaders, inst.pc) {
                if let Some(last) = blocks.last_mut() {
                    last.end = position;
                }
                block_at[inst.pc] = blocks.len() as u32;
                blocks.push(Block {
                    start: position,
                    end: position + 1,
                    entry: None,
                });
            }
        }
        if let Some(last) = blocks.last_mut() {
            last.end = self.insts.len();
        }
        let lookup = |pc: usize| {
            block_at
                .get(pc)
                .copied()
                .filter(|&block| block != NO_BLOCK)
                .map(|block| block as usize)
        };
        let mut successors = List::with_capacity_in(blocks.len(), self.arena);
        for block in &blocks {
            let last = &self.insts[block.end - 1];
            let fallthrough = if falls_through(last.op) && block.end < self.insts.len() {
                lookup(self.insts[block.end].pc)
            } else {
                None
            };
            successors.push([fallthrough, jump_target(last).and_then(lookup)]);
        }
        let mut loop_headers = List::new_in(self.arena);
        for inst in self
            .insts
            .iter()
            .filter(|inst| inst.op == Op::Jump && inst.operand(0) <= inst.pc)
        {
            if let Some(header) = lookup(inst.operand(0)) {
                loop_headers.push(header);
            }
        }
        loop_headers.sort_unstable();
        loop_headers.dedup();
        (blocks, block_at, successors, loop_headers)
    }

    /// Registers live before each instruction, as tier 0 sees them (every path, including the
    /// fallback paths compiled code never takes, since deoptimization can reach them).
    fn liveness(&self, blocks: &[Block], successors: &[[Option<usize>; 2]]) -> Live<'a> {
        let words = self.registers.div_ceil(64).max(1);
        let mut live_in = List::new_in(self.arena);
        live_in.resize(blocks.len() * words, 0u64);
        let mut live = List::new_in(self.arena);
        live.resize(words, 0u64);
        let live_out = |live: &mut [u64], live_in: &[u64], index: usize| {
            live.fill(0);
            for &successor in successors[index].iter().flatten() {
                for (word, incoming) in live
                    .iter_mut()
                    .zip(&live_in[successor * words..(successor + 1) * words])
                {
                    *word |= incoming;
                }
            }
        };
        let mut changed = true;
        while changed {
            changed = false;
            for index in (0..blocks.len()).rev() {
                live_out(&mut live, &live_in, index);
                for position in (blocks[index].start..blocks[index].end).rev() {
                    apply_liveness(&self.insts[position], &mut live);
                }
                let row = &mut live_in[index * words..(index + 1) * words];
                if *row != *live {
                    row.copy_from_slice(&live);
                    changed = true;
                }
            }
        }
        let mut bits = List::new_in(self.arena);
        bits.resize(self.insts.len() * words, 0u64);
        for (index, block) in blocks.iter().enumerate() {
            live_out(&mut live, &live_in, index);
            for position in (block.start..block.end).rev() {
                apply_liveness(&self.insts[position], &mut live);
                bits[position * words..(position + 1) * words].copy_from_slice(&live);
            }
        }
        Live { words, bits }
    }

    /// Computes every reachable block's entry state. A loop header is also where on-stack
    /// replacement enters with every variable still in the environment, so its entry state
    /// allows every slot to be unloaded.
    ///
    /// Each block's entry is a row reserved up front; the rows a block's transfer adds (its
    /// edges and inlined steps) are dropped once they are joined into the targets' entries.
    unsafe fn fixpoint(
        &mut self,
        blocks: &mut [Block],
        block_at: &[u32],
        live: &Live,
        loop_headers: &[usize],
    ) -> Result<(), Refusal> {
        let mut state = State::bottom(self.arena, self.registers + TEMPORARIES, self.slots.len());
        // One entry row per block, then the edge rows of whichever block is being visited.
        let longest = blocks
            .iter()
            .map(|block| block.end - block.start)
            .max()
            .unwrap_or(0);
        self.states.reserve(blocks.len() + 2 * longest);
        for _ in 0..blocks.len() {
            self.states.push(&state);
        }
        let reserved = self.states.rows();
        let mut reached = state.clone();
        blocks[0].entry = Some(0);
        let mut work = List::new_in(self.arena);
        work.push(0usize);
        let mut rounds = 0usize;
        while let Some(index) = work.pop() {
            rounds += 1;
            if rounds > 100_000 {
                return Err(Refusal("the type analysis did not converge"));
            }
            let Some(entry) = blocks[index].entry else {
                continue;
            };
            self.states.load(entry, &mut state);
            for position in blocks[index].start..blocks[index].end {
                self.edges.clear();
                self.transfer(position, &mut state)?;
            }
            for edge in 0..self.edges.len() {
                let (target_pc, out) = self.edges[edge];
                let Some(target) = block_at
                    .get(target_pc)
                    .copied()
                    .filter(|&block| block != NO_BLOCK)
                else {
                    continue;
                };
                let target = target as usize;
                self.states.adapt_edge(
                    out,
                    live.row(blocks[target].start),
                    loop_headers.contains(&target),
                );
                let changed = match blocks[target].entry {
                    Some(existing) => self.states.join_into(existing, out),
                    None => {
                        self.states.load(out, &mut reached);
                        self.states.store(target as StateId, &reached);
                        blocks[target].entry = Some(target as StateId);
                        true
                    }
                };
                if changed && !work.contains(&target) {
                    work.push(target);
                }
            }
            self.states.truncate(reserved);
        }
        Ok(())
    }

    /// The action for the instruction at `position` and its effect on `state`. It also adds
    /// the state along each outgoing edge to [`Builder::edges`].
    unsafe fn transfer(
        &mut self,
        position: usize,
        state: &mut State<'a>,
    ) -> Result<Action<'a>, Refusal> {
        let inst = self.insts[position];
        let operand = |index: usize| inst.operand(index);
        let next_pc = self.insts.get(position + 1).map(|next| next.pc);
        let action = match inst.op {
            Op::Constant => {
                let value = self.constant(operand(1));
                let ty = constant_type(value);
                state.registers[operand(0)] = ty;
                state.visible = Some(true);
                Action::Constant {
                    dst: operand(0),
                    value,
                    ty,
                }
            }
            Op::GetVar => {
                let slot = self.slot(self.constant(operand(1)));
                let current = state.slots[slot];
                let load = if current.may_be(status::NOT_LOADED) {
                    let ty = if current.may_be(status::LOADED) {
                        current.ty
                    } else if self.never_ran(inst.pc) && current.ty != Ty::Bottom {
                        // A variable forgotten across an opaque call, read where tier 0 has not
                        // read it yet (a branch not taken so far): the type it had before the call
                        // is a better guess than a boxed unknown, which leaves its arithmetic to
                        // tier 0 for good.
                        current.ty
                    } else {
                        self.feedback_at(inst.pc)
                    };
                    Some(if ty == Ty::Bottom { Ty::Boxed } else { ty })
                } else {
                    None
                };
                let forcing = load.is_some() && self.defaulted.contains_key(&self.slots[slot]);
                if let Some(ty) = load {
                    let mut status_bits = current.status & status::LOADED;
                    status_bits |= status::LOCAL | status::ENCLOSING;
                    let loaded = Slot {
                        status: status_bits,
                        ty,
                    };
                    // Code generation writes the variables back before the load and forgets them
                    // after it, but only when the load really happens.
                    let mut forced = state.clone();
                    forced.flushed();
                    forced.forget_slots();
                    forced.slots[slot] = loaded;
                    state.slots[slot] = loaded;
                    if forcing {
                        if current.may_be(status::LOADED) {
                            state.join(&forced);
                        } else {
                            *state = forced;
                        }
                    }
                }
                state.registers[operand(0)] = state.slots[slot].ty;
                // A load can force a promise to `invisible()`, which leaves the value invisible,
                // as `withCallingHandlers(expr)` relies on.
                state.visible = if load.is_some() { None } else { Some(true) };
                Action::GetVar {
                    dst: operand(0),
                    slot,
                    load,
                    forcing,
                }
            }
            Op::SetVar => {
                let slot = self.slot(self.constant(operand(0)));
                let ty = match state.registers[operand(1)] {
                    Ty::Bottom => Ty::Boxed,
                    ty => ty,
                };
                state.slots[slot] = Slot {
                    status: status::DIRTY,
                    ty,
                };
                state.visible = Some(false);
                Action::SetVar {
                    slot,
                    src: operand(1),
                }
            }
            Op::GetFun => {
                let symbol = self.constant(operand(1));
                if let Some(&slot) = self.slot_of.get(&symbol)
                    && let Ty::Closure(..) = state.slots[slot].ty
                    && !self.defaulted.contains_key(&symbol)
                {
                    let current = state.slots[slot];
                    let load = current.may_be(status::NOT_LOADED).then_some(current.ty);
                    if load.is_some() {
                        state.slots[slot] = Slot {
                            status: (current.status & status::LOADED)
                                | status::LOCAL
                                | status::ENCLOSING,
                            ty: current.ty,
                        };
                    }
                    state.registers[operand(0)] = current.ty;
                    return Ok(self.finish(
                        Action::GetFunSlot {
                            dst: operand(0),
                            slot,
                            load,
                        },
                        inst.op,
                        next_pc,
                        state,
                    ));
                }
                match self.speculate(symbol) {
                    Some(function) if !self.bound_locally(symbol) => {
                        state.registers[operand(0)] = Ty::Fun(function);
                        Action::GetFun {
                            dst: operand(0),
                            function,
                        }
                    }
                    _ => {
                        self.opaque_step(state, Some((operand(0), Ty::Boxed)), &[], Effect::Opaque)
                    }
                }
            }
            Op::BranchUnlessBuiltin => match state.registers[operand(0)] {
                Ty::Fun(function) if function.kind() == SexpType::Builtin => Action::FallThrough,
                Ty::Fun(_) | Ty::Closure(..) => {
                    self.push_edge(operand(1), state);
                    return Ok(Action::Branch);
                }
                _ => {
                    self.push_edge(operand(1), state);
                    Action::BranchUnlessBuiltin {
                        function: operand(0),
                    }
                }
            },
            Op::BranchUnlessPrimitive => {
                let expected = self.constant(operand(1));
                match state.registers[operand(0)] {
                    Ty::Fun(function) if function == expected => Action::FallThrough,
                    Ty::Fun(_) | Ty::Closure(..) => {
                        self.push_edge(operand(2), state);
                        return Ok(Action::Branch);
                    }
                    _ => {
                        self.push_edge(operand(2), state);
                        Action::BranchUnlessPrimitive {
                            function: operand(0),
                            expected,
                        }
                    }
                }
            }
            Op::BranchIfObject => {
                if state.registers[operand(0)].is_plain() {
                    Action::FallThrough
                } else {
                    self.push_edge(operand(1), state);
                    Action::BranchIfObject { src: operand(0) }
                }
            }
            Op::DispatchSubject => self.opaque_step(
                state,
                Some((operand(0), self.feedback_at(inst.pc))),
                &[],
                Effect::Opaque,
            ),
            Op::CallBuiltin => self.call_builtin(&inst, state)?,
            Op::CallGeneric | Op::CallRebinding => {
                let call = self.constant(operand(2));
                // `x <- f(x, ...)` may move a ry dictionary or set into the call to update it in
                // place, which only tier 0 does, so the call is inlined only when the variable
                // cannot hold one.
                let inlinable = inst.op == Op::CallGeneric
                    || self
                        .slot_of
                        .get(&self.constant(operand(3)))
                        .is_some_and(|&slot| state.slots[slot].ty.is_plain());
                let inlined = match state.registers[operand(1)] {
                    _ if !inlinable => None,
                    Ty::Closure(formals, body) => {
                        self.inline(state, call, formals, body, None, operand(0))
                    }
                    Ty::Fun(function) if function.kind() == SexpType::Closure => self
                        .inline(
                            state,
                            call,
                            function.car(),
                            bytecode::closure_body(function),
                            Some(function.tag()),
                            operand(0),
                        )
                        .or_else(|| {
                            self.direct_call(
                                state,
                                call,
                                function,
                                (operand(0), self.call_result_at(inst.pc, function)),
                            )
                        }),
                    _ => None,
                };
                match inlined {
                    Some(action) => action,
                    None => self.opaque_step(
                        state,
                        Some((operand(0), self.feedback_at(inst.pc))),
                        &[],
                        Effect::Opaque,
                    ),
                }
            }
            Op::Ast => self.opaque_step(
                state,
                Some((operand(0), self.feedback_at(inst.pc))),
                &[],
                Effect::Opaque,
            ),
            Op::Guard => {
                let symbol = self.constant(operand(0));
                let expected = self.constant(operand(1));
                if operand(3) != 0 || self.bound_locally(symbol) {
                    return Err(Refusal("the frame may rebind a syntax function"));
                }
                if self.speculate(symbol) != Some(expected) {
                    return Err(Refusal("a syntax function is redefined"));
                }
                Action::FallThrough
            }
            Op::Jump => {
                self.push_edge(operand(0), state);
                return Ok(Action::Jump);
            }
            Op::JumpUnless => {
                self.push_edge(operand(2), state);
                Action::JumpUnless { src: operand(0) }
            }
            Op::Arithmetic => self.arithmetic(&inst, state)?,
            Op::Return => Action::Return { src: operand(0) },
            Op::ForInit => {
                let src = operand(1);
                let ty = state.registers[src];
                let refine = match ty {
                    // Code generation boxes an unboxed scalar sequence before the loop.
                    Ty::IntVec
                    | Ty::DblVec
                    | Ty::LglVec
                    | Ty::List
                    | Ty::Int
                    | Ty::Dbl
                    | Ty::Lgl => None,
                    _ => {
                        if self.never_ran(inst.pc) {
                            return Err(Refusal(if self.patient {
                                COLD
                            } else {
                                "a for loop that has never run"
                            }));
                        }
                        let speculated = self.feedback_at(inst.pc);
                        if !matches!(
                            speculated,
                            Ty::IntVec
                                | Ty::DblVec
                                | Ty::LglVec
                                | Ty::List
                                | Ty::Int
                                | Ty::Dbl
                                | Ty::Lgl
                        ) {
                            return Err(Refusal(
                                "a for loop over something other than a plain vector",
                            ));
                        }
                        Some(speculated.vector())
                    }
                };
                state.registers[src] = refine.unwrap_or(ty.vector());
                Action::ForInit {
                    index: operand(0),
                    src,
                    refine,
                }
            }
            Op::ForStep => {
                let slot = self.slot(self.constant(operand(2)));
                self.push_edge(operand(3), state);
                let element = state.registers[operand(1)].element();
                state.slots[slot] = Slot {
                    status: status::DIRTY,
                    ty: element,
                };
                Action::ForStep {
                    index: operand(0),
                    src: operand(1),
                    slot,
                }
            }
            Op::ForEnd => Action::ForEnd { src: operand(0) },
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
                self.push_edge(operand(4), &decided);
                state.registers[operand(1)] = Ty::Lgl;
                Action::LogicLeft {
                    dst: operand(0),
                    src: operand(1),
                    is_and: operand(3) == 1,
                }
            }
            Op::LogicRight => {
                state.registers[operand(0)] = Ty::Lgl;
                state.visible = Some(true);
                Action::LogicRight {
                    dst: operand(0),
                    left: operand(1),
                    right: operand(2),
                    is_and: operand(4) == 1,
                }
            }
            // The interpreter's own step sets the current srcref; it touches no register.
            Op::SetSrcref => Action::Step {
                effect: Effect::Pure,
                objects: &[],
                dst: None,
            },
            Op::Closure => {
                state.visible = Some(true);
                let ty = Ty::Closure(self.constant(operand(1)), self.constant(operand(2)));
                state.registers[operand(0)] = ty;
                Action::Step {
                    effect: Effect::Pure,
                    objects: &[],
                    dst: Some((operand(0), ty)),
                }
            }
            Op::SubAssign => {
                let slot = self.slot(self.constant(operand(1)));
                let (index, value) = (state.registers[operand(2)], state.registers[operand(3)]);
                let target = state.slots[slot].ty;
                let fits = match target {
                    Ty::DblVec => value.is_numeric_scalar(),
                    Ty::IntVec => matches!(value, Ty::Int | Ty::Lgl),
                    Ty::LglVec => value == Ty::Lgl,
                    // An Array takes only items of its own type; anything else turns it into
                    // an R vector, which tier 0 does.
                    Ty::IntArr | Ty::DblArr | Ty::LglArr => value == target.element(),
                    _ => false,
                };
                if fits
                    && matches!(index, Ty::Int | Ty::Dbl)
                    && state.slots[slot].may_be(status::LOADED)
                    && !(self.defaulted.contains_key(&self.slots[slot])
                        && state.slots[slot].may_be(status::NOT_LOADED))
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
                    Action::SubAssign {
                        dst: operand(0),
                        slot,
                        index: operand(2),
                        value: operand(3),
                    }
                } else {
                    let refine = match value {
                        Ty::Bottom => Ty::Boxed,
                        ty => ty,
                    };
                    let action =
                        self.opaque_step(state, Some((operand(0), refine)), &[], Effect::Opaque);
                    state.visible = Some(false);
                    action
                }
            }
        };
        Ok(self.finish(action, inst.op, next_pc, state))
    }

    fn push_edge(&mut self, target: usize, state: &State) {
        let id = self.states.push(state);
        self.edges.push((target, id));
    }

    /// Adds the fallthrough edge. Every instruction reports it: the fixpoint reads the edges of a
    /// block's last instruction, and code generation reads the state after any instruction.
    fn finish(
        &mut self,
        action: Action<'a>,
        op: Op,
        next_pc: Option<usize>,
        state: &State,
    ) -> Action<'a> {
        if falls_through(op)
            && let Some(next) = next_pc
            && !matches!(action, Action::Branch)
        {
            self.push_edge(next, state);
        }
        action
    }

    /// Adds a step to an inlined call, with the state before it.
    fn push_step(&mut self, lowering: &mut Lowering<'a>, action: Action<'a>) {
        let id = self.states.push(&lowering.state);
        lowering.steps.push((action, id));
    }

    /// Copies `items` into the arena.
    fn slice<T: Copy>(&self, items: impl IntoIterator<Item = T>) -> &'a [T] {
        let mut list = List::new_in(self.arena);
        list.extend(items);
        list.leak()
    }

    /// Inlines a call of the closure `formals`/`body` made in this activation, when the call and
    /// the body compute an unboxed scalar from scalars without any effect: arithmetic,
    /// comparisons, inline builtins and further such calls. The arguments are evaluated before
    /// the body, which R's lazy evaluation cannot observe because nothing in between has an
    /// effect, and an argument that would fail or is not a plain scalar hands the whole call
    /// back to tier 0, which then evaluates it lazily.
    unsafe fn inline(
        &mut self,
        state: &mut State<'a>,
        call: Sexp,
        formals: Sexp,
        body: Sexp,
        closed: Option<Sexp>,
        dst: usize,
    ) -> Option<Action<'a>> {
        let mut lowering = Lowering {
            state: state.clone(),
            steps: List::new_in(self.arena),
            next: self.registers,
            scope: SmallVec::new(),
            closed: None,
        };
        let result = self.lower_call(&mut lowering, call, formals, body, closed, 0)?;
        let ty = lowering.state.registers[result];
        self.push_step(
            &mut lowering,
            Action::Move {
                dst,
                src: result,
                ty,
            },
        );
        let mut after = lowering.state;
        for register in self.registers..after.registers.len() {
            after.registers[register] = Ty::Bottom;
        }
        after.registers[dst] = ty;
        after.visible = Some(true);
        *state = after;
        Some(Action::Inline {
            steps: lowering.steps.leak(),
        })
    }

    /// Calls `function` directly when every argument is a scalar computed without effects or a
    /// variable, which then need no promise to evaluate: each is bound to a promise that
    /// already holds its value, so `substitute` still sees the argument's expression.
    ///
    /// Evaluating an argument before the call is only invisible when nothing can change what it
    /// reads before the callee forces it, so the callee must be frame-safe.
    unsafe fn direct_call(
        &mut self,
        state: &mut State<'a>,
        call: Sexp,
        function: Sexp,
        dst: (usize, Ty),
    ) -> Option<Action<'a>> {
        if !self.frame_safe(function) {
            return None;
        }
        let mut lowering = Lowering {
            state: state.clone(),
            steps: List::new_in(self.arena),
            next: self.registers,
            scope: SmallVec::new(),
            closed: None,
        };
        let mut args = List::new_in(self.arena);
        for (tag, expression) in values::pairlist_items(call.cdr()) {
            if tag != values::nil()
                || expression == values::missing_arg()
                || expression == values::symbols().dots
            {
                return None;
            }
            if expression.kind() == SexpType::Symbol
                && let Some(slot) = self.peek_slot(&mut lowering, expression, true)
                && !lowering.state.slots[slot].ty.is_unboxed()
            {
                args.push(Argument::Slot(slot));
                continue;
            }
            let register = self.lower_expression(&mut lowering, expression, 0)?;
            args.push(Argument::Unboxed {
                register,
                ty: lowering.state.registers[register],
            });
        }
        let mut after = lowering.state;
        for register in self.registers..after.registers.len() {
            after.registers[register] = Ty::Bottom;
        }
        // The callee may reach this frame through `parent.frame()`, so the call writes the
        // variables back and forgets them like any opaque step.
        after.flushed();
        after.forget_slots();
        after.visible = None;
        after.registers[dst.0] = dst.1;
        *state = after;
        let kinds: SmallVec<[Option<SexpType>; 4]> = args
            .iter()
            .map(|argument| match *argument {
                Argument::Unboxed { ty, .. } => ty.storage(),
                _ => None,
            })
            .collect();
        let entry = annotations::entry_for(function, &kinds);
        Some(Action::Call {
            steps: lowering.steps.leak(),
            function,
            call,
            args: args.leak(),
            entry,
            dst,
        })
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn lower_call(
        &mut self,
        lowering: &mut Lowering<'a>,
        call: Sexp,
        formals: Sexp,
        body: Sexp,
        closed: Option<Sexp>,
        depth: usize,
    ) -> Option<usize> {
        if depth > MAX_INLINE_DEPTH {
            return None;
        }
        let args = values::pairlist_items(call.cdr());
        let parameters = values::pairlist_items(formals);
        let positional = args.iter().all(|(tag, value)| {
            *tag == values::nil()
                && *value != values::missing_arg()
                && *value != values::symbols().dots
        });
        // Every formal needs its argument: a default is evaluated in the callee's frame, which
        // the inlined body does not have.
        if !positional
            || args.len() != parameters.len()
            || parameters
                .iter()
                .any(|(symbol, _)| *symbol == values::symbols().dots)
        {
            return None;
        }
        let outer = std::mem::take(&mut lowering.scope);
        let mut scope: SmallVec<[usize; 4]> = SmallVec::with_capacity(args.len());
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
        lowering.scope = parameters
            .iter()
            .map(|(symbol, _)| *symbol)
            .zip(scope)
            .collect();
        let outer_closed = std::mem::replace(&mut lowering.closed, closed);
        let result = self.lower_expression(lowering, body, depth);
        let arguments: SmallVec<[Option<SexpType>; 4]> = lowering
            .scope
            .iter()
            .map(|&(_, register)| lowering.state.registers[register].storage())
            .collect();
        lowering.scope = outer;
        lowering.closed = outer_closed;
        let result = result?;
        // Inlined code has no frame to check annotations in, so a typed closure is inlined only
        // when its checks are known to pass; tier 0 runs any other call and reports the failure.
        let returned = lowering.state.registers[result].storage();
        annotations::admits_inline(formals, body, &arguments, returned).then_some(result)
    }

    /// Lowers an expression of an inlined call into steps, returning the register that holds
    /// its unboxed value. Symbols resolve in `lowering.scope` (the formals of the closure being
    /// inlined) and then in this activation's slots, which is where both the call's arguments
    /// and the free variables of a closure made in this activation are found.
    unsafe fn lower_expression(
        &mut self,
        lowering: &mut Lowering<'a>,
        expression: Sexp,
        depth: usize,
    ) -> Option<usize> {
        match expression.kind() {
            SexpType::Integer | SexpType::Real | SexpType::Logical => {
                let ty = constant_type(expression);
                if !ty.is_unboxed() {
                    return None;
                }
                let dst = lowering.temporary()?;
                self.push_step(
                    lowering,
                    Action::Constant {
                        dst,
                        value: expression,
                        ty,
                    },
                );
                lowering.state.registers[dst] = ty;
                Some(dst)
            }
            SexpType::Symbol => {
                if let Some(&(_, register)) = lowering
                    .scope
                    .iter()
                    .find(|(symbol, _)| *symbol == expression)
                {
                    return Some(register);
                }
                if lowering.closed.is_some() {
                    return None;
                }
                let slot = self.peek_slot(lowering, expression, false)?;
                let ty = lowering.state.slots[slot].ty;
                if !ty.is_unboxed() {
                    return None;
                }
                let dst = lowering.temporary()?;
                self.push_step(
                    lowering,
                    Action::GetVar {
                        dst,
                        slot,
                        load: None,
                        forcing: false,
                    },
                );
                lowering.state.registers[dst] = ty;
                Some(dst)
            }
            SexpType::Language => {
                let head = expression.car();
                if head.kind() != SexpType::Symbol {
                    return None;
                }
                let args: SmallVec<[Sexp; 4]> = values::pairlist_items(expression.cdr())
                    .iter()
                    .map(|(tag, value)| {
                        if *tag == values::nil() {
                            *value
                        } else {
                            values::missing_arg()
                        }
                    })
                    .collect();
                if args.contains(&values::missing_arg()) {
                    return None;
                }
                if lowering.scope.iter().any(|(symbol, _)| *symbol == head) {
                    return None;
                }
                if lowering.closed.is_none()
                    && let Some(&slot) = self.slot_of.get(&head)
                {
                    // A closure this activation made, called from inlined code: inline it too.
                    self.peek_slot(lowering, head, false)?;
                    let Ty::Closure(formals, body) = lowering.state.slots[slot].ty else {
                        return None;
                    };
                    return self.lower_call(lowering, expression, formals, body, None, depth + 1);
                }
                let function = self.resolve_head(lowering, head)?;
                if function.kind() == SexpType::Closure {
                    let body = bytecode::closure_body(function);
                    return self.lower_call(
                        lowering,
                        expression,
                        function.car(),
                        body,
                        Some(function.tag()),
                        depth + 1,
                    );
                }
                let name = base_builtin(function, head)?;
                let registers = args
                    .iter()
                    .map(|&arg| self.lower_expression(lowering, arg, depth))
                    .collect::<Option<SmallVec<[usize; 4]>>>()?;
                let types: SmallVec<[Ty; 4]> = registers
                    .iter()
                    .map(|&register| lowering.state.registers[register])
                    .collect();
                match (name.as_str(), registers.as_slice()) {
                    ("(" | "{", [inner]) => Some(*inner),
                    (operator, [left, right])
                        if let Some(&(arith, _)) =
                            ARITHS.iter().find(|(_, symbol)| *symbol == operator) =>
                    {
                        let (a, b) = (types[0], types[1]);
                        let dst = lowering.temporary()?;
                        let integers = a != Ty::Dbl && b != Ty::Dbl;
                        let comparison = matches!(
                            arith,
                            Arith::Equal
                                | Arith::NotEqual
                                | Arith::Less
                                | Arith::Greater
                                | Arith::LessEqual
                                | Arith::GreaterEqual
                        );
                        let (action, ty) = if comparison {
                            let numeric = if integers { Numeric::Int } else { Numeric::Dbl };
                            (
                                Action::Comparison {
                                    arith,
                                    numeric,
                                    dst,
                                    left: *left,
                                    right: *right,
                                },
                                Ty::Lgl,
                            )
                        } else {
                            let numeric = match arith {
                                Arith::Add | Arith::Subtract | Arith::Multiply if integers => {
                                    Numeric::Int
                                }
                                _ => Numeric::Dbl,
                            };
                            let ty = if numeric == Numeric::Int {
                                Ty::Int
                            } else {
                                Ty::Dbl
                            };
                            (
                                Action::Arithmetic {
                                    arith,
                                    numeric,
                                    dst,
                                    left: *left,
                                    right: *right,
                                },
                                ty,
                            )
                        };
                        self.push_step(lowering, action);
                        lowering.state.registers[dst] = ty;
                        Some(dst)
                    }
                    (name, _) => {
                        let (intrinsic, result) = intrinsic_for(name, &types)?;
                        if !result.is_unboxed() {
                            return None;
                        }
                        let dst = lowering.temporary()?;
                        let args = self.slice(registers);
                        self.push_step(
                            lowering,
                            Action::Intrinsic {
                                intrinsic,
                                dst,
                                args,
                                result,
                            },
                        );
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
    /// A variable compiled code has not loaded is assumed to have the type tier 0 saw at its
    /// loads, which the peek checks. Only a `boxed` peek may find a boxed value.
    unsafe fn peek_slot(
        &mut self,
        lowering: &mut Lowering<'a>,
        symbol: Sexp,
        boxed: bool,
    ) -> Option<usize> {
        let slot = *self.slot_of.get(&symbol)?;
        if self.defaulted.contains_key(&symbol) {
            return None;
        }
        let current = lowering.state.slots[slot];
        if current.may_be(status::NOT_LOADED) {
            let ty = if current.may_be(status::LOADED) {
                current.ty
            } else {
                self.hints[slot]
            };
            if ty == Ty::Bottom || (ty == Ty::Boxed && !boxed) {
                return None;
            }
            self.push_step(lowering, Action::Peek { slot, ty });
            lowering.state.slots[slot] = Slot {
                status: (current.status & status::LOADED) | status::LOCAL | status::ENCLOSING,
                ty,
            };
        }
        Some(slot)
    }

    /// The function `symbol` calls from inlined code that is not a formal of the closure being
    /// inlined, found from that closure's environment and recorded as a speculation.
    unsafe fn resolve_head(&mut self, lowering: &Lowering<'a>, symbol: Sexp) -> Option<Sexp> {
        match lowering.closed {
            None if self.bound_locally(symbol) => None,
            None => self.speculate(symbol),
            Some(env) if env == self.enclosure => self.speculate(symbol),
            Some(env) => {
                let speculation = resolve_function(symbol, env)?;
                let function = speculation.function;
                self.speculations.push(speculation);
                Some(function)
            }
        }
    }

    /// A tier-0 step whose destination is refined to `dst`'s type afterwards.
    fn opaque_step(
        &mut self,
        state: &mut State<'a>,
        dst: Option<(usize, Ty)>,
        objects: &'a [usize],
        effect: Effect,
    ) -> Action<'a> {
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
        Action::Step {
            effect,
            objects,
            dst,
        }
    }

    unsafe fn arithmetic(
        &mut self,
        inst: &Inst<'a>,
        state: &mut State<'a>,
    ) -> Result<Action<'a>, Refusal> {
        let operand = |index: usize| inst.operand(index);
        let arith = ARITHS[operand(0)].0;
        let (dst, left, right) = (operand(1), operand(2), operand(3));
        let (a, b) = (state.registers[left], state.registers[right]);
        state.visible = Some(true);
        if a.is_numeric_scalar() && b.is_numeric_scalar() {
            let integers = !matches!(a, Ty::Dbl) && !matches!(b, Ty::Dbl);
            let comparison = matches!(
                arith,
                Arith::Equal
                    | Arith::NotEqual
                    | Arith::Less
                    | Arith::Greater
                    | Arith::LessEqual
                    | Arith::GreaterEqual
            );
            if comparison {
                state.registers[dst] = Ty::Lgl;
                let numeric = if integers { Numeric::Int } else { Numeric::Dbl };
                return Ok(Action::Comparison {
                    arith,
                    numeric,
                    dst,
                    left,
                    right,
                });
            }
            let numeric = match arith {
                Arith::Add | Arith::Subtract | Arith::Multiply if integers => Numeric::Int,
                _ => Numeric::Dbl,
            };
            state.registers[dst] = if numeric == Numeric::Int {
                Ty::Int
            } else {
                Ty::Dbl
            };
            return Ok(Action::Arithmetic {
                arith,
                numeric,
                dst,
                left,
                right,
            });
        }
        // An operand of unknown type is typically the result of a call that has not returned
        // yet, as in a recursive function compiled on its way down.
        if self.patient && (a == Ty::Boxed || b == Ty::Boxed) && self.never_ran(inst.pc) {
            return Err(Refusal(COLD));
        }
        let effect = if a.is_plain() && b.is_plain() {
            Effect::PureUnlessObserved
        } else {
            Effect::Opaque
        };
        let ty = self.feedback_at(inst.pc);
        let action = self.opaque_step(state, Some((dst, ty)), &[], effect);
        state.visible = Some(true);
        Ok(action)
    }

    unsafe fn call_builtin(
        &mut self,
        inst: &Inst<'a>,
        state: &mut State<'a>,
    ) -> Result<Action<'a>, Refusal> {
        let operand = |index: usize| inst.operand(index);
        let dst = operand(0);
        let call = self.constant(operand(2));
        let (first, count) = (operand(3), operand(4));
        let args = self.slice(first..first + count);
        let feedback = self.feedback_at(inst.pc);
        let Ty::Fun(function) = state.registers[operand(1)] else {
            return Ok(self.opaque_step(state, Some((dst, feedback)), &[], Effect::Opaque));
        };
        let definition = interp::eval::builtin_def(function)
            .map_err(|_| Refusal("a builtin without a definition"))?;
        let name = definition.name;
        let untagged = values::pairlist_items(call.cdr())
            .iter()
            .all(|(tag, _)| *tag == values::nil());
        let types: SmallVec<[Ty; 4]> = args
            .iter()
            .map(|&register| state.registers[register])
            .collect();
        if untagged && let Some((intrinsic, result)) = intrinsic_for(name, &types) {
            state.registers[dst] = result;
            state.visible = Some(true);
            return Ok(Action::Intrinsic {
                intrinsic,
                dst,
                args,
                result,
            });
        }
        if PURE_BUILTINS.contains(&name) {
            let objects = self.slice(
                args.iter()
                    .copied()
                    .filter(|&register| !state.registers[register].is_plain()),
            );
            return Ok(self.opaque_step(
                state,
                Some((dst, feedback)),
                objects,
                Effect::PureUnlessObserved,
            ));
        }
        Ok(self.opaque_step(state, Some((dst, feedback)), &[], Effect::Opaque))
    }

    /// Whether `function` can never assign a variable in its caller's frame or anywhere else:
    /// its defaults and body call only itself and base builtins from [`FRAME_SAFE_BUILTINS`],
    /// and assign only to its own locals. Each function a call resolves to is recorded as a
    /// speculation from the closure's environment, so a redefinition invalidates the code.
    /// Calling handlers are the remaining way R code could run inside such a call; the direct
    /// call checks for them when it runs.
    unsafe fn frame_safe(&mut self, function: Sexp) -> bool {
        if let Some(&safe) = self.frame_safe.get(&function) {
            return safe;
        }
        // A recursive call is safe exactly when the rest of the body is, so it is assumed safe
        // while the body is walked.
        self.frame_safe.insert(function, true);
        let mut found = List::new_in(self.arena);
        let defaults = values::pairlist_items(function.car());
        let safe = defaults
            .iter()
            .all(|&(_, default)| safe_expression(default, function, &mut found))
            && safe_expression(bytecode::closure_body(function), function, &mut found);
        if safe {
            self.speculations.extend(found);
        }
        self.frame_safe.insert(function, safe);
        safe
    }

    /// Whether the activation's own frame may bind `symbol`.
    fn bound_locally(&self, symbol: Sexp) -> bool {
        self.locals.contains(&symbol) || self.slot_of.contains_key(&symbol)
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

/// Base builtins that never run R code of the caller's choosing nor touch any environment but
/// their own arguments, apart from S3 dispatch on a classed argument.
const FRAME_SAFE_BUILTINS: &[&str] = &[
    "{",
    "(",
    "if",
    "for",
    "while",
    "repeat",
    "break",
    "next",
    "return",
    "invisible",
    "+",
    "-",
    "*",
    "/",
    "^",
    "%%",
    "%/%",
    "==",
    "!=",
    "<",
    ">",
    "<=",
    ">=",
    "!",
    "&",
    "|",
    "&&",
    "||",
    "sqrt",
    "exp",
    "log",
    "abs",
    "floor",
    "ceiling",
    "trunc",
    "round",
    "signif",
    "sin",
    "cos",
    "tan",
    "log2",
    "log10",
    "log1p",
    "expm1",
    "length",
    "[",
    "[[",
    "c",
    "is.na",
    "is.null",
    "sum",
    "prod",
    "max",
    "min",
    "cumsum",
    "cumprod",
    "seq_len",
    "seq_along",
    ":",
    "vector",
    "numeric",
    "integer",
    "double",
    "logical",
    "character",
    "rep",
    "rep.int",
    "rep_len",
    "nchar",
    "any",
    "all",
    "which",
    "rev",
    "as.integer",
    "as.numeric",
    "as.double",
    "as.logical",
    "is.numeric",
    "is.character",
    "is.logical",
    "identical",
    "range",
    "pmin",
    "pmax",
];

/// Whether `expression`, in the body of `function`, keeps `function` frame-safe (see
/// `Builder::frame_safe`), collecting the speculation for every function it calls.
unsafe fn safe_expression(expression: Sexp, function: Sexp, found: &mut List<Speculation>) -> bool {
    if expression.kind() != SexpType::Language {
        return true;
    }
    let head = expression.car();
    if head.kind() != SexpType::Symbol {
        return false;
    }
    let Some(speculation) = resolve_function(head, function.tag()) else {
        return false;
    };
    let callee = speculation.function;
    let arguments = values::pairlist_items(expression.cdr());
    let safe = if callee == function {
        true
    } else {
        match base_builtin(callee, head).as_deref() {
            // An assignment to a symbol binds in the function's own frame; one to `x[i]`
            // also calls the replacement builtin, and any other target could call a closure.
            Some("<-" | "=") => match arguments.first() {
                Some(&(_, target)) if target.kind() == SexpType::Symbol => true,
                Some(&(_, target))
                    if target.kind() == SexpType::Language
                        && target.car().kind() == SexpType::Symbol =>
                {
                    let index = values::symbol_name(target.car());
                    matches!(index.as_ref(), "[" | "[[")
                        && values::pairlist_items(target.cdr())
                            .first()
                            .is_some_and(|&(_, value)| value.kind() == SexpType::Symbol)
                }
                _ => false,
            },
            Some(name) => FRAME_SAFE_BUILTINS.contains(&name),
            None => false,
        }
    };
    if !safe {
        return false;
    }
    found.push(speculation);
    arguments
        .iter()
        .all(|&(_, argument)| safe_expression(argument, function, found))
}

/// The name of the base builtin `function`, when `symbol` names it.
unsafe fn base_builtin(function: Sexp, symbol: Sexp) -> Option<String> {
    if !matches!(function.kind(), SexpType::Builtin | SexpType::Special) {
        return None;
    }
    let name = interp::eval::builtin_def(function).ok()?.name;
    (values::symbol_name(symbol) == name).then(|| name.to_string())
}

/// Walks from `enclosure` to the binding a function lookup of `symbol` finds, as tier 0 does,
/// but gives up instead of forcing a promise or calling an active binding.
pub unsafe fn resolve_function(symbol: Sexp, start: Sexp) -> Option<Speculation> {
    let heap = interp::heap();
    let mut env = start;
    let empty = heap.known().empty_env;
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
                return Some(Speculation {
                    symbol,
                    env: start,
                    cell,
                    function: value,
                });
            }
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
        feedback::INTEGER_ARRAY => Ty::IntArr,
        feedback::DOUBLE_ARRAY => Ty::DblArr,
        feedback::LOGICAL_ARRAY => Ty::LglArr,
        _ if values::is_function(value) => Ty::Fun(value),
        _ => Ty::Boxed,
    }
}

/// The intrinsic for a call of builtin `name` with arguments of these types, and its result.
fn intrinsic_for(name: &str, types: &[Ty]) -> Option<(Intrinsic, Ty)> {
    let numeric = |ty: Ty| {
        if ty == Ty::Dbl {
            Numeric::Dbl
        } else {
            Numeric::Int
        }
    };
    Some(match (name, types) {
        ("[" | "[[", [vector, index])
            if vector.is_vector() && matches!(index, Ty::Int | Ty::Dbl) =>
        {
            (Intrinsic::Element, vector.element())
        }
        // `[` on an Array makes an Array, so only `[[` reads an element.
        ("[[", [array, index]) if array.is_array() && matches!(index, Ty::Int | Ty::Dbl) => {
            (Intrinsic::Element, array.element())
        }
        ("[[", [Ty::List, index]) if matches!(index, Ty::Int | Ty::Dbl) => {
            (Intrinsic::Element, Ty::Boxed)
        }
        ("length", [ty])
            if ty.is_vector() || ty.is_array() || ty.is_numeric_scalar() || *ty == Ty::List =>
        {
            (Intrinsic::Length, Ty::Int)
        }
        // An opaque call would make the compiled code forget every variable's type.
        ("List.len", [ty]) if ty.is_array() || *ty == Ty::List => (Intrinsic::Length, Ty::Int),
        ("-", [ty]) if ty.is_numeric_scalar() => (
            Intrinsic::Negate(numeric(*ty)),
            if *ty == Ty::Dbl { Ty::Dbl } else { Ty::Int },
        ),
        ("!", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Not, Ty::Lgl),
        ("is.na", [ty]) if ty.is_numeric_scalar() => (Intrinsic::IsNa, Ty::Lgl),
        ("sqrt", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Sqrt, Ty::Dbl),
        ("exp", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Exp, Ty::Dbl),
        ("log", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Log, Ty::Dbl),
        ("abs", [ty]) if ty.is_numeric_scalar() => (
            Intrinsic::Abs(numeric(*ty)),
            if *ty == Ty::Dbl { Ty::Dbl } else { Ty::Int },
        ),
        ("floor", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Floor, Ty::Dbl),
        ("ceiling", [ty]) if ty.is_numeric_scalar() => (Intrinsic::Ceiling, Ty::Dbl),
        ("%%", [a, b]) if a.is_numeric_scalar() && b.is_numeric_scalar() => {
            let numeric = if *a == Ty::Dbl || *b == Ty::Dbl {
                Numeric::Dbl
            } else {
                Numeric::Int
            };
            (
                Intrinsic::Modulo(numeric),
                if numeric == Numeric::Int {
                    Ty::Int
                } else {
                    Ty::Dbl
                },
            )
        }
        ("%/%", [a, b]) if a.is_numeric_scalar() && b.is_numeric_scalar() => {
            let numeric = if *a == Ty::Dbl || *b == Ty::Dbl {
                Numeric::Dbl
            } else {
                Numeric::Int
            };
            (
                Intrinsic::IntegerDivide(numeric),
                if numeric == Numeric::Int {
                    Ty::Int
                } else {
                    Ty::Dbl
                },
            )
        }
        _ => return None,
    })
}

pub fn jump_target(inst: &Inst) -> Option<usize> {
    match inst.op {
        Op::Jump => Some(inst.operand(0)),
        Op::JumpUnless => Some(inst.operand(2)),
        Op::BranchUnlessBuiltin | Op::BranchIfObject => Some(inst.operand(1)),
        Op::BranchUnlessPrimitive => Some(inst.operand(2)),
        Op::ForStep => Some(inst.operand(3)),
        Op::LogicLeft => Some(inst.operand(4)),
        Op::Guard => Some(inst.operand(4)),
        _ => None,
    }
}

fn ends_block(op: Op) -> bool {
    matches!(
        op,
        Op::Jump
            | Op::JumpUnless
            | Op::BranchUnlessBuiltin
            | Op::BranchUnlessPrimitive
            | Op::BranchIfObject
            | Op::ForStep
            | Op::LogicLeft
            | Op::Guard
            | Op::Return
    )
}

fn falls_through(op: Op) -> bool {
    !matches!(op, Op::Jump | Op::Return)
}

/// Updates `live` from after `inst` to before it.
fn apply_liveness(inst: &Inst, live: &mut [u64]) {
    let operand = |index: usize| inst.operand(index);
    let kill = |register: usize, live: &mut [u64]| set_bit(live, register, false);
    let gen_ = |register: usize, live: &mut [u64]| set_bit(live, register, true);
    match inst.op {
        Op::Constant | Op::GetVar | Op::GetFun | Op::Ast | Op::Closure => kill(operand(0), live),
        Op::SetVar => gen_(operand(1), live),
        Op::BranchUnlessBuiltin | Op::BranchUnlessPrimitive | Op::BranchIfObject => {
            gen_(operand(0), live)
        }
        Op::DispatchSubject => {
            kill(operand(0), live);
            gen_(operand(1), live);
            gen_(operand(3), live);
        }
        Op::CallBuiltin => {
            kill(operand(0), live);
            gen_(operand(1), live);
            for register in operand(3)..operand(3) + operand(4) {
                gen_(register, live);
            }
        }
        Op::CallGeneric | Op::CallRebinding => {
            kill(operand(0), live);
            gen_(operand(1), live);
        }
        Op::Guard | Op::Jump | Op::Invisible | Op::Visible | Op::SetSrcref => {}
        Op::JumpUnless | Op::Return => gen_(operand(0), live),
        Op::Arithmetic => {
            kill(operand(1), live);
            gen_(operand(2), live);
            gen_(operand(3), live);
        }
        Op::ForInit | Op::ForStep => gen_(operand(1), live),
        Op::ForEnd => gen_(operand(0), live),
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
