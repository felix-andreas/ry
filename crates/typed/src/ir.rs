//! The typed core IR: one function's body as blocks of instructions over numbered locals, with
//! explicit reference-count operations.
//!
//! Locals are mutable slots, not SSA values: the backend hands each one to Cranelift as a
//! variable, and Cranelift builds the SSA form. A local of a counted type (a string, a vector, a
//! list or a dictionary) owns the reference it holds, except a parameter, which borrows its
//! caller's. The lowering emits no count operations at all. [`crate::ownership`] then decides,
//! from liveness, which operand each instruction may take ownership of, inserts a `Dup` where a
//! value must be shared and a `Drop` where a local dies, and lists what a failing instruction
//! must drop before it gives up.

use smallvec::SmallVec;

/// A local's index into [`Function::locals`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Local(pub u32);

/// A block's index into [`Function::blocks`]. Block 0 is the entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlockId(pub u32);

/// What a vector or list holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Item {
    Int,
    Dbl,
    Lgl,
    Str,
}

/// The type of a local.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Type {
    /// An R integer scalar (`NA` is `i32::MIN`).
    Int,
    /// An R double scalar.
    Dbl,
    /// An R logical scalar: 0, 1 or `NA` (`i32::MIN`).
    Lgl,
    /// A condition that is known not to be `NA`: 0 or 1.
    Bool,
    /// A string scalar, which is never `NA`.
    Str,
    Null,
    /// An R atomic vector of these items. `array` marks an `Array<T>`, which has no `NA` and
    /// carries the Array bit when it crosses back to R.
    Vector {
        item: Item,
        array: bool,
    },
    /// An R list whose elements are scalars of these items.
    List(Item),
    Dict {
        key: Item,
        value: Item,
    },
}

impl Type {
    /// Whether a value of this type is a counted reference, which the ownership pass tracks.
    pub fn is_counted(self) -> bool {
        matches!(
            self,
            Type::Str | Type::Vector { .. } | Type::List(_) | Type::Dict { .. }
        )
    }

    /// Whether a value of this type is a pointer to a counted object with the shared header.
    pub fn is_object(self) -> bool {
        matches!(
            self,
            Type::Vector { .. } | Type::List(_) | Type::Dict { .. }
        )
    }

    /// The scalar type of one of these items.
    pub fn of_item(item: Item) -> Type {
        match item {
            Item::Int => Type::Int,
            Item::Dbl => Type::Dbl,
            Item::Lgl => Type::Lgl,
            Item::Str => Type::Str,
        }
    }

    /// The item a scalar of this type is, if it can be one.
    pub fn item(self) -> Option<Item> {
        match self {
            Type::Int => Some(Item::Int),
            Type::Dbl => Some(Item::Dbl),
            Type::Lgl | Type::Bool => Some(Item::Lgl),
            Type::Str => Some(Item::Str),
            _ => None,
        }
    }

    /// The items of a vector or list.
    pub fn items(self) -> Option<Item> {
        match self {
            Type::Vector { item, .. } | Type::List(item) => Some(item),
            _ => None,
        }
    }

    pub fn vector(item: Item) -> Type {
        Type::Vector { item, array: false }
    }
}

/// A function in the IR.
#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub locals: Vec<Type>,
    /// The leading locals, which hold the arguments.
    pub params: usize,
    pub blocks: Vec<Block>,
    pub result: Type,
}

impl Function {
    pub fn ty(&self, local: Local) -> Type {
        self.locals[local.0 as usize]
    }

    pub fn is_param(&self, local: Local) -> bool {
        (local.0 as usize) < self.params
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug)]
pub struct Inst {
    pub dest: Option<Local>,
    pub op: Op,
    /// The operands this instruction takes ownership of, by position in [`Op::operands`]. The
    /// ownership pass sets it, only where [`Op::can_consume`] allows.
    pub moves: u8,
    /// The counted locals a failure of this instruction must drop: those live across it and the
    /// operands it borrows that die at it. The ownership pass sets it.
    pub fail_drops: SmallVec<[Local; 4]>,
}

impl Inst {
    pub fn new(dest: Option<Local>, op: Op) -> Inst {
        Inst {
            dest,
            op,
            moves: 0,
            fail_drops: SmallVec::new(),
        }
    }

    pub fn moves(&self, position: usize) -> bool {
        self.moves & (1 << position) != 0
    }
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Jump(BlockId),
    Branch {
        condition: Local,
        then: BlockId,
        otherwise: BlockId,
    },
    Return(Local),
    /// Gives up: the caller runs the call again in the interpreter.
    Fail,
}

impl Terminator {
    pub fn successors(&self) -> SmallVec<[BlockId; 2]> {
        match *self {
            Terminator::Jump(target) => SmallVec::from_slice(&[target]),
            Terminator::Branch {
                then, otherwise, ..
            } => SmallVec::from_slice(&[then, otherwise]),
            Terminator::Return(_) | Terminator::Fail => SmallVec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Constant {
    Int(i32),
    Dbl(f64),
    Lgl(i32),
    /// A string constant, as the bytes of a static string the compiler built.
    Str(u64, u64),
    Null,
}

/// Arithmetic on two scalars or vectors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arith {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
    Pow = 4,
    Mod = 5,
    IntDiv = 6,
    Min = 7,
    Max = 8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compare {
    Eq = 0,
    Ne = 1,
    Lt = 2,
    Le = 3,
    Gt = 4,
    Ge = 5,
}

/// A function of one number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Math {
    Neg = 0,
    Exp = 1,
    Log = 2,
    Sqrt = 3,
    Abs = 4,
    Sin = 5,
    Cos = 6,
    Floor = 7,
    Ceiling = 8,
    Trunc = 9,
    Log2 = 10,
    Log10 = 11,
    Tan = 12,
    Round = 13,
    Sign = 14,
}

/// A reduction of a vector to a scalar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reduce {
    Sum,
    Prod,
    Max,
    Min,
    Mean,
    Any,
    All,
}

/// A string function of one string scalar or one vector of strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Text {
    Upper,
    Lower,
    Nchar,
}

#[derive(Clone, Debug)]
pub enum Op {
    Const(Constant),
    Copy(Local),
    /// Scalar arithmetic on operands of this item type (`Int` or `Dbl`).
    Arith(Arith, Item, Local, Local),
    /// Scalar comparison giving a logical.
    Compare(Compare, Item, Local, Local),
    Not(Local),
    And(Local, Local),
    Or(Local, Local),
    /// A scalar function of a double (or, for `Neg` and `Abs`, of an integer).
    Math(Math, Item, Local),
    /// `round(x, digits)` of a scalar double.
    RoundDigits(Local, Local),
    /// An integer or logical scalar as a double.
    ToDbl(Local),
    /// A logical scalar as an integer.
    LglToInt(Local),
    /// A logical as a branch condition: fails on `NA`.
    Truth(Local),
    /// Whether two integers that are not `NA` are equal, as a branch condition.
    Same(Local, Local),
    /// A loop counter plus its step (1 or -1), for a counter that has not reached its end, so
    /// the sum neither overflows nor lands on `NA`.
    Step(Local, Local),
    /// The length of a vector or list, as an integer.
    Length(Local),
    /// Item `index` (1-based) of a vector or list. Fails out of range.
    Get(Local, Local),
    /// The vector or list with item `index` (1-based) replaced: `x[[i]] <- value`. Fails out of
    /// range.
    Set(Local, Local, Local),
    /// A new vector of this many default items (`numeric(n)`, `character(n)`).
    NewVector(Item, Local),
    /// `seq_len(n)`, failing for a negative `n`.
    SeqLen(Local),
    /// `from:to` of integers.
    Range(Local, Local),
    /// `rep(value, times)`.
    Repeat(Local, Local),
    /// Elementwise arithmetic where at least one operand is a vector. The item type is the
    /// operands', which the lowering has made equal.
    VectorArith(Arith, Item, Local, Local),
    VectorCompare(Compare, Item, Local, Local),
    VectorLogic(bool, Local, Local),
    VectorNot(Local),
    VectorMath(Math, Item, Local),
    VectorRound(Local, Local),
    /// A vector of integers or logicals as one of doubles, or of logicals as integers.
    Coerce(Local, Item),
    Reduce(Reduce, Item, Local),
    /// The items of a list as a vector of the same items, or the reverse: the same buffer.
    Relabel(Local, Type),
    /// `paste(parts..., sep = separator)` of string scalars.
    Concat(SmallVec<[Local; 4]>, Local),
    /// `paste(parts..., sep = separator)` where some parts are vectors.
    PasteVectors(SmallVec<[Local; 4]>, Local),
    /// `paste(x, collapse = separator)`.
    Join(Local, Local),
    /// A string function of a scalar.
    Text(Text, Local),
    /// A string function of every item of a vector.
    VectorText(Text, Local),
    /// `startsWith` (`true`) or `endsWith` of a scalar and a prefix.
    Affix(bool, Local, Local),
    VectorAffix(bool, Local, Local),
    /// A number as a string: `as.character` and `paste`.
    Format(Local),
    /// `sprintf("%.Nf", x)`.
    FormatFixed(Local, i32),
    /// `formatC(i, width = w, flag = "0")`, or with spaces when the flag is false.
    FormatPadded(Local, i32, bool),
    /// A call of a compiled typed function with borrowed arguments.
    CallTyped(usize, SmallVec<[Local; 4]>),
    /// Fails when an interrupt is pending.
    Poll,
    Dup(Local),
    Drop(Local),
}

impl Op {
    /// The locals the instruction reads, in a fixed order that [`Inst::moves`] indexes.
    pub fn operands(&self) -> SmallVec<[Local; 4]> {
        use Op::*;
        match self {
            Const(_) | Poll => SmallVec::new(),
            Copy(a)
            | Not(a)
            | Math(_, _, a)
            | ToDbl(a)
            | LglToInt(a)
            | Truth(a)
            | Length(a)
            | NewVector(_, a)
            | SeqLen(a)
            | VectorNot(a)
            | VectorMath(_, _, a)
            | Coerce(a, _)
            | Reduce(_, _, a)
            | Relabel(a, _)
            | Text(_, a)
            | VectorText(_, a)
            | Format(a)
            | FormatFixed(a, _)
            | FormatPadded(a, _, _)
            | Dup(a)
            | Drop(a) => SmallVec::from_slice(&[*a]),
            Arith(_, _, a, b)
            | Compare(_, _, a, b)
            | And(a, b)
            | Or(a, b)
            | RoundDigits(a, b)
            | Get(a, b)
            | Range(a, b)
            | Same(a, b)
            | Step(a, b)
            | Repeat(a, b)
            | VectorArith(_, _, a, b)
            | VectorCompare(_, _, a, b)
            | VectorLogic(_, a, b)
            | VectorRound(a, b)
            | Join(a, b)
            | Affix(_, a, b)
            | VectorAffix(_, a, b) => SmallVec::from_slice(&[*a, *b]),
            Set(a, b, c) => SmallVec::from_slice(&[*a, *b, *c]),
            Concat(parts, separator) | PasteVectors(parts, separator) => {
                let mut operands = parts.clone();
                operands.push(*separator);
                operands
            }
            CallTyped(_, arguments) => arguments.clone(),
        }
    }

    /// Whether the instruction may take ownership of the operand at `position`, which lets a
    /// unique buffer be reused or moved instead of copied.
    pub fn can_consume(&self, position: usize) -> bool {
        use Op::*;
        match self {
            Copy(_) | Relabel(..) | Coerce(..) | VectorNot(_) | VectorMath(..) | Text(..)
            | VectorText(..) => position == 0,
            VectorArith(..) | VectorCompare(..) | VectorLogic(..) => position < 2,
            VectorRound(..) => position == 0,
            // The list, and the new item, which the list then owns.
            Set(..) => position != 1,
            _ => false,
        }
    }

    /// Whether the instruction can fail, which makes it give up the whole call.
    pub fn can_fail(&self) -> bool {
        use Op::*;
        match self {
            Const(_) | Copy(_) | Compare(..) | Not(_) | And(..) | Or(..) | ToDbl(_)
            | LglToInt(_) | Length(_) | Relabel(..) | Dup(_) | Drop(_) | Concat(..) | Join(..)
            | Text(..) | Affix(..) | VectorText(..) | VectorAffix(..) | RoundDigits(..)
            | Same(..) | Step(..) => false,
            // Integer arithmetic overflows, a double `%%` loses all accuracy, and a math function
            // warns about NaNs.
            Arith(op, item, ..) => *item == Item::Int || *op == self::Arith::Mod,
            Math(..) => true,
            // `NA` formats differently in `as.character`, `paste` and `sprintf`, so the
            // interpreter formats it.
            Format(_) | FormatFixed(..) | FormatPadded(..) => true,
            _ => true,
        }
    }
}

/// A builder that appends to the current block.
pub struct Builder {
    pub function: Function,
    pub current: BlockId,
}

impl Builder {
    pub fn new(name: String, params: &[Type]) -> Builder {
        Builder {
            function: Function {
                name,
                locals: params.to_vec(),
                params: params.len(),
                blocks: vec![Block {
                    insts: Vec::new(),
                    terminator: Terminator::Fail,
                }],
                result: Type::Null,
            },
            current: BlockId(0),
        }
    }

    pub fn local(&mut self, ty: Type) -> Local {
        self.function.locals.push(ty);
        Local(self.function.locals.len() as u32 - 1)
    }

    pub fn block(&mut self) -> BlockId {
        self.function.blocks.push(Block {
            insts: Vec::new(),
            terminator: Terminator::Fail,
        });
        BlockId(self.function.blocks.len() as u32 - 1)
    }

    pub fn ty(&self, local: Local) -> Type {
        self.function.ty(local)
    }

    /// Appends `op` with a fresh destination of type `ty`.
    pub fn emit(&mut self, ty: Type, op: Op) -> Local {
        let dest = self.local(ty);
        self.push(Some(dest), op);
        dest
    }

    pub fn push(&mut self, dest: Option<Local>, op: Op) {
        let block = &mut self.function.blocks[self.current.0 as usize];
        block.insts.push(Inst::new(dest, op));
    }

    pub fn terminate(&mut self, terminator: Terminator) {
        self.function.blocks[self.current.0 as usize].terminator = terminator;
    }

    pub fn switch_to(&mut self, block: BlockId) {
        self.current = block;
    }
}
