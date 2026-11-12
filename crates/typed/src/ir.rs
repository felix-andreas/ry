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
    /// ry's 64-bit integer, which has no `NA` and whose arithmetic fails on overflow.
    I64,
    /// A boxed record of this shape ([`crate::shapes`]).
    Record(u32),
    /// A boxed tag of this union of shapes.
    Tag(u32),
    /// A ry list of the items [`crate::shapes::array_item`] gives for this id, which lets a
    /// record field or a list item be a list while an item stays one word.
    Array(u32),
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
    /// ry's `I64`: a 64-bit integer without `NA`.
    I64,
    Null,
    /// An R atomic vector of these items. `array` marks an `Array<T>`, which has no `NA` and
    /// carries the Array bit when it crosses back to R.
    Vector {
        item: Item,
        array: bool,
    },
    /// An R list whose elements are scalars of these items.
    List(Item),
    /// ry's `Set` of these items: their distinct values in first-insertion order, held in a list
    /// object, which never crosses back to R.
    Set(Item),
    Dict {
        key: Item,
        value: Item,
    },
    /// A boxed record of one shape ([`crate::shapes`]).
    Record(u32),
    /// A boxed tag whose shape is one of a union's ([`crate::shapes::ANY`] for any shape).
    Tag(u32),
    /// A value of ry's table API: a `DataFrame`, a `LazyFrame`, a `GroupBy` or an expression.
    /// Typed code holds the R object itself and never looks inside it, so only the API's own
    /// functions make one or take one apart. The collector owns the object, which the call that
    /// entered typed code keeps alive, so it is not counted.
    Table,
}

impl Type {
    /// Whether a value of this type is a counted reference, which the ownership pass tracks.
    pub fn is_counted(self) -> bool {
        matches!(
            self,
            Type::Str
                | Type::Vector { .. }
                | Type::List(_)
                | Type::Set(_)
                | Type::Dict { .. }
                | Type::Record(_)
                | Type::Tag(_)
        )
    }

    /// Whether a value of this type is a pointer to a counted object with the shared header.
    pub fn is_object(self) -> bool {
        matches!(
            self,
            Type::Vector { .. }
                | Type::List(_)
                | Type::Set(_)
                | Type::Dict { .. }
                | Type::Record(_)
                | Type::Tag(_)
        )
    }

    /// The scalar type of one of these items.
    pub fn of_item(item: Item) -> Type {
        match item {
            Item::Int => Type::Int,
            Item::Dbl => Type::Dbl,
            Item::Lgl => Type::Lgl,
            Item::Str => Type::Str,
            Item::I64 => Type::I64,
            Item::Record(shape) => Type::Record(shape),
            Item::Tag(union) => Type::Tag(union),
            Item::Array(id) => Type::Vector {
                item: crate::shapes::array_item(id),
                array: true,
            },
        }
    }

    /// The item a scalar of this type is, if it can be one.
    pub fn item(self) -> Option<Item> {
        match self {
            Type::Int => Some(Item::Int),
            Type::Dbl => Some(Item::Dbl),
            Type::Lgl | Type::Bool => Some(Item::Lgl),
            Type::Str => Some(Item::Str),
            Type::I64 => Some(Item::I64),
            Type::Record(shape) => Some(Item::Record(shape)),
            Type::Tag(union) => Some(Item::Tag(union)),
            _ => None,
        }
    }

    /// The item a value of this type is when a ry list or a record holds it: a scalar's item, or
    /// a ry list as an [`Item::Array`].
    pub fn held_item(self) -> Option<Item> {
        match self {
            Type::Vector { item, array: true } => Some(Item::Array(crate::shapes::array_of(item))),
            _ => self.item(),
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
    I64(i64),
    Null,
    /// A static object, such as a tag without a payload, whose count never changes.
    Object(u64),
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
    /// `Num.pow`: the platform's `pow`, without the multiplication R's `^` uses for small
    /// whole powers, so the last bit matches the interpreter's `Num.pow`.
    NumPow = 9,
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
    /// Rounding half away from zero, as `Num.round` does.
    RoundAway = 15,
}

/// A bit operation on the 64-bit two's complement of two `I64`s.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bits {
    And,
    Or,
    Xor,
    ShiftLeft,
    /// Shifts right, copying the sign bit in.
    ShiftRight,
    /// Shifts right, filling with zeros.
    ShiftRightZf,
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
    /// `Str.with_ascii_uppercased`: only ASCII letters change.
    AsciiUpper,
    /// `Str.with_ascii_lowercased`.
    AsciiLower,
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
    /// A bit operation on two `I64`s. A shift fails unless its count is from 0 to 63.
    Bits(Bits, Local, Local),
    /// Whether a double is `NaN`, as a `Bool`. It fails on `NA`, which no library function takes.
    IsNan(Local),
    /// `round(x, digits)` of a scalar double.
    RoundDigits(Local, Local),
    /// An integer, logical or `I64` scalar as a double.
    ToDbl(Local),
    /// An integer or double scalar as an `I64`, failing unless it converts exactly.
    ToI64(Local),
    /// A logical scalar as an integer.
    LglToInt(Local),
    /// An `I64` count as an integer, failing unless it is in `0..=i32::MAX`.
    CountOfI64(Local),
    /// A logical as a branch condition: fails on `NA`.
    Truth(Local),
    /// Whether two integers that are not `NA` are equal, as a branch condition.
    Same(Local, Local),
    /// A loop counter plus its step (1 or -1), for a counter that has not reached its end, so
    /// the sum neither overflows nor lands on `NA`.
    Step(Local, Local),
    /// The length of a vector or list, as an integer or an `I64` (the destination's type).
    Length(Local),
    /// The `I64` list `from..=to`, empty when `to < from`.
    RangeI64(Local, Local),
    /// Item `index` (1-based) of a vector or list. Fails out of range.
    Get(Local, Local),
    /// Item `position` (1-based) of a list, for a position a loop counts from 1 to the length of
    /// a list it holds, which is why the read carries no range check.
    ItemAt(Local, Local),
    /// The vector or list with item `index` (1-based) replaced: `x[[i]] <- value`. Fails out of
    /// range.
    Set(Local, Local, Local),
    /// `List.swap(list, first, second)` of 1-based `I64` positions; out of range, the list is
    /// unchanged.
    Swap(Local, Local, Local),
    /// `List.concat(first, second)` of two lists of one item type. A unique first list grows in
    /// place.
    Extend(Local, Local),
    /// The items of a list from 0-based `I64` `start` for `count` items. Fails unless the range
    /// lies within the list.
    Slice(Local, Local, Local),
    /// A new vector of this many default items (`numeric(n)`, `character(n)`).
    NewVector(Item, Local),
    /// An empty ry list with room for this many items (`List.with_capacity(n)`), failing for a
    /// negative count.
    EmptyList(Item, Local),
    /// `seq_len(n)`, failing for a negative `n`.
    SeqLen(Local),
    /// `from:to` of integers.
    Range(Local, Local),
    /// `rep(value, times)`.
    Repeat(Local, Local),
    /// `x[mask]` of a vector and a logical vector of the same length. Fails on any other length
    /// and on an `NA` in the mask.
    Select(Local, Local),
    /// `Set.from_list`: the distinct items of a list, in first-occurrence order.
    Unique(Local),
    /// `List.sort_asc`, or `List.sort_desc` when true, of a numeric list. Fails on `NA` or NaN.
    Sort(Local, bool),
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
    /// Arithmetic over ry lists fused into one pass with borrowed inputs (lists of `item` and
    /// scalars), as `values::ry_elementwise` encodes `program`. The result is a list of `item`,
    /// or with `sum` the sum of its items.
    Elementwise {
        item: Item,
        sum: bool,
        program: SmallVec<[u32; 8]>,
        inputs: SmallVec<[Local; 4]>,
    },
    /// How many items of a logical ry list are true, as an `I64`, failing for an `NA` item.
    /// With an empty `program` the list is input 0; otherwise the list is the last step of
    /// `program`, which `values::ry_count_true` encodes: comparisons of numeric lists and
    /// scalars, and `&`, `|` and `!` of their results, fused into one pass with borrowed inputs
    /// and no list for any step ([`crate::fusion`] builds it).
    CountTrue {
        program: SmallVec<[u32; 8]>,
        inputs: SmallVec<[Local; 4]>,
    },
    /// A call of a compiled typed function with borrowed arguments.
    CallTyped(usize, SmallVec<[Local; 4]>),
    /// A call of an R function ([`crate::lower::RCall`]) with borrowed arguments, whose result,
    /// if the destination exists, is converted to a native value. It fails when the R function
    /// raises an error, which the call then raises.
    CallR(usize, SmallVec<[Local; 4]>),
    /// A new box of this shape holding the fields, which it owns, in the shape's field order.
    Construct(u32, SmallVec<[Local; 4]>),
    /// Field `index` of a box known to have this shape.
    Field(Local, u32, u32),
    /// The shape id of a box, as an integer.
    ShapeOf(Local),
    /// `Str.count_utf8_bytes`, as an `I64`.
    ByteCount(Local),
    /// `Str.to_utf8`: the string's bytes, as a list of `I64`s.
    ToUtf8(Local),
    /// `Str.from_utf8_lossy` of a list of `I64`s, failing for an item outside 0 to 255.
    FromUtf8Lossy(Local),
    /// `Str.repeat(string, count)`, failing for a negative count.
    RepeatStr(Local, Local),
    /// `Str.split_on(string, separator)`, a list of strings.
    Split(Local, Local),
    /// A string parsed as the destination's type (`I64` or `Dbl`), with Rust's number syntax,
    /// failing when it does not parse. [`Op::Parses`] tests first.
    Parse(Local),
    /// Whether a string parses as an `I64` (`true`) or as a double that is not `NaN`.
    Parses(bool, Local),
    /// An empty dictionary of the destination's type.
    DictNew,
    /// Whether the dictionary holds the key.
    DictContains(Local, Local),
    /// The value of a key the dictionary holds, failing otherwise.
    DictGet(Local, Local),
    /// The dictionary with the key mapped to the value.
    DictInsert(Local, Local, Local),
    /// The dictionary without the key.
    DictRemove(Local, Local),
    /// How many entries the dictionary holds, as an `I64`.
    DictLen(Local),
    /// The dictionary's keys in insertion order, as a ry list.
    DictKeys(Local),
    /// The dictionary's values in insertion order, as a ry list.
    DictValues(Local),
    /// The list with the item appended, which it then owns.
    Push(Local, Local),
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
            Const(_) | Poll | DictNew => SmallVec::new(),
            Copy(a)
            | Not(a)
            | Math(_, _, a)
            | IsNan(a)
            | ToDbl(a)
            | ToI64(a)
            | LglToInt(a)
            | CountOfI64(a)
            | Truth(a)
            | Length(a)
            | NewVector(_, a)
            | EmptyList(_, a)
            | SeqLen(a)
            | VectorNot(a)
            | VectorMath(_, _, a)
            | Coerce(a, _)
            | Reduce(_, _, a)
            | Relabel(a, _)
            | Unique(a)
            | Sort(a, _)
            | Text(_, a)
            | VectorText(_, a)
            | Format(a)
            | FormatFixed(a, _)
            | FormatPadded(a, _, _)
            | Dup(a)
            | Drop(a)
            | Field(a, _, _)
            | ShapeOf(a)
            | ByteCount(a)
            | ToUtf8(a)
            | FromUtf8Lossy(a)
            | DictLen(a)
            | DictKeys(a)
            | DictValues(a)
            | Parse(a)
            | Parses(_, a) => SmallVec::from_slice(&[*a]),
            Arith(_, _, a, b)
            | Bits(_, a, b)
            | Compare(_, _, a, b)
            | And(a, b)
            | Or(a, b)
            | RoundDigits(a, b)
            | Get(a, b)
            | ItemAt(a, b)
            | Range(a, b)
            | RangeI64(a, b)
            | Same(a, b)
            | Step(a, b)
            | Repeat(a, b)
            | Select(a, b)
            | VectorArith(_, _, a, b)
            | VectorCompare(_, _, a, b)
            | VectorLogic(_, a, b)
            | VectorRound(a, b)
            | Join(a, b)
            | Affix(_, a, b)
            | VectorAffix(_, a, b)
            | RepeatStr(a, b)
            | Split(a, b)
            | DictContains(a, b)
            | DictGet(a, b)
            | DictRemove(a, b)
            | Push(a, b)
            | Extend(a, b) => SmallVec::from_slice(&[*a, *b]),
            Set(a, b, c) | DictInsert(a, b, c) | Swap(a, b, c) | Slice(a, b, c) => {
                SmallVec::from_slice(&[*a, *b, *c])
            }
            Concat(parts, separator) | PasteVectors(parts, separator) => {
                let mut operands = parts.clone();
                operands.push(*separator);
                operands
            }
            CallTyped(_, arguments)
            | CallR(_, arguments)
            | Construct(_, arguments)
            | CountTrue {
                inputs: arguments, ..
            }
            | Elementwise {
                inputs: arguments, ..
            } => arguments.clone(),
        }
    }

    /// Whether the instruction may take ownership of the operand at `position`, which lets a
    /// unique buffer be reused or moved instead of copied.
    pub fn can_consume(&self, position: usize) -> bool {
        use Op::*;
        match self {
            Copy(_) | Relabel(..) | Coerce(..) | VectorNot(_) | VectorMath(..) | Text(..)
            | VectorText(..) | Unique(_) | Sort(..) => position == 0,
            VectorArith(..) | VectorCompare(..) | VectorLogic(..) => position < 2,
            VectorRound(..) => position == 0,
            // The list, and the new item, which the list then owns.
            Set(..) | DictInsert(..) => position != 1,
            DictRemove(..) | Swap(..) | Slice(..) => position == 0,
            Extend(..) => position < 2,
            Push(..) => true,
            // A box owns its fields.
            Construct(..) => true,
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
            | Same(..) | Step(..) | ItemAt(..) | Construct(..) | Field(..) | ShapeOf(_) | ByteCount(_)
            | Split(..) | Parses(..) | DictContains(..) | DictRemove(..) | Push(..) | Unique(_)
            | Swap(..) | ToUtf8(_) | DictLen(_) | DictKeys(_) | DictValues(_) => false,
            // Integer arithmetic overflows, a double `%%` loses all accuracy, and a math function
            // warns about NaNs.
            Arith(op, item, ..) => *item != Item::Dbl || *op == self::Arith::Mod,
            Math(..) => true,
            Bits(op, ..) => !matches!(op, self::Bits::And | self::Bits::Or | self::Bits::Xor),
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
