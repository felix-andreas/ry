//! Machine code for the typed IR, through Cranelift.
//!
//! Every local becomes one Cranelift variable per machine word: an integer, a logical and a
//! condition are one `i32`, a double one `f64`, a string two `i64` words (the [`values::Str`]
//! layout) and a vector, list or dictionary one pointer. Counted locals start out null, which
//! every drop treats as nothing to drop, so a local that a path never assigned is safe to drop.
//!
//! A compiled function takes the remaining call depth and its arguments as machine words and
//! returns a status (0 for a value, 1 to give up) followed by its value's words. Functions call
//! each other with Cranelift's `tail` convention, which returns a string's two words and the
//! status in registers. Each function also gets a trampoline in the host's C convention, which
//! reads the arguments from an array of words and writes the value to another, so the driver
//! calls every function the same way.

use {
    crate::{
        Refusal,
        ir::{
            Arith, BitCount, Bits, Block as IrBlock, Compare, Constant, Function, Inst, Item,
            Local, Math, Op, Overflow, Reduce, StrTest, Terminator, Text, Type,
        },
        lower::SELF,
        ranges::{self, Range, Ranges, exact_bounds, ranges},
        shapes, values,
    },
    heap::object::NA_REAL_BITS,
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    ssa::{
        AbiParam, Allocator, Block, Builder, CallConv, FloatCC, FuncId, FuncRef, IntCC,
        MemFlagsData, Relocatable, SigRef, Signature, StackSlotData, StackSlotKind, Target,
        Type as Machine, Value, Variable,
        types::{F64, I32, I64},
    },
    std::{
        cell::Cell,
        sync::atomic::{AtomicBool, AtomicUsize},
    },
};

/// A compiled function another one may call: where its code is and its machine signature.
#[derive(Clone, Debug)]
pub struct Callee {
    pub entry: Entry,
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
}

/// Where compiled code finds a function it calls.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    /// The function's address in this process, for code compiled in the session.
    Address(usize),
    /// A function of the same code file, which the call reaches relative to itself.
    Function(FuncId),
    /// A typed function of another namespace, by its index in the code file's callees, whose
    /// address the loader writes into the offset table when the caller attaches.
    Callee(u32),
    /// The address of a word in this process that holds the function's address, for a function
    /// still being compiled when its caller is, as in mutual recursion.
    Cell(usize),
}

/// The addresses of a compiled function and of its trampoline.
#[derive(Clone, Copy, Debug)]
pub struct Code {
    pub address: usize,
    pub trampoline: usize,
}

/// Calls a trampoline: the call budget, the argument words and where the value's words go.
pub type Trampoline = unsafe extern "C" fn(i64, *const u64, *mut u64) -> i64;

/// A word of a code file's offset table, which the loader fills: every kind but `Site` and
/// `Callee` when it maps the file, and those two when the function that uses them attaches.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Slot {
    /// The address of a runtime helper.
    Helper(Helper),
    /// The address of `interp::unwind::INTERRUPT_REQUESTED`.
    Interrupt,
    /// The address of [`crate::STACK_LIMIT`].
    StackLimit,
    /// The loading process's id of a shape, named by its id in the writing process.
    Shape(u32),
    /// The static box of a shape without fields, named by its id in the writing process.
    Constant(u32),
    /// The address of the file's read-only data plus this offset.
    Data(u32),
    /// The `RCall` of an R call site, by its index in the file's sites.
    Site(u32),
    /// The entry of a typed function of another namespace, by its index in the file's callees.
    Callee(u32),
}

/// The offset table and the read-only data of a code file being written. Slot `n` sits `8 * (n
/// + 1)` bytes below the code, so a slot's place does not depend on how many follow it.
#[derive(Default)]
pub struct Slots {
    pub slots: Vec<Slot>,
    numbers: FxHashMap<Slot, u32>,
    /// Static strings, each in the layout [`values::ry_str_static`] gives a heap string.
    pub data: Vec<u8>,
    strings: FxHashMap<Vec<u8>, u32>,
}

impl Slots {
    /// The number of `slot`, added on its first use.
    pub fn number(&mut self, slot: Slot) -> u32 {
        if let Some(&number) = self.numbers.get(&slot) {
            return number;
        }
        let number = self.slots.len() as u32;
        self.slots.push(slot);
        self.numbers.insert(slot, number);
        number
    }

    /// The offset in the read-only data of a static string holding `bytes`.
    fn string(&mut self, bytes: &[u8]) -> u32 {
        if let Some(&offset) = self.strings.get(bytes) {
            return offset;
        }
        let offset = self.data.len() as u32;
        self.data
            .extend_from_slice(&values::STATIC_COUNT.to_le_bytes());
        self.data
            .extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        self.data.extend_from_slice(bytes);
        self.data.resize(self.data.len().next_multiple_of(8), 0);
        self.strings.insert(bytes.to_vec(), offset);
        offset
    }
}

const NA_INTEGER: i64 = i32::MIN as i64;
const LENGTH_OFFSET: i32 = std::mem::offset_of!(values::List, length) as i32;
const DATA_OFFSET: i32 = std::mem::offset_of!(values::List, data) as i32;

/// The machine words a value of `ty` occupies.
pub fn words(ty: Type) -> SmallVec<[Machine; 2]> {
    match ty {
        Type::Int | Type::Lgl | Type::Bool => SmallVec::from_slice(&[I32]),
        Type::Dbl => SmallVec::from_slice(&[F64]),
        Type::I64 => SmallVec::from_slice(&[I64]),
        Type::Str => SmallVec::from_slice(&[I64, I64]),
        Type::Null
        | Type::Vector { .. }
        | Type::List(_)
        | Type::Set(_)
        | Type::Dict { .. }
        | Type::Record(_)
        | Type::Tag(_)
        | Type::Table => SmallVec::from_slice(&[I64]),
        Type::Absent => SmallVec::new(),
    }
}

/// The signature compiled functions call each other with.
fn signature(params: &[Type], result: Type) -> Signature {
    let mut signature = Signature::new(CallConv::Tail);
    signature.params.push(AbiParam::new(I64));
    for &param in params {
        signature
            .params
            .extend(words(param).into_iter().map(AbiParam::new));
    }
    signature.returns.push(AbiParam::new(I64));
    signature
        .returns
        .extend(words(result).into_iter().map(AbiParam::new));
    signature
}

/// Defines [`Helper`], one variant per runtime function compiled code calls, in the order of a
/// code file's helper numbers, and [`helper_address`].
macro_rules! helpers {
    ($($name:ident => $function:path,)*) => {
        /// A runtime function compiled code calls. A code file names one by its number, which
        /// only the build that wrote the file reads.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        #[repr(u16)]
        pub enum Helper {
            $($name,)*
        }

        impl Helper {
            pub const ALL: &[Helper] = &[$(Helper::$name,)*];
        }

        /// The address of `helper` in this process.
        pub fn helper_address(helper: Helper) -> usize {
            match helper {
                $(Helper::$name => $function as *const () as usize,)*
            }
        }
    };
}

helpers! {
All => values::ry_all,
    Any => values::ry_any,
    Arith => values::ry_arith,
    ArithSv => values::ry_arith_sv,
    ArithVs => values::ry_arith_vs,
    BoxEqual => values::ry_box_equal,
    BoxNew => values::ry_box_new,
    Compare => values::ry_compare,
    CompareSv => values::ry_compare_sv,
    CompareVs => values::ry_compare_vs,
    CountTrue => values::ry_count_true,
    DictContains => values::ry_dict_contains,
    DictFind => values::ry_dict_find,
    DictInsert => values::ry_dict_insert,
    DictKeys => values::ry_dict_keys,
    DictLen => values::ry_dict_len,
    DictNew => values::ry_dict_new,
    DictPut => values::ry_dict_put,
    DictRemove => values::ry_dict_remove,
    DictValues => values::ry_dict_values,
    Elementwise => values::ry_elementwise,
    FormatDbl => values::ry_format_dbl,
    FormatFixed => values::ry_format_fixed,
    FormatI64 => values::ry_format_i64,
    FormatInt => values::ry_format_int,
    FormatPaddedInt => values::ry_format_padded_int,
    ListCoerce => values::ry_list_coerce,
    ListConcat => values::ry_list_concat,
    ListEndsWith => values::ry_list_ends_with,
    ListFree => values::ry_list_free,
    ListFromUtf8Lossy => values::ry_list_from_utf8_lossy,
    ListJoin => values::ry_list_join,
    ListNchar => values::ry_list_nchar,
    ListNew => values::ry_list_new,
    ListPaste => values::ry_list_paste,
    ListPush => values::ry_list_push,
    ListRangeI64 => values::ry_list_range_i64,
    ListRep => values::ry_list_rep,
    ListSelect => values::ry_list_select,
    ListSeq => values::ry_list_seq,
    ListSeqLen => values::ry_list_seq_len,
    ListSlice => values::ry_list_slice,
    ListSort => values::ry_list_sort,
    ListStartsWith => values::ry_list_starts_with,
    ListSumDbl => values::ry_list_sum_dbl,
    ListSwap => values::ry_list_swap,
    ListTolower => values::ry_list_tolower,
    ListToupper => values::ry_list_toupper,
    ListUnshare => values::ry_list_unshare,
    ListWithCapacity => values::ry_list_with_capacity,
    Logic => values::ry_logic,
    Math => values::ry_math,
    MaxDbl => values::ry_max_dbl,
    MaxInt => values::ry_max_int,
    MeanDbl => values::ry_mean_dbl,
    MinDbl => values::ry_min_dbl,
    MinInt => values::ry_min_int,
    Not => values::ry_not,
    PowerI64 => values::ry_power_i64,
    ProdDbl => values::ry_prod_dbl,
    Round => values::ry_round,
    StrAsciiCase => values::ry_str_ascii_case,
    StrCaselessAsciiEquals => values::ry_str_caseless_ascii_equals,
    StrCompare => values::ry_str_compare,
    StrConcat => values::ry_str_concat,
    StrContains => values::ry_str_contains,
    StrDropAffix => values::ry_str_drop_affix,
    StrEndsWith => values::ry_str_ends_with,
    StrEqual => values::ry_str_equal,
    StrFind => values::ry_str_find,
    StrFree => values::ry_str_free,
    StrNchar => values::ry_str_nchar,
    StrPiece => values::ry_str_piece,
    StrRepeat => values::ry_str_repeat,
    StrReplaceEach => values::ry_str_replace_each,
    StrSplit => values::ry_str_split,
    StrSplitBounds => values::ry_str_split_bounds,
    StrStartsWith => values::ry_str_starts_with,
    StrToDbl => values::ry_str_to_dbl,
    StrToI64 => values::ry_str_to_i64,
    StrToUtf8 => values::ry_str_to_utf8,
    StrTolower => values::ry_str_tolower,
    StrToupper => values::ry_str_toupper,
    StrTrim => values::ry_str_trim,
    SumDbl => values::ry_sum_dbl,
    SumI64 => values::ry_sum_i64,
    SumInt => values::ry_sum_int,
    CallR => crate::ry_call_r,
    ModulusLosesAccuracy => modulus_loses_accuracy,
    RealAcos => real_acos,
    RealAsin => real_asin,
    RealAtan => real_atan,
    RealCos => real_cos,
    RealExp => real_exp,
    RealIntegerDivide => real_integer_divide,
    RealLog => real_log,
    RealLog10 => real_log10,
    RealLog2 => real_log2,
    RealMax => real_max,
    RealMin => real_min,
    RealModulo => real_modulo,
    RealPlatformPower => real_platform_power,
    RealPower => real_power,
    RealRound => real_round,
    RealRoundAway => real_round_away,
    RealSign => real_sign,
    RealSin => real_sin,
    RealTan => real_tan,
    RoundDigits => round_digits,
    Floor => libcall_floor,
    Ceil => libcall_ceil,
    Trunc => libcall_trunc,
    Nearbyint => libcall_nearbyint,
    Fma => libcall_fma,
    Memcpy => libc::memcpy,
    Memset => libc::memset,
    Memmove => libc::memmove,
    Memcmp => libc::memcmp,
}

/// The helper that stands for the libcall Cranelift names `name`, which code on a CPU without
/// the matching instruction calls.
pub fn libcall_helper(name: &str) -> Option<Helper> {
    Some(match name {
        "floor" => Helper::Floor,
        "ceil" => Helper::Ceil,
        "trunc" => Helper::Trunc,
        "nearbyint" => Helper::Nearbyint,
        "fma" => Helper::Fma,
        "memcpy" => Helper::Memcpy,
        "memset" => Helper::Memset,
        "memmove" => Helper::Memmove,
        "memcmp" => Helper::Memcmp,
        _ => return None,
    })
}

/// Compiles `function`, whose calls of other functions index `callees`.
pub fn compile(
    function: &Function,
    callees: &[Callee],
    r_calls: &[usize],
) -> Result<Code, Refusal> {
    let frees = frees()?;
    ssa::with_module(Allocator::Backtracking, |module| -> Result<Code, Refusal> {
        let ids = declare(module, &function.locals[..function.params], function.result)?;
        define(module, None, frees, function, callees, r_calls, ids)?;
        module.finalize()?;
        Ok(Code {
            address: module.address(ids.0),
            trampoline: module.address(ids.1),
        })
    })
}

/// Declares a function of these types and its trampoline in `module`.
pub fn declare(
    module: &mut impl Target,
    params: &[Type],
    result: Type,
) -> Result<(FuncId, FuncId), Refusal> {
    let body = module.declare(&signature(params, result))?;
    let trampoline = module.declare(&trampoline_signature())?;
    Ok((body, trampoline))
}

/// Emits `function` and its trampoline into the code file `object`, at the ids [`declare`]
/// gave them. Its R calls index `sites`, the file's sites, and its words of the process come
/// from `slots`.
pub fn compile_into(
    object: &mut Relocatable,
    slots: &mut Slots,
    frees: Frees,
    function: &Function,
    callees: &[Callee],
    sites: &[usize],
    ids: (FuncId, FuncId),
) -> Result<(), Refusal> {
    define(object, Some(slots), frees, function, callees, sites, ids)
}

fn define(
    module: &mut impl Target,
    table: Option<&mut Slots>,
    frees: Frees,
    function: &Function,
    callees: &[Callee],
    r_calls: &[usize],
    (id, trampoline_id): (FuncId, FuncId),
) -> Result<(), Refusal> {
    let params = &function.locals[..function.params];
    let own_signature = signature(params, function.result);
    let mut builder = Builder::new(own_signature.clone());
    let own = builder.import_function(id, own_signature);
    let mut emitter = Emitter {
        builder,
        function,
        callees,
        r_calls,
        table,
        own,
        host: CallConv::Host,
        frees,
        free: None,
        variables: Vec::new(),
        blocks: Vec::new(),
        signatures: FxHashMap::default(),
        budget: None,
        ranges: ranges(function),
        current: 0,
    };
    emitter.emit_function()?;
    emitter.builder.seal_all_blocks();
    let compiled = emitter.builder.finalize();
    if std::env::var_os("RY_TYPED_DUMP").is_some() {
        eprintln!("{compiled}");
    }
    module.define(id, &compiled)?;
    trampoline(module, trampoline_id, id, params, function.result)
}

fn trampoline_signature() -> Signature {
    let mut outer = Signature::new(CallConv::Host);
    outer.params.extend([AbiParam::new(I64); 3]);
    outer.returns.push(AbiParam::new(I64));
    outer
}

/// Defines `id` as a function in the host's convention that calls the compiled function
/// `inner` with arguments read from an array of words and stores its value's words.
fn trampoline(
    module: &mut impl Target,
    id: FuncId,
    inner: FuncId,
    params: &[Type],
    result: Type,
) -> Result<(), Refusal> {
    let mut builder = Builder::new(trampoline_signature());
    let callee = builder.import_function(inner, signature(params, result));
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let &[budget, input, output] = builder.block_params(entry) else {
        return Err(Refusal::new("a trampoline without its parameters"));
    };
    let mut arguments: SmallVec<[Value; 8]> = SmallVec::new();
    arguments.push(budget);
    let mut offset = 0;
    for &param in params {
        for machine in words(param) {
            arguments.push(
                builder
                    .ins()
                    .load(machine, MemFlagsData::trusted(), input, offset),
            );
            offset += 8;
        }
    }
    let call = builder.ins().call(callee, &arguments);
    let results: SmallVec<[Value; 4]> = builder.inst_results(call).iter().copied().collect();
    let Some((&status, value)) = results.split_first() else {
        return Err(Refusal::new("a typed call without a status"));
    };
    for (index, &word) in value.iter().enumerate() {
        builder
            .ins()
            .store(MemFlagsData::trusted(), word, output, 8 * index as i32);
    }
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
    module.define(id, &builder.finalize())?;
    Ok(())
}

/// The entries compiled code frees a string's buffer and an object through. A drop frees only
/// when it takes the last reference, and the free functions follow the host's C convention,
/// under which a call costs its caller every floating-point register and most others. The
/// allocator would answer by keeping each value that is live across a drop in memory, on the
/// paths that never free too, so compiled code calls these entries instead: they preserve every
/// register and pass their argument on.
#[derive(Clone, Copy)]
pub struct Frees {
    string: Entry,
    object: Entry,
}

impl Frees {
    /// The functions of a code file's free entries ([`file_frees`]).
    pub fn ids(&self) -> SmallVec<[FuncId; 2]> {
        [self.string, self.object]
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Function(id) => Some(id),
                _ => None,
            })
            .collect()
    }
}

thread_local! {
    /// Compiled on first use and kept per thread, as the code modules are.
    static FREES: Cell<Option<Frees>> = const { Cell::new(None) };
}

fn frees() -> Result<Frees, Refusal> {
    if let Some(frees) = FREES.get() {
        return Ok(frees);
    }
    let frees = ssa::with_module(
        Allocator::Backtracking,
        |module| -> Result<Frees, Refusal> {
            let string = preserving(module, None, Helper::StrFree)?;
            let object = preserving(module, None, Helper::ListFree)?;
            module.finalize()?;
            Ok(Frees {
                string: Entry::Address(module.address(string)),
                object: Entry::Address(module.address(object)),
            })
        },
    )?;
    FREES.set(Some(frees));
    Ok(frees)
}

/// The free entries of a code file, defined in `object` after its functions.
pub fn file_frees(object: &mut Relocatable, slots: &mut Slots) -> Result<Frees, Refusal> {
    let string = preserving(object, Some(&mut *slots), Helper::StrFree)?;
    let list = preserving(object, Some(slots), Helper::ListFree)?;
    Ok(Frees {
        string: Entry::Function(string),
        object: Entry::Function(list),
    })
}

/// A function in the register-preserving convention that passes its one word to `helper`.
fn preserving(
    module: &mut impl Target,
    table: Option<&mut Slots>,
    helper: Helper,
) -> Result<FuncId, Refusal> {
    let mut outer = Signature::new(CallConv::Preserving);
    outer.params.push(AbiParam::new(I64));
    let id = module.declare(&outer)?;
    let mut builder = Builder::new(outer);
    let mut inner = Signature::new(CallConv::Host);
    inner.params.push(AbiParam::new(I64));
    let inner = builder.import_signature(inner);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let &[pointer] = builder.block_params(entry) else {
        return Err(Refusal::new("a free entry without its parameter"));
    };
    let callee = match table {
        None => builder.ins().iconst(I64, helper_address(helper) as i64),
        Some(table) => builder.ins().table_word(table.number(Slot::Helper(helper))),
    };
    builder.ins().call_indirect(inner, callee, &[pointer]);
    builder.ins().return_(&[]);
    builder.seal_all_blocks();
    module.define(id, &builder.finalize())?;
    Ok(id)
}

type SignatureKey = (SmallVec<[Machine; 8]>, SmallVec<[Machine; 3]>);

struct Emitter<'a> {
    builder: Builder,
    function: &'a Function,
    callees: &'a [Callee],
    /// The address of each [`crate::lower::RCall`] the function makes, as `Op::CallR` indexes
    /// them, or in a code file the index of its site there.
    r_calls: &'a [usize],
    /// The code file's offset table, where code for a file loads every word of the process.
    table: Option<&'a mut Slots>,
    own: FuncRef,
    host: CallConv,
    frees: Frees,
    /// The signature of a [`Frees`] entry, imported on the function's first drop.
    free: Option<SigRef>,
    variables: Vec<SmallVec<[Variable; 2]>>,
    blocks: Vec<Block>,
    signatures: FxHashMap<SignatureKey, SigRef>,
    budget: Option<Variable>,
    ranges: Ranges,
    /// The block being emitted, as its index in the function.
    current: usize,
}

impl Emitter<'_> {
    fn emit_function(&mut self) -> Result<(), Refusal> {
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let parameters: SmallVec<[Value; 8]> =
            self.builder.block_params(entry).iter().copied().collect();
        let Some((&budget, mut arguments)) = parameters.split_first() else {
            return Err(Refusal::new("a function without its budget"));
        };
        let budget_variable = self.builder.declare_var(I64);
        self.builder.def_var(budget_variable, budget);
        self.budget = Some(budget_variable);
        for (index, &ty) in self.function.locals.iter().enumerate() {
            let mut variables = SmallVec::new();
            for machine in words(ty) {
                let variable = self.builder.declare_var(machine);
                let initial = if index < self.function.params {
                    let (&first, rest) = arguments
                        .split_first()
                        .ok_or(Refusal::new("a function without an argument"))?;
                    arguments = rest;
                    first
                } else {
                    self.zero(machine)
                };
                self.builder.def_var(variable, initial);
                variables.push(variable);
            }
            self.variables.push(variables);
        }
        self.blocks = (0..self.function.blocks.len())
            .map(|_| self.builder.create_block())
            .collect();
        // The budget counts the calls still allowed below this one, as R's `expressions` limit
        // does, and the stack limit the bytes, so the deepest recursion gives up instead of
        // overflowing the machine stack.
        let deep = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::SignedLessThan, budget, 1);
        let frame = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            8,
            3,
        ));
        let here = self.builder.ins().stack_addr(I64, frame, 0);
        let limit = self.word(Slot::StackLimit, || {
            &crate::STACK_LIMIT as *const AtomicUsize as i64
        });
        let address = limit;
        let limit = self
            .builder
            .ins()
            .load(I64, MemFlagsData::trusted(), address, 0);
        let low = self
            .builder
            .ins()
            .icmp(IntCC::UnsignedLessThan, here, limit);
        let exhausted = self.builder.ins().bor(deep, low);
        // The stop leaves [`crate::EXHAUSTED_LIMIT`] as the limit, which tells the native entry
        // why the code gave up and makes any frame entered before it returns give up too.
        let failure = self.builder.create_block();
        let go = self.builder.create_block();
        self.builder.set_cold_block(failure);
        self.builder.ins().brif(exhausted, failure, &[], go, &[]);
        self.builder.switch_to_block(failure);
        let sentinel = self
            .builder
            .ins()
            .iconst(I64, crate::EXHAUSTED_LIMIT as i64);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), sentinel, address, 0);
        self.give_up();
        self.builder.switch_to_block(go);
        let first = *self
            .blocks
            .first()
            .ok_or(Refusal::new("a function without blocks"))?;
        self.builder.ins().jump(first, &[]);
        let function = self.function;
        for (index, block) in function.blocks.iter().enumerate() {
            self.builder.switch_to_block(self.blocks[index]);
            self.current = index;
            self.emit_block(block)?;
        }
        Ok(())
    }

    fn emit_block(&mut self, block: &IrBlock) -> Result<(), Refusal> {
        // A block that ends by comparing two doubles and branching on the answer branches on
        // the machine comparison itself. Building the logical first, with its `NA` for an
        // unordered pair, and then testing it costs three times the instructions, and this is
        // the shape of every `if` and `while` on doubles.
        if let (
            [
                plain @ ..,
                Inst {
                    dest: Some(compared),
                    op: Op::Compare(op, Item::Dbl, a, b),
                    ..
                },
                truth @ Inst {
                    dest: Some(tested),
                    op: Op::Truth(source),
                    ..
                },
            ],
            Terminator::Branch {
                condition,
                then,
                otherwise,
            },
        ) = (block.insts.as_slice(), &block.terminator)
            && source == compared
            && condition == tested
        {
            for inst in plain {
                self.emit_inst(inst)?;
            }
            let (x, y) = (self.one(*a)?, self.one(*b)?);
            let holds = self.builder.ins().fcmp(float_condition(*op), x, y);
            let unordered = self.builder.ins().fcmp(FloatCC::Unordered, x, y);
            // Both locals keep a value for a later reader. An unordered pair gives up below,
            // so the logical is never the `NA` it would otherwise be.
            let logical = self.logical(holds);
            self.set(*compared, &[logical])?;
            self.set(*tested, &[logical])?;
            let (then, otherwise) = (self.block(*then)?, self.block(*otherwise)?);
            if *op == Compare::Ne {
                // `!=` holds for an unordered pair, so that case is ruled out first.
                self.fail_if(unordered, &truth.fail_drops)?;
                self.builder.ins().brif(holds, then, &[], otherwise, &[]);
            } else {
                // Every other comparison is false for an unordered pair, so only the path
                // where it does not hold needs the second test.
                let ordered = self.builder.create_block();
                self.builder.ins().brif(holds, then, &[], ordered, &[]);
                self.builder.switch_to_block(ordered);
                self.fail_if(unordered, &truth.fail_drops)?;
                self.builder.ins().jump(otherwise, &[]);
            }
            return Ok(());
        }
        for inst in &block.insts {
            self.emit_inst(inst)?;
        }
        match block.terminator {
            Terminator::Jump(target) => {
                let target = self.block(target)?;
                self.builder.ins().jump(target, &[]);
            }
            Terminator::Branch {
                condition,
                then,
                otherwise,
            } => {
                let condition = self.one(condition)?;
                let (then, otherwise) = (self.block(then)?, self.block(otherwise)?);
                self.builder
                    .ins()
                    .brif(condition, then, &[], otherwise, &[]);
            }
            Terminator::Return(local) => {
                let ty = self.function.ty(local);
                let compatible = match (ty, self.function.result) {
                    (Type::Bool, Type::Lgl) => true,
                    (Type::Tag(a), Type::Tag(b)) => shapes::within(a, b),
                    (a, b) => a == b,
                };
                if words(ty) != words(self.function.result) || !compatible {
                    return Err(Refusal::new("a `return` of another type than the result"));
                }
                let values = self.get(local)?;
                if self.function.is_param(local) && ty.is_counted() {
                    self.dup(ty, &values);
                }
                let status = self.builder.ins().iconst(I64, 0);
                let mut results: SmallVec<[Value; 3]> = SmallVec::from_slice(&[status]);
                results.extend(values);
                self.builder.ins().return_(&results);
            }
            Terminator::Fail => self.give_up(),
        }
        Ok(())
    }

    fn block(&self, block: crate::ir::BlockId) -> Result<Block, Refusal> {
        self.blocks
            .get(block.0 as usize)
            .copied()
            .ok_or(Refusal::new("a jump to a missing block"))
    }

    /// Returns the failure status from the current block.
    fn give_up(&mut self) {
        let mut results: SmallVec<[Value; 3]> = SmallVec::new();
        results.push(self.builder.ins().iconst(I64, 1));
        for machine in words(self.function.result) {
            let zero = self.zero(machine);
            results.push(zero);
        }
        self.builder.ins().return_(&results);
    }

    /// Gives up the call when `condition` holds, dropping `drops` first.
    fn fail_if(&mut self, condition: Value, drops: &[Local]) -> Result<(), Refusal> {
        let failure = self.builder.create_block();
        let go = self.builder.create_block();
        self.builder.set_cold_block(failure);
        self.builder.ins().brif(condition, failure, &[], go, &[]);
        self.builder.switch_to_block(failure);
        for &local in drops {
            let values = self.get(local)?;
            self.drop(self.function.ty(local), &values);
        }
        self.give_up();
        self.builder.switch_to_block(go);
        Ok(())
    }

    /// Gives up the call when `value`, a scalar of `ty`, is `NA` (or `NaN`, for a double).
    fn fail_if_na(&mut self, value: Value, ty: Type, drops: &[Local]) -> Result<(), Refusal> {
        let missing = match ty {
            Type::Dbl => self.builder.ins().fcmp(FloatCC::Unordered, value, value),
            Type::Int | Type::Lgl => self.is_na_integer(value),
            _ => return Ok(()),
        };
        self.fail_if(missing, drops)
    }

    fn zero(&mut self, machine: Machine) -> Value {
        if machine == F64 {
            self.builder.ins().f64const(0.0)
        } else {
            self.builder.ins().iconst(machine, 0)
        }
    }

    fn get(&mut self, local: Local) -> Result<SmallVec<[Value; 2]>, Refusal> {
        let variables = self
            .variables
            .get(local.0 as usize)
            .ok_or(Refusal::new("a missing local"))?
            .clone();
        Ok(variables
            .iter()
            .map(|&variable| self.builder.use_var(variable))
            .collect())
    }

    /// The range of `local` where the block being emitted reads it.
    fn range(&self, local: Local) -> Range {
        self.ranges.of(self.current, local)
    }

    /// The single machine word of `local`.
    fn one(&mut self, local: Local) -> Result<Value, Refusal> {
        let values = self.get(local)?;
        values
            .first()
            .copied()
            .ok_or(Refusal::new("a local without a value"))
    }

    fn set(&mut self, local: Local, values: &[Value]) -> Result<(), Refusal> {
        let variables = self
            .variables
            .get(local.0 as usize)
            .ok_or(Refusal::new("a missing local"))?
            .clone();
        if variables.len() != values.len() {
            return Err(Refusal::new("a value with the wrong number of words"));
        }
        for (&variable, &value) in variables.iter().zip(values) {
            self.builder.def_var(variable, value);
        }
        Ok(())
    }

    /// Calls the runtime function `helper` in the host's convention.
    fn call(
        &mut self,
        helper: Helper,
        params: &[Machine],
        returns: &[Machine],
        arguments: &[Value],
    ) -> SmallVec<[Value; 3]> {
        let key: SignatureKey = (SmallVec::from_slice(params), SmallVec::from_slice(returns));
        let signature = match self.signatures.get(&key) {
            Some(&signature) => signature,
            None => {
                let mut signature = Signature::new(self.host);
                signature
                    .params
                    .extend(params.iter().map(|&machine| AbiParam::new(machine)));
                signature
                    .returns
                    .extend(returns.iter().map(|&machine| AbiParam::new(machine)));
                let imported = self.builder.import_signature(signature);
                self.signatures.insert(key, imported);
                imported
            }
        };
        let callee = self.word(Slot::Helper(helper), || helper_address(helper) as i64);
        let call = self
            .builder
            .ins()
            .call_indirect(signature, callee, arguments);
        self.builder.inst_results(call).iter().copied().collect()
    }

    /// Calls a runtime function whose parameters are all 64-bit words and which returns one
    /// value of `returns`.
    fn call_words(&mut self, helper: Helper, returns: Machine, arguments: &[Value]) -> Value {
        let params: SmallVec<[Machine; 8]> = arguments
            .iter()
            .map(|&argument| self.builder.value_type(argument))
            .collect();
        let results = self.call(helper, &params, &[returns], arguments);
        results[0]
    }

    fn iconst(&mut self, value: i64) -> Value {
        self.builder.ins().iconst(I64, value)
    }

    /// A word that differs between processes: the immediate `value` gives in the session, or
    /// the word of `slot` in a code file's offset table.
    fn word(&mut self, slot: Slot, value: impl FnOnce() -> i64) -> Value {
        match &mut self.table {
            None => {
                let value = value();
                self.iconst(value)
            }
            Some(table) => {
                let number = table.number(slot);
                self.builder.ins().table_word(number)
            }
        }
    }

    /// The id of `shape` in the process that runs the code, as an `I32`.
    fn shape_id(&mut self, shape: u32) -> Value {
        if self.table.is_none() {
            return self.builder.ins().iconst(I32, i64::from(shape));
        }
        let word = self.word(Slot::Shape(shape), || i64::from(shape));
        self.builder.ins().ireduce(I32, word)
    }

    /// Calls the function at `entry`, whose signature is `signature`.
    fn call_entry(&mut self, entry: Entry, signature: Signature, arguments: &[Value]) -> ssa::Inst {
        match entry {
            Entry::Function(id) => {
                let function = self.builder.import_function(id, signature);
                self.builder.ins().call(function, arguments)
            }
            Entry::Address(address) => {
                let signature = self.builder.import_signature(signature);
                let callee = self.iconst(address as i64);
                self.builder
                    .ins()
                    .call_indirect(signature, callee, arguments)
            }
            Entry::Callee(index) => {
                let signature = self.builder.import_signature(signature);
                let callee = self.word(Slot::Callee(index), || 0);
                self.builder
                    .ins()
                    .call_indirect(signature, callee, arguments)
            }
            Entry::Cell(address) => {
                let signature = self.builder.import_signature(signature);
                let cell = self.iconst(address as i64);
                let callee = self
                    .builder
                    .ins()
                    .load(I64, MemFlagsData::trusted(), cell, 0);
                self.builder
                    .ins()
                    .call_indirect(signature, callee, arguments)
            }
        }
    }

    /// A 32-bit integer as a sign-extended 64-bit word, and an `I64` as itself.
    fn wide(&mut self, value: Value) -> Value {
        if self.builder.value_type(value) == I64 {
            return value;
        }
        self.builder.ins().sextend(I64, value)
    }

    /// A scalar as the two words the runtime takes for an item: an integer or logical
    /// sign-extended, a double's bits, or a string's two words.
    fn item_words(&mut self, local: Local) -> Result<(Value, Value), Refusal> {
        let values = self.get(local)?;
        let ty = self.function.ty(local);
        Ok(match ty {
            Type::Str => (values[0], values[1]),
            Type::Dbl => {
                let bits = self
                    .builder
                    .ins()
                    .bitcast(I64, MemFlagsData::new(), values[0]);
                (bits, self.iconst(0))
            }
            Type::Int | Type::Lgl | Type::Bool | Type::I64 | Type::Record(_) | Type::Tag(_) => {
                let wide = self.wide(values[0]);
                (wide, self.iconst(0))
            }
            // A ry list is an item of a list of lists, held as its pointer.
            Type::Vector { array: true, .. } => (values[0], self.iconst(0)),
            _ => return Err(Refusal::new("an item that is not a scalar")),
        })
    }

    /// The owned flag the runtime takes for the operand at `position`.
    fn owned(&mut self, inst: &Inst, position: usize) -> Value {
        self.iconst(i64::from(inst.moves(position)))
    }

    /// Fails when `pointer`, a runtime function's result, is null.
    fn fail_if_null(&mut self, pointer: Value, inst: &Inst) -> Result<(), Refusal> {
        let null = self.builder.ins().icmp_imm_s(IntCC::Equal, pointer, 0);
        self.fail_if(null, &inst.fail_drops)
    }

    fn is_na_integer(&mut self, value: Value) -> Value {
        self.builder
            .ins()
            .icmp_imm_s(IntCC::Equal, value, NA_INTEGER)
    }

    /// A logical from a machine condition.
    fn logical(&mut self, condition: Value) -> Value {
        self.builder.ins().uextend(I32, condition)
    }

    fn na_or(&mut self, na: Value, value: Value) -> Value {
        let missing = self.builder.ins().iconst(I32, NA_INTEGER);
        self.builder.ins().select(na, missing, value)
    }

    fn dup(&mut self, ty: Type, values: &[Value]) {
        match ty {
            Type::Str => {
                let heap = self.heap_string(values[0], values[1]);
                self.count(heap, values[0], None);
            }
            ty if ty.is_object() => {
                let live = self.builder.ins().icmp_imm_s(IntCC::NotEqual, values[0], 0);
                self.count(live, values[0], None);
            }
            _ => {}
        }
    }

    fn drop(&mut self, ty: Type, values: &[Value]) {
        match ty {
            Type::Str => {
                let heap = self.heap_string(values[0], values[1]);
                self.count(heap, values[0], Some(self.frees.string));
            }
            ty if ty.is_object() => {
                let live = self.builder.ins().icmp_imm_s(IntCC::NotEqual, values[0], 0);
                self.count(live, values[0], Some(self.frees.object));
            }
            _ => {}
        }
    }

    /// Whether a string's words point at a counted buffer: it is not stored inline and not null.
    fn heap_string(&mut self, lo: Value, hi: Value) -> Value {
        let pointer = self.builder.ins().icmp_imm_s(IntCC::NotEqual, lo, 0);
        let outline = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, hi, 0);
        self.builder.ins().band(pointer, outline)
    }

    /// Counts a reference to the object at `pointer` up, or down when `free` is given, which it
    /// calls when the count reaches zero, all only when `live`. A saturated count is permanent.
    fn count(&mut self, live: Value, pointer: Value, free: Option<Entry>) {
        let (counted, done) = (self.builder.create_block(), self.builder.create_block());
        self.builder.ins().brif(live, counted, &[], done, &[]);
        self.builder.switch_to_block(counted);
        let count = self
            .builder
            .ins()
            .load(I32, MemFlagsData::trusted(), pointer, 0);
        let saturated = self.builder.ins().icmp_imm_s(IntCC::Equal, count, -1);
        let change = self.builder.create_block();
        self.builder.ins().brif(saturated, done, &[], change, &[]);
        self.builder.switch_to_block(change);
        match free {
            None => {
                let up = self.builder.ins().iadd_imm_s(count, 1);
                self.builder
                    .ins()
                    .store(MemFlagsData::trusted(), up, pointer, 0);
                self.builder.ins().jump(done, &[]);
            }
            Some(free) => {
                let down = self.builder.ins().iadd_imm_s(count, -1);
                let (keep, release) = (self.builder.create_block(), self.builder.create_block());
                self.builder.ins().brif(down, keep, &[], release, &[]);
                self.builder.switch_to_block(keep);
                self.builder
                    .ins()
                    .store(MemFlagsData::trusted(), down, pointer, 0);
                self.builder.ins().jump(done, &[]);
                self.builder.switch_to_block(release);
                self.builder.set_cold_block(release);
                match free {
                    Entry::Address(address) => {
                        let signature = match self.free {
                            Some(signature) => signature,
                            None => {
                                let imported = self.builder.import_signature(free_signature());
                                self.free = Some(imported);
                                imported
                            }
                        };
                        let callee = self.iconst(address as i64);
                        self.builder
                            .ins()
                            .call_indirect(signature, callee, &[pointer]);
                    }
                    entry => {
                        self.call_entry(entry, free_signature(), &[pointer]);
                    }
                }
                self.builder.ins().jump(done, &[]);
            }
        }
        self.builder.switch_to_block(done);
    }

    fn emit_inst(&mut self, inst: &Inst) -> Result<(), Refusal> {
        let function = self.function;
        let ty = |local: Local| function.ty(local);
        let result: SmallVec<[Value; 2]> = match &inst.op {
            Op::Const(constant) => match *constant {
                Constant::Int(value) | Constant::Lgl(value) => {
                    SmallVec::from_slice(&[self.builder.ins().iconst(I32, i64::from(value))])
                }
                Constant::Dbl(value) => SmallVec::from_slice(&[self.builder.ins().f64const(value)]),
                Constant::I64(value) => SmallVec::from_slice(&[self.iconst(value)]),
                // A string longer than fits inline points at a static buffer, which a code file
                // holds in its read-only data.
                Constant::Str(lo, hi) if lo != 0 && (hi as i64) >= 0 && self.table.is_some() => {
                    // SAFETY: lowering made the buffer with `ry_str_static`, which never frees it.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            (lo as *const u8).add(values::STR_HEADER),
                            hi as usize,
                        )
                    };
                    let offset = self.table.as_mut().map_or(0, |table| table.string(bytes));
                    let pointer = self.word(Slot::Data(offset), || 0);
                    SmallVec::from_slice(&[pointer, self.iconst(hi as i64)])
                }
                Constant::Str(lo, hi) => {
                    SmallVec::from_slice(&[self.iconst(lo as i64), self.iconst(hi as i64)])
                }
                Constant::Null => SmallVec::from_slice(&[self.iconst(0)]),
                Constant::Object(shape) => SmallVec::from_slice(&[
                    self.word(Slot::Constant(shape), || shapes::constant(shape) as i64)
                ]),
                Constant::Shape(shape) => SmallVec::from_slice(&[self.shape_id(shape)]),
            },
            Op::Copy(a) | Op::Relabel(a, _) => {
                let values = self.get(*a)?;
                if !inst.moves(0) {
                    self.dup(ty(*a), &values);
                }
                values
            }
            Op::Arith(op, item, a, b) => {
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                let value = match item {
                    Item::Dbl => {
                        if *op == Arith::Mod {
                            let lost = self.call(
                                Helper::ModulusLosesAccuracy,
                                &[F64, F64],
                                &[I32],
                                &[x, y],
                            )[0];
                            self.fail_if(lost, &inst.fail_drops)?;
                        }
                        self.real_arith(*op, x, y)?
                    }
                    Item::I64 => {
                        let ranges = (self.range(*a), self.range(*b));
                        self.i64_arith(*op, x, y, ranges, inst)?
                    }
                    _ => self.integer_arith(*op, x, y, inst)?,
                };
                SmallVec::from_slice(&[value])
            }
            Op::Compare(op, item, a, b) => {
                let value = match item {
                    Item::Str => {
                        let (a, b) = (self.get(*a)?, self.get(*b)?);
                        let words = [a[0], a[1], b[0], b[1]];
                        match op {
                            Compare::Eq | Compare::Ne => {
                                let equal =
                                    self.call(Helper::StrEqual, &[I64; 4], &[I32], &words)[0];
                                match op {
                                    Compare::Eq => equal,
                                    _ => self.builder.ins().bxor_imm_s(equal, 1),
                                }
                            }
                            _ => {
                                let order =
                                    self.call(Helper::StrCompare, &[I64; 4], &[I32], &words)[0];
                                let compared =
                                    self.builder
                                        .ins()
                                        .icmp_imm_s(integer_condition(*op), order, 0);
                                self.logical(compared)
                            }
                        }
                    }
                    Item::Dbl => {
                        let (x, y) = (self.one(*a)?, self.one(*b)?);
                        let unordered = self.builder.ins().fcmp(FloatCC::Unordered, x, y);
                        let compared = self.builder.ins().fcmp(float_condition(*op), x, y);
                        let compared = self.logical(compared);
                        self.na_or(unordered, compared)
                    }
                    Item::I64 => {
                        let (x, y) = (self.one(*a)?, self.one(*b)?);
                        let compared = self.builder.ins().icmp(integer_condition(*op), x, y);
                        self.logical(compared)
                    }
                    Item::Int | Item::Lgl => {
                        let (x, y) = (self.one(*a)?, self.one(*b)?);
                        let na = self.either_na(x, y);
                        let compared = self.builder.ins().icmp(integer_condition(*op), x, y);
                        let compared = self.logical(compared);
                        self.na_or(na, compared)
                    }
                    Item::Record(_) | Item::Tag(_) | Item::Array(_)
                        if matches!(op, Compare::Eq | Compare::Ne) =>
                    {
                        let (x, y) = (self.one(*a)?, self.one(*b)?);
                        let equal = self.call(Helper::BoxEqual, &[I64; 2], &[I32], &[x, y])[0];
                        match op {
                            Compare::Eq => equal,
                            _ => self.builder.ins().bxor_imm_s(equal, 1),
                        }
                    }
                    Item::Record(_) | Item::Tag(_) | Item::Array(_) => {
                        return Err(Refusal::new("an ordering of records, tags or lists"));
                    }
                };
                SmallVec::from_slice(&[value])
            }
            Op::Not(a) => {
                let x = self.one(*a)?;
                let na = self.is_na_integer(x);
                let flipped = self.builder.ins().bxor_imm_s(x, 1);
                SmallVec::from_slice(&[self.na_or(na, flipped)])
            }
            Op::And(a, b) | Op::Or(a, b) => {
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                // `FALSE & NA` is `FALSE` and `TRUE | NA` is `TRUE`: the deciding value wins.
                let decisive = i64::from(matches!(inst.op, Op::Or(..)));
                let x_decides = self.builder.ins().icmp_imm_s(IntCC::Equal, x, decisive);
                let y_decides = self.builder.ins().icmp_imm_s(IntCC::Equal, y, decisive);
                let decided = self.builder.ins().bor(x_decides, y_decides);
                let na = self.either_na(x, y);
                let decisive = self.builder.ins().iconst(I32, decisive);
                let other = self.builder.ins().bxor_imm_s(decisive, 1);
                let undecided = self.na_or(na, other);
                SmallVec::from_slice(&[self.builder.ins().select(decided, decisive, undecided)])
            }
            Op::Math(op, item, a) => {
                let x = self.one(*a)?;
                let value = match item {
                    Item::Dbl => self.real_math(*op, x, inst)?,
                    Item::I64 => {
                        // Only the most negative `I64` has no negation or absolute value.
                        let lowest = self.builder.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                        self.fail_if(lowest, &inst.fail_drops)?;
                        match op {
                            Math::Neg => self.builder.ins().ineg(x),
                            Math::Abs => self.builder.ins().iabs(x),
                            _ => return Err(Refusal::new("an I64 math function")),
                        }
                    }
                    _ => {
                        let na = self.is_na_integer(x);
                        let value = match op {
                            Math::Neg => self.builder.ins().ineg(x),
                            Math::Abs => self.builder.ins().iabs(x),
                            _ => return Err(Refusal::new("an integer math function")),
                        };
                        self.na_or(na, value)
                    }
                };
                SmallVec::from_slice(&[value])
            }
            Op::CountBits(count, a) => {
                let x = self.one(*a)?;
                let counted = match count {
                    BitCount::Ones => self.builder.ins().popcnt(x),
                    BitCount::LeadingZeros => self.builder.ins().clz(x),
                    BitCount::TrailingZeros => self.builder.ins().ctz(x),
                };
                SmallVec::from_slice(&[counted])
            }
            Op::Overflowing(overflow, op, a, b) => {
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                let (value, overflowed) = match op {
                    Arith::Add => self.builder.ins().sadd_overflow(x, y),
                    Arith::Sub => self.builder.ins().ssub_overflow(x, y),
                    Arith::Mul => self.builder.ins().smul_overflow(x, y),
                    _ => return Err(Refusal::new("an overflowing operation other than + - *")),
                };
                let result = match overflow {
                    Overflow::Wrap => value,
                    Overflow::Test => self.builder.ins().uextend(I32, overflowed),
                    Overflow::Saturate => {
                        // An overflowing sum or difference has the first operand's sign, and an
                        // overflowing product the sign the operands' signs give.
                        let sign = match op {
                            Arith::Mul => self.builder.ins().bxor(x, y),
                            _ => x,
                        };
                        let negative =
                            self.builder
                                .ins()
                                .icmp_imm_s(IntCC::SignedLessThan, sign, 0);
                        let (lowest, highest) = (self.iconst(i64::MIN), self.iconst(i64::MAX));
                        let bound = self.builder.ins().select(negative, lowest, highest);
                        self.builder.ins().select(overflowed, bound, value)
                    }
                };
                SmallVec::from_slice(&[result])
            }
            Op::Bits(op, a, b) => {
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                if matches!(op, Bits::ShiftLeft | Bits::ShiftRight | Bits::ShiftRightZf) {
                    // An unsigned comparison also catches a negative count.
                    let out_of_range =
                        self.builder
                            .ins()
                            .icmp_imm_u(IntCC::UnsignedGreaterThan, y, 63);
                    self.fail_if(out_of_range, &inst.fail_drops)?;
                }
                let ins = self.builder.ins();
                let value = match op {
                    Bits::And => ins.band(x, y),
                    Bits::Or => ins.bor(x, y),
                    Bits::Xor => ins.bxor(x, y),
                    Bits::ShiftLeft => ins.ishl(x, y),
                    Bits::ShiftRight => ins.sshr(x, y),
                    Bits::ShiftRightZf => ins.ushr(x, y),
                };
                SmallVec::from_slice(&[value])
            }
            Op::IsNan(a) => {
                let x = self.one(*a)?;
                let unordered = self.builder.ins().fcmp(FloatCC::Unordered, x, x);
                let bits = self.builder.ins().bitcast(I64, MemFlagsData::new(), x);
                let payload = self.builder.ins().band_imm_u(bits, 0xFFFF_FFFF);
                let marked = self.builder.ins().icmp_imm_u(IntCC::Equal, payload, 1954);
                let na = self.builder.ins().band(unordered, marked);
                self.fail_if(na, &inst.fail_drops)?;
                SmallVec::from_slice(&[self.logical(unordered)])
            }
            Op::RoundDigits(a, digits) => {
                let (x, digits) = (self.one(*a)?, self.one(*digits)?);
                let rounded = self.call(Helper::RoundDigits, &[F64, F64], &[F64], &[x, digits])[0];
                SmallVec::from_slice(&[rounded])
            }
            Op::ToDbl(a) => {
                let x = self.one(*a)?;
                let converted = self.builder.ins().fcvt_from_sint(F64, x);
                let value = if ty(*a) == Type::I64 || self.range(*a).lo > ranges::NA {
                    converted
                } else {
                    let na = self.is_na_integer(x);
                    let missing = self.builder.ins().f64const(f64::from_bits(NA_REAL_BITS));
                    self.builder.ins().select(na, missing, converted)
                };
                SmallVec::from_slice(&[value])
            }
            Op::ToI64(a) => {
                let x = self.one(*a)?;
                let value = match ty(*a) {
                    Type::I64 => x,
                    Type::Dbl => {
                        // Exactly a whole number in [-2^63, 2^63): `NaN` fails the first test.
                        let whole = self.builder.ins().trunc(x);
                        let exact = self.builder.ins().fcmp(FloatCC::Equal, whole, x);
                        let low = self.builder.ins().f64const(-9_223_372_036_854_775_808.0);
                        let high = self.builder.ins().f64const(9_223_372_036_854_775_808.0);
                        let above = self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, x, low);
                        let below = self.builder.ins().fcmp(FloatCC::LessThan, x, high);
                        let inside = self.builder.ins().band(above, below);
                        let fits = self.builder.ins().band(exact, inside);
                        let outside = self.builder.ins().bxor_imm_s(fits, 1);
                        self.fail_if(outside, &inst.fail_drops)?;
                        self.builder.ins().fcvt_to_sint_sat(I64, x)
                    }
                    Type::Int => {
                        let na = self.is_na_integer(x);
                        self.fail_if(na, &inst.fail_drops)?;
                        self.wide(x)
                    }
                    _ => return Err(Refusal::new("an I64 from a value that is not a number")),
                };
                SmallVec::from_slice(&[value])
            }
            Op::LglToInt(a) => SmallVec::from_slice(&[self.one(*a)?]),
            Op::CountOfI64(a) => {
                let x = self.one(*a)?;
                let outside = self.builder.ins().icmp_imm_u(
                    IntCC::UnsignedGreaterThan,
                    x,
                    i64::from(i32::MAX),
                );
                self.fail_if(outside, &inst.fail_drops)?;
                SmallVec::from_slice(&[self.builder.ins().ireduce(I32, x)])
            }
            Op::Truth(a) => {
                let x = self.one(*a)?;
                if self.range(*a).lo <= ranges::NA {
                    let na = self.is_na_integer(x);
                    self.fail_if(na, &inst.fail_drops)?;
                }
                SmallVec::from_slice(&[x])
            }
            Op::Same(a, b) => {
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                let same = self.builder.ins().icmp(IntCC::Equal, x, y);
                SmallVec::from_slice(&[self.builder.ins().uextend(I32, same)])
            }
            Op::Step(a, b) => {
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                SmallVec::from_slice(&[self.builder.ins().iadd(x, y)])
            }
            Op::Length(a) => {
                let pointer = self.one(*a)?;
                let length =
                    self.builder
                        .ins()
                        .load(I64, MemFlagsData::trusted(), pointer, LENGTH_OFFSET);
                if inst.dest.is_some_and(|dest| ty(dest) == Type::I64) {
                    SmallVec::from_slice(&[length])
                } else {
                    SmallVec::from_slice(&[self.builder.ins().ireduce(I32, length)])
                }
            }
            Op::Get(a, index) => {
                let item = ty(*a)
                    .items()
                    .ok_or(Refusal::new("an index into a non-vector"))?;
                let pointer = self.one(*a)?;
                let index = self.one(*index)?;
                let address = self.item_address(pointer, index, item, inst)?;
                let values = self.load_item(address, item);
                self.dup(Type::of_item(item), &values);
                values
            }
            Op::ItemAt(a, position) => {
                let item = ty(*a)
                    .items()
                    .ok_or(Refusal::new("an item of a non-vector"))?;
                let pointer = self.one(*a)?;
                let position = self.one(*position)?;
                let address = self.item_address_unchecked(pointer, position, item);
                let values = self.load_item(address, item);
                self.dup(Type::of_item(item), &values);
                values
            }
            Op::Set(a, index, value) => {
                let item = ty(*a)
                    .items()
                    .ok_or(Refusal::new("an assignment into a non-vector"))?;
                let pointer = self.one(*a)?;
                let index = self.one(*index)?;
                // The bounds are checked before the list is made unique, so a failure still
                // finds it where it was.
                self.item_address(pointer, index, item, inst)?;
                if let Type::Vector { array: true, .. } = ty(*a)
                    && item != Item::Str
                {
                    // An Array holds no `NA`, so storing one leaves it to the interpreter.
                    let value = self.one(*value)?;
                    self.fail_if_na(value, Type::of_item(item), &inst.fail_drops)?;
                }
                let unique = self.unique(pointer, inst.moves(0));
                let address = self.item_address_unchecked(unique, index, item);
                let values = self.get(*value)?;
                let item_type = Type::of_item(item);
                if item_type.is_counted() {
                    if !inst.moves(2) {
                        self.dup(item_type, &values);
                    }
                    let old = self.load_item(address, item);
                    self.drop(item_type, &old);
                }
                self.store_item(address, item, &values);
                SmallVec::from_slice(&[unique])
            }
            Op::NewVector(item, count) => {
                let count = self.one(*count)?;
                let negative = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, count, 0);
                self.fail_if(negative, &inst.fail_drops)?;
                let kind = self.iconst(kind(*item));
                let count = self.wide(count);
                let list = self.call_words(Helper::ListNew, I64, &[kind, count]);
                SmallVec::from_slice(&[list])
            }
            Op::EmptyList(item, capacity) => {
                let capacity = self.one(*capacity)?;
                let negative = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, capacity, 0);
                self.fail_if(negative, &inst.fail_drops)?;
                let kind = self.iconst(kind(*item));
                let capacity = self.wide(capacity);
                let list = self.call_words(Helper::ListWithCapacity, I64, &[kind, capacity]);
                SmallVec::from_slice(&[list])
            }
            Op::SeqLen(count) => {
                let count = self.one(*count)?;
                let negative = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, count, 0);
                self.fail_if(negative, &inst.fail_drops)?;
                let count = self.wide(count);
                let list = self.call_words(Helper::ListSeqLen, I64, &[count]);
                SmallVec::from_slice(&[list])
            }
            Op::Range(from, to) => {
                let (from, to) = (self.one(*from)?, self.one(*to)?);
                let na = self.either_na(from, to);
                self.fail_if(na, &inst.fail_drops)?;
                let (from, to) = (self.wide(from), self.wide(to));
                let list = self.call_words(Helper::ListSeq, I64, &[from, to]);
                SmallVec::from_slice(&[list])
            }
            Op::RangeI64(from, to) => {
                let (from, to) = (self.one(*from)?, self.one(*to)?);
                let list = self.call_words(Helper::ListRangeI64, I64, &[from, to]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Repeat(value, times) => {
                let item = ty(*value)
                    .held_item()
                    .ok_or(Refusal::new("a `rep` of a non-scalar"))?;
                let times = self.one(*times)?;
                let negative = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, times, 0);
                self.fail_if(negative, &inst.fail_drops)?;
                let (w0, w1) = self.item_words(*value)?;
                let kind = self.iconst(kind(item));
                let times = self.wide(times);
                let list = self.call_words(Helper::ListRep, I64, &[kind, w0, w1, times]);
                SmallVec::from_slice(&[list])
            }
            Op::VectorArith(op, item, a, b) => {
                let lists = inst
                    .dest
                    .is_some_and(|dest| matches!(ty(dest), Type::Vector { array: true, .. }));
                let flag = if lists { values::LIST_ARITH } else { 0 };
                let operation = self.iconst(i64::from(*op as u32 | flag));
                let kind = self.iconst(kind(*item));
                let list = match (ty(*a).items().is_some(), ty(*b).items().is_some()) {
                    (true, true) => {
                        let (x, y) = (self.one(*a)?, self.one(*b)?);
                        let (ox, oy) = (self.owned(inst, 0), self.owned(inst, 1));
                        self.call_words(Helper::Arith, I64, &[operation, kind, x, ox, y, oy])
                    }
                    (true, false) => {
                        let x = self.one(*a)?;
                        let ox = self.owned(inst, 0);
                        let (y, _) = self.item_words(*b)?;
                        self.call_words(Helper::ArithVs, I64, &[operation, kind, x, ox, y])
                    }
                    _ => {
                        let (x, _) = self.item_words(*a)?;
                        let y = self.one(*b)?;
                        let oy = self.owned(inst, 1);
                        self.call_words(Helper::ArithSv, I64, &[operation, kind, x, y, oy])
                    }
                };
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Elementwise {
                item,
                sum,
                program,
                inputs,
            } => {
                let words = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8 * inputs.len().max(1) as u32,
                    3,
                ));
                let mut lists = 0i64;
                for (index, &input) in inputs.iter().enumerate() {
                    let word = if ty(input).items().is_some() {
                        lists |= 1 << index;
                        self.one(input)?
                    } else {
                        self.item_words(input)?.0
                    };
                    self.builder
                        .ins()
                        .stack_store(I64, word, words, 8 * index as i32);
                }
                let steps = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    4 * program.len() as u32,
                    2,
                ));
                for (index, &step) in program.iter().enumerate() {
                    let step = self.builder.ins().iconst(I32, i64::from(step));
                    self.builder
                        .ins()
                        .stack_store(I64, step, steps, 4 * index as i32);
                }
                let out = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let arguments = [
                    self.iconst(kind(*item)),
                    self.builder.ins().stack_addr(I64, steps, 0),
                    self.iconst(program.len() as i64),
                    self.builder.ins().stack_addr(I64, words, 0),
                    self.iconst(inputs.len() as i64),
                    self.iconst(lists),
                    self.iconst(i64::from(*sum)),
                    self.builder.ins().stack_addr(I64, out, 0),
                ];
                let status = self.call_words(Helper::Elementwise, I32, &arguments);
                self.fail_if(status, &inst.fail_drops)?;
                let result = match (*sum, item) {
                    (true, Item::Dbl) => self.builder.ins().stack_load(I64, F64, out, 0),
                    _ => self.builder.ins().stack_load(I64, I64, out, 0),
                };
                SmallVec::from_slice(&[result])
            }
            Op::CountTrue {
                program,
                inputs,
                counts,
            } => {
                let words = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8 * inputs.len().max(1) as u32,
                    3,
                ));
                let mut lists = 0i64;
                for (index, &input) in inputs.iter().enumerate() {
                    let word = if ty(input).items().is_some() {
                        lists |= 1 << index;
                        self.one(input)?
                    } else {
                        self.item_words(input)?.0
                    };
                    self.builder
                        .ins()
                        .stack_store(I64, word, words, 8 * index as i32);
                }
                // The steps and, after them, the operand numbers of the counted lists.
                let codes = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    4 * (program.len() + counts.len()).max(1) as u32,
                    2,
                ));
                let counted = counts.iter().map(|&(operand, _)| operand);
                for (index, code) in program.iter().copied().chain(counted).enumerate() {
                    let code = self.builder.ins().iconst(I32, i64::from(code));
                    self.builder
                        .ins()
                        .stack_store(I64, code, codes, 4 * index as i32);
                }
                let out = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8 * counts.len().max(1) as u32,
                    3,
                ));
                let arguments = [
                    self.builder.ins().stack_addr(I64, codes, 0),
                    self.iconst(program.len() as i64),
                    self.builder.ins().stack_addr(I64, words, 0),
                    self.iconst(inputs.len() as i64),
                    self.iconst(lists),
                    self.builder
                        .ins()
                        .stack_addr(I64, codes, 4 * program.len() as i32),
                    self.iconst(counts.len() as i64),
                    self.builder.ins().stack_addr(I64, out, 0),
                ];
                let status = self.call_words(Helper::CountTrue, I32, &arguments);
                self.fail_if(status, &inst.fail_drops)?;
                for (index, &(_, local)) in counts.iter().enumerate() {
                    let count = self
                        .builder
                        .ins()
                        .stack_load(I64, I64, out, 8 * index as i32);
                    self.set(local, &[count])?;
                }
                SmallVec::new()
            }
            Op::VectorCompare(op, item, a, b) => {
                let operation = self.iconst(*op as i64);
                let kind = self.iconst(kind(*item));
                let list = match (ty(*a).items().is_some(), ty(*b).items().is_some()) {
                    (true, true) => {
                        let (x, y) = (self.one(*a)?, self.one(*b)?);
                        let (ox, oy) = (self.owned(inst, 0), self.owned(inst, 1));
                        self.call_words(Helper::Compare, I64, &[operation, kind, x, ox, y, oy])
                    }
                    (true, false) => {
                        let x = self.one(*a)?;
                        let ox = self.owned(inst, 0);
                        let (y0, y1) = self.item_words(*b)?;
                        self.call_words(Helper::CompareVs, I64, &[operation, kind, x, ox, y0, y1])
                    }
                    _ => {
                        let (x0, x1) = self.item_words(*a)?;
                        let y = self.one(*b)?;
                        let oy = self.owned(inst, 1);
                        self.call_words(Helper::CompareSv, I64, &[operation, kind, x0, x1, y, oy])
                    }
                };
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::VectorLogic(and, a, b) => {
                let operation = self.iconst(if *and { 0 } else { 1 });
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                let (ox, oy) = (self.owned(inst, 0), self.owned(inst, 1));
                let list = self.call_words(Helper::Logic, I64, &[operation, x, ox, y, oy]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::VectorNot(a) => {
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let list = self.call_words(Helper::Not, I64, &[x, owned]);
                SmallVec::from_slice(&[list])
            }
            Op::VectorMath(op, _, a) => {
                let operation = self.iconst(*op as i64);
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let list = self.call_words(Helper::Math, I64, &[operation, x, owned]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::VectorRound(a, digits) => {
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let digits = self.one(*digits)?;
                let digits = self.builder.ins().fcvt_to_sint_sat(I32, digits);
                let list =
                    self.call(Helper::Round, &[I64, I64, I32], &[I64], &[x, owned, digits])[0];
                SmallVec::from_slice(&[list])
            }
            Op::Select(a, mask) => {
                let (x, flags) = (self.one(*a)?, self.one(*mask)?);
                let list = self.call_words(Helper::ListSelect, I64, &[x, flags]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Sort(a, descending) => {
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let descending = self.iconst(i64::from(*descending));
                let list = self.call_words(Helper::ListSort, I64, &[x, owned, descending]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Coerce(a, item) => {
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let kind = self.iconst(kind(*item));
                let list = self.call_words(Helper::ListCoerce, I64, &[x, owned, kind]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Reduce(reduce, item, a) => {
                let x = self.one(*a)?;
                SmallVec::from_slice(&[self.reduce(*reduce, *item, x, inst)?])
            }
            Op::Concat(parts, separator) => {
                let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    16 * parts.len() as u32,
                    3,
                ));
                for (index, &part) in parts.iter().enumerate() {
                    let values = self.get(part)?;
                    for (word, &value) in values.iter().enumerate() {
                        self.builder.ins().stack_store(
                            I64,
                            value,
                            slot,
                            (16 * index + 8 * word) as i32,
                        );
                    }
                }
                let base = self.builder.ins().stack_addr(I64, slot, 0);
                let count = self.iconst(parts.len() as i64);
                let separator = self.get(*separator)?;
                let text = self.call(
                    Helper::StrConcat,
                    &[I64; 4],
                    &[I64, I64],
                    &[base, count, separator[0], separator[1]],
                );
                SmallVec::from_slice(&text)
            }
            Op::PasteVectors(parts, separator) => {
                let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8 * parts.len() as u32,
                    3,
                ));
                for (index, &part) in parts.iter().enumerate() {
                    let pointer = self.one(part)?;
                    self.builder
                        .ins()
                        .stack_store(I64, pointer, slot, 8 * index as i32);
                }
                let base = self.builder.ins().stack_addr(I64, slot, 0);
                let count = self.iconst(parts.len() as i64);
                let separator = self.get(*separator)?;
                let list = self.call_words(
                    Helper::ListPaste,
                    I64,
                    &[base, count, separator[0], separator[1]],
                );
                SmallVec::from_slice(&[list])
            }
            Op::Join(a, separator) => {
                let x = self.one(*a)?;
                let separator = self.get(*separator)?;
                let text = self.call(
                    Helper::ListJoin,
                    &[I64; 3],
                    &[I64, I64],
                    &[x, separator[0], separator[1]],
                );
                SmallVec::from_slice(&text)
            }
            Op::Text(text, a) => {
                let x = self.get(*a)?;
                match text {
                    Text::Nchar => SmallVec::from_slice(&self.call(
                        Helper::StrNchar,
                        &[I64; 2],
                        &[I32],
                        &[x[0], x[1]],
                    )),
                    Text::Upper | Text::Lower => {
                        let owned = self.owned(inst, 0);
                        let function = if *text == Text::Upper {
                            Helper::StrToupper
                        } else {
                            Helper::StrTolower
                        };
                        SmallVec::from_slice(&self.call(
                            function,
                            &[I64; 3],
                            &[I64, I64],
                            &[x[0], x[1], owned],
                        ))
                    }
                    Text::AsciiUpper | Text::AsciiLower => {
                        let owned = self.owned(inst, 0);
                        let upper = self.iconst(i64::from(*text == Text::AsciiUpper));
                        SmallVec::from_slice(&self.call(
                            Helper::StrAsciiCase,
                            &[I64; 4],
                            &[I64, I64],
                            &[x[0], x[1], owned, upper],
                        ))
                    }
                    Text::Trim | Text::TrimStart | Text::TrimEnd => {
                        let owned = self.owned(inst, 0);
                        let ends = self.iconst(match text {
                            Text::TrimStart => 1,
                            Text::TrimEnd => 2,
                            _ => 3,
                        });
                        SmallVec::from_slice(&self.call(
                            Helper::StrTrim,
                            &[I64; 4],
                            &[I64, I64],
                            &[x[0], x[1], owned, ends],
                        ))
                    }
                }
            }
            Op::VectorText(text, a) => {
                let x = self.one(*a)?;
                let list = match text {
                    Text::Nchar => self.call_words(Helper::ListNchar, I64, &[x]),
                    Text::Upper | Text::Lower => {
                        let owned = self.owned(inst, 0);
                        let function = if *text == Text::Upper {
                            Helper::ListToupper
                        } else {
                            Helper::ListTolower
                        };
                        self.call_words(function, I64, &[x, owned])
                    }
                    Text::AsciiUpper
                    | Text::AsciiLower
                    | Text::Trim
                    | Text::TrimStart
                    | Text::TrimEnd => {
                        return Err(Refusal::new("a `Str` function of a vector"));
                    }
                };
                SmallVec::from_slice(&[list])
            }
            Op::StrTest(test, a, other) => {
                let (x, other) = (self.get(*a)?, self.get(*other)?);
                let function = match test {
                    StrTest::StartsWith => Helper::StrStartsWith,
                    StrTest::EndsWith => Helper::StrEndsWith,
                    StrTest::Contains => Helper::StrContains,
                    StrTest::CaselessAsciiEquals => Helper::StrCaselessAsciiEquals,
                };
                SmallVec::from_slice(&self.call(
                    function,
                    &[I64; 4],
                    &[I32],
                    &[x[0], x[1], other[0], other[1]],
                ))
            }
            Op::DropAffix(prefix, a, affix) => {
                let (x, affix) = (self.get(*a)?, self.get(*affix)?);
                let owned = self.owned(inst, 0);
                let prefix = self.iconst(i64::from(*prefix));
                SmallVec::from_slice(&self.call(
                    Helper::StrDropAffix,
                    &[I64; 6],
                    &[I64, I64],
                    &[x[0], x[1], owned, affix[0], affix[1], prefix],
                ))
            }
            Op::Find(last, a, pattern) => {
                let (x, pattern) = (self.get(*a)?, self.get(*pattern)?);
                let last = self.iconst(i64::from(*last));
                SmallVec::from_slice(&self.call(
                    Helper::StrFind,
                    &[I64; 5],
                    &[I64],
                    &[x[0], x[1], pattern[0], pattern[1], last],
                ))
            }
            Op::Substring(a, start, end) => {
                let x = self.get(*a)?;
                let (start, end) = (self.one(*start)?, self.one(*end)?);
                SmallVec::from_slice(&self.call(
                    Helper::StrPiece,
                    &[I64; 4],
                    &[I64, I64],
                    &[x[0], x[1], start, end],
                ))
            }
            Op::VectorAffix(starts, a, affix) => {
                let x = self.one(*a)?;
                let affix = self.get(*affix)?;
                let function = if *starts {
                    Helper::ListStartsWith
                } else {
                    Helper::ListEndsWith
                };
                let list = self.call_words(function, I64, &[x, affix[0], affix[1]]);
                SmallVec::from_slice(&[list])
            }
            Op::Format(a) => {
                let x = self.one(*a)?;
                self.fail_if_na(x, ty(*a), &inst.fail_drops)?;
                let text = match ty(*a) {
                    Type::Dbl => self.call(Helper::FormatDbl, &[F64], &[I64, I64], &[x]),
                    Type::I64 => self.call(Helper::FormatI64, &[I64], &[I64, I64], &[x]),
                    _ => {
                        let x = self.wide(x);
                        self.call(Helper::FormatInt, &[I64], &[I64, I64], &[x])
                    }
                };
                SmallVec::from_slice(&text)
            }
            Op::FormatFixed(a, decimals) => {
                let x = self.one(*a)?;
                self.fail_if_na(x, ty(*a), &inst.fail_drops)?;
                let decimals = self.iconst(i64::from(*decimals));
                SmallVec::from_slice(&self.call(
                    Helper::FormatFixed,
                    &[F64, I64],
                    &[I64, I64],
                    &[x, decimals],
                ))
            }
            Op::FormatPadded(a, width, zero) => {
                let x = self.one(*a)?;
                self.fail_if_na(x, ty(*a), &inst.fail_drops)?;
                let x = self.wide(x);
                let width = self.iconst(i64::from(*width));
                let zero = self.iconst(i64::from(*zero));
                SmallVec::from_slice(&self.call(
                    Helper::FormatPaddedInt,
                    &[I64; 3],
                    &[I64, I64],
                    &[x, width, zero],
                ))
            }
            Op::CallTyped(index, arguments) => self.call_typed(*index, arguments, inst)?,
            Op::CallR(index, arguments) => self.call_r(*index, arguments, inst)?,
            Op::Construct(shape, fields) => self.construct(*shape, fields, inst)?,
            Op::Field(a, shape, index) | Op::Take(a, shape, index) => {
                let pointer = self.one(*a)?;
                let shape = shapes::shape(*shape);
                let (Some(&item), Some(&offset)) = (
                    shape.fields.get(*index as usize),
                    shape.offsets.get(*index as usize),
                ) else {
                    return Err(Refusal::new("a field the shape does not have"));
                };
                let address = self
                    .builder
                    .ins()
                    .iadd_imm_s(pointer, (values::BOX_FIELDS + 8 * offset as usize) as i64);
                let values = self.load_item(address, item);
                if matches!(inst.op, Op::Take(..)) {
                    self.take(pointer, address, Type::of_item(item), &values);
                } else {
                    self.dup(Type::of_item(item), &values);
                }
                values
            }
            Op::ShapeOf(a) => {
                let pointer = self.one(*a)?;
                SmallVec::from_slice(&[self.builder.ins().load(
                    I32,
                    MemFlagsData::trusted(),
                    pointer,
                    values::BOX_SHAPE as i32,
                )])
            }
            Op::ByteCount(a) => {
                let x = self.get(*a)?;
                // An inline string keeps its length in the top byte of `hi`, under the flag
                // that makes `hi` negative.
                let inline = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, x[1], 0);
                let shifted = self.builder.ins().ushr_imm_u(x[1], 56);
                let short = self.builder.ins().band_imm_u(shifted, 0x7F);
                SmallVec::from_slice(&[self.builder.ins().select(inline, short, x[1])])
            }
            Op::DictLen(dict) => {
                let pointer = self.one(*dict)?;
                let length = self.call_words(Helper::DictLen, I64, &[pointer]);
                SmallVec::from_slice(&[length])
            }
            Op::DictKeys(dict) | Op::DictValues(dict) => {
                let pointer = self.one(*dict)?;
                let function = if matches!(inst.op, Op::DictKeys(_)) {
                    Helper::DictKeys
                } else {
                    Helper::DictValues
                };
                let list = self.call_words(function, I64, &[pointer]);
                SmallVec::from_slice(&[list])
            }
            Op::ToUtf8(a) => {
                let x = self.get(*a)?;
                let list = self.call_words(Helper::StrToUtf8, I64, &[x[0], x[1]]);
                SmallVec::from_slice(&[list])
            }
            Op::FromUtf8Lossy(a) => {
                let list = self.one(*a)?;
                let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    16,
                    3,
                ));
                let out = self.builder.ins().stack_addr(I64, slot, 0);
                let decoded =
                    self.call(Helper::ListFromUtf8Lossy, &[I64; 2], &[I32], &[list, out])[0];
                let bad = self.builder.ins().icmp_imm_s(IntCC::Equal, decoded, 0);
                self.fail_if(bad, &inst.fail_drops)?;
                self.load_item(out, Item::Str)
            }
            Op::RepeatStr(a, count) => {
                let x = self.get(*a)?;
                let count = self.one(*count)?;
                let negative = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, count, 0);
                self.fail_if(negative, &inst.fail_drops)?;
                let repeated = self.call(
                    Helper::StrRepeat,
                    &[I64; 3],
                    &[I64, I64],
                    &[x[0], x[1], count],
                );
                let [lo, hi] = [repeated[0], repeated[1]];
                let both = self.builder.ins().bor(lo, hi);
                let too_long = self.builder.ins().icmp_imm_s(IntCC::Equal, both, 0);
                self.fail_if(too_long, &inst.fail_drops)?;
                SmallVec::from_slice(&[lo, hi])
            }
            Op::ReplaceEach(a, pattern, replacement) => {
                let (x, pattern, replacement) =
                    (self.get(*a)?, self.get(*pattern)?, self.get(*replacement)?);
                let replaced = self.call(
                    Helper::StrReplaceEach,
                    &[I64; 6],
                    &[I64, I64],
                    &[
                        x[0],
                        x[1],
                        pattern[0],
                        pattern[1],
                        replacement[0],
                        replacement[1],
                    ],
                );
                let [lo, hi] = [replaced[0], replaced[1]];
                let both = self.builder.ins().bor(lo, hi);
                let too_long = self.builder.ins().icmp_imm_s(IntCC::Equal, both, 0);
                self.fail_if(too_long, &inst.fail_drops)?;
                SmallVec::from_slice(&[lo, hi])
            }
            Op::Split(a, separator) => {
                let (x, separator) = (self.get(*a)?, self.get(*separator)?);
                let list = self.call_words(
                    Helper::StrSplit,
                    I64,
                    &[x[0], x[1], separator[0], separator[1]],
                );
                SmallVec::from_slice(&[list])
            }
            Op::SplitBounds {
                string,
                separator,
                bounds,
            } => {
                let (x, separator) = (self.get(*string)?, self.get(*separator)?);
                let pieces = bounds.iter().map(|&(index, ..)| index).max().unwrap_or(0);
                let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    16 * pieces,
                    3,
                ));
                let out = self.builder.ins().stack_addr(I64, slot, 0);
                let limit = self.iconst(i64::from(pieces));
                let count = self.call(
                    Helper::StrSplitBounds,
                    &[I64; 6],
                    &[I64],
                    &[x[0], x[1], separator[0], separator[1], limit, out],
                )[0];
                // The runtime writes only the pieces the string has. The rest of the slot is
                // read as it is, into locals that an `Op::Piece` checks the count before using.
                for &(index, start, end) in bounds {
                    let offset = 16 * (index as i32 - 1);
                    let value = self.builder.ins().stack_load(I64, I64, slot, offset);
                    self.set(start, &[value])?;
                    let value = self.builder.ins().stack_load(I64, I64, slot, offset + 8);
                    self.set(end, &[value])?;
                }
                SmallVec::from_slice(&[count])
            }
            Op::Piece {
                string,
                start,
                end,
                count,
                index,
            } => {
                let count = self.one(*count)?;
                let missing =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedLessThan, count, i64::from(*index));
                self.fail_if(missing, &inst.fail_drops)?;
                let x = self.get(*string)?;
                let (start, end) = (self.one(*start)?, self.one(*end)?);
                let piece = self.call(
                    Helper::StrPiece,
                    &[I64; 4],
                    &[I64, I64],
                    &[x[0], x[1], start, end],
                );
                SmallVec::from_slice(&[piece[0], piece[1]])
            }
            Op::Parse { string, parsed } => {
                let dest = inst
                    .dest
                    .ok_or(Refusal::new("a parse without a destination"))?;
                let i64 = ty(dest) == Type::I64;
                let x = self.get(*string)?;
                let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                // The runtime writes the number only when the string parses.
                let zero = self.iconst(0);
                self.builder.ins().stack_store(I64, zero, slot, 0);
                let out = self.builder.ins().stack_addr(I64, slot, 0);
                let function = if i64 {
                    Helper::StrToI64
                } else {
                    Helper::StrToDbl
                };
                let status = self.call(function, &[I64; 3], &[I32], &[x[0], x[1], out])[0];
                let valid = self.builder.ins().icmp_imm_s(IntCC::Equal, status, 0);
                let valid = self.logical(valid);
                self.set(*parsed, &[valid])?;
                let machine = if i64 { I64 } else { F64 };
                SmallVec::from_slice(&[self.builder.ins().stack_load(I64, machine, slot, 0)])
            }
            Op::DictNew => {
                let dest = inst
                    .dest
                    .ok_or(Refusal::new("a dictionary without a destination"))?;
                let Some((key, value)) = ty(dest).entry_items() else {
                    return Err(Refusal::new("a new dictionary of another type"));
                };
                let (key, value) = (self.iconst(kind(key)), self.iconst(kind(value)));
                let dict = self.call_words(Helper::DictNew, I64, &[key, value]);
                self.fail_if_null(dict, inst)?;
                SmallVec::from_slice(&[dict])
            }
            Op::DictContains(dict, key) => {
                let dict = self.one(*dict)?;
                let (k0, k1) = self.item_words(*key)?;
                SmallVec::from_slice(&self.call(
                    Helper::DictContains,
                    &[I64; 3],
                    &[I32],
                    &[dict, k0, k1],
                ))
            }
            Op::DictFind(dict, key) => {
                let dict = self.one(*dict)?;
                let (k0, k1) = self.item_words(*key)?;
                SmallVec::from_slice(&self.call(
                    Helper::DictFind,
                    &[I64; 3],
                    &[I64],
                    &[dict, k0, k1],
                ))
            }
            Op::DictAt(dict, position) => {
                let Some((_, value)) = ty(*dict).entry_items() else {
                    return Err(Refusal::new(
                        "a lookup in something other than a dictionary",
                    ));
                };
                let pointer = self.one(*dict)?;
                let position = self.one(*position)?;
                let column = self.builder.ins().load(
                    I64,
                    MemFlagsData::trusted(),
                    pointer,
                    values::DICT_VALUES,
                );
                let scaled = self.builder.ins().imul_imm_s(position, item_size(value));
                let address = self.builder.ins().iadd(column, scaled);
                let values = self.load_item(address, value);
                self.dup(Type::of_item(value), &values);
                values
            }
            Op::DictPut(dict, key, position, value) => {
                let pointer = self.one(*dict)?;
                let owned = self.owned(inst, 0);
                let (k0, k1) = self.item_words(*key)?;
                let position = self.one(*position)?;
                let values = self.get(*value)?;
                if !inst.moves(3) {
                    self.dup(ty(*value), &values);
                }
                let (v0, v1) = self.item_words(*value)?;
                let dict = self.call_words(
                    Helper::DictPut,
                    I64,
                    &[pointer, owned, position, k0, k1, v0, v1],
                );
                self.fail_if_null(dict, inst)?;
                SmallVec::from_slice(&[dict])
            }
            Op::DictInsert(dict, key, value) => {
                let pointer = self.one(*dict)?;
                let owned = self.owned(inst, 0);
                let (k0, k1) = self.item_words(*key)?;
                let values = self.get(*value)?;
                if !inst.moves(2) {
                    self.dup(ty(*value), &values);
                }
                let (v0, v1) = self.item_words(*value)?;
                let dict =
                    self.call_words(Helper::DictInsert, I64, &[pointer, owned, k0, k1, v0, v1]);
                self.fail_if_null(dict, inst)?;
                SmallVec::from_slice(&[dict])
            }
            Op::DictRemove(dict, key) => {
                let pointer = self.one(*dict)?;
                let owned = self.owned(inst, 0);
                let (k0, k1) = self.item_words(*key)?;
                let dict = self.call_words(Helper::DictRemove, I64, &[pointer, owned, k0, k1]);
                SmallVec::from_slice(&[dict])
            }
            Op::Push(list, value) => {
                let pointer = self.one(*list)?;
                let owned = self.owned(inst, 0);
                let values = self.get(*value)?;
                if !inst.moves(1) {
                    self.dup(ty(*value), &values);
                }
                let (w0, w1) = self.item_words(*value)?;
                let list = self.call_words(Helper::ListPush, I64, &[pointer, owned, w0, w1]);
                SmallVec::from_slice(&[list])
            }
            Op::Swap(list, first, second) => {
                let pointer = self.one(*list)?;
                let owned = self.owned(inst, 0);
                let (first, second) = (self.one(*first)?, self.one(*second)?);
                let list = self.call_words(Helper::ListSwap, I64, &[pointer, owned, first, second]);
                SmallVec::from_slice(&[list])
            }
            Op::Extend(first, second) => {
                let (a, b) = (self.one(*first)?, self.one(*second)?);
                let (a_owned, b_owned) = (self.owned(inst, 0), self.owned(inst, 1));
                let list = self.call_words(Helper::ListConcat, I64, &[a, a_owned, b, b_owned]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Slice(list, start, count) => {
                let pointer = self.one(*list)?;
                let owned = self.owned(inst, 0);
                let (start, count) = (self.one(*start)?, self.one(*count)?);
                let list = self.call_words(Helper::ListSlice, I64, &[pointer, owned, start, count]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Poll => {
                let address = self.word(Slot::Interrupt, || {
                    &interp::unwind::INTERRUPT_REQUESTED as *const AtomicBool as i64
                });
                let requested = self
                    .builder
                    .ins()
                    .uload8(I32, MemFlagsData::trusted(), address, 0);
                self.fail_if(requested, &inst.fail_drops)?;
                SmallVec::new()
            }
            Op::Dup(a) => {
                let values = self.get(*a)?;
                self.dup(ty(*a), &values);
                SmallVec::new()
            }
            Op::Drop(a) => {
                let values = self.get(*a)?;
                self.drop(ty(*a), &values);
                SmallVec::new()
            }
        };
        if let Some(dest) = inst.dest {
            self.set(dest, &result)?;
        }
        Ok(())
    }

    fn either_na(&mut self, x: Value, y: Value) -> Value {
        let (a, b) = (self.is_na_integer(x), self.is_na_integer(y));
        self.builder.ins().bor(a, b)
    }

    fn real_arith(&mut self, op: Arith, x: Value, y: Value) -> Result<Value, Refusal> {
        let helper = match op {
            Arith::Add => return Ok(self.builder.ins().fadd(x, y)),
            Arith::Sub => return Ok(self.builder.ins().fsub(x, y)),
            Arith::Mul => return Ok(self.builder.ins().fmul(x, y)),
            Arith::Div => return Ok(self.builder.ins().fdiv(x, y)),
            Arith::Pow => Helper::RealPower,
            Arith::NumPow => Helper::RealPlatformPower,
            Arith::Mod => Helper::RealModulo,
            Arith::IntDiv => Helper::RealIntegerDivide,
            Arith::Min => Helper::RealMin,
            Arith::Max => Helper::RealMax,
            Arith::DivTrunc | Arith::RemTrunc => {
                return Err(Refusal::new("a truncating division of doubles"));
            }
        };
        Ok(self.call(helper, &[F64, F64], &[F64], &[x, y])[0])
    }

    /// Integer arithmetic as R does it: `NA` in gives `NA` out, and a result outside the
    /// integers gives up, so the interpreter warns as R does.
    fn integer_arith(
        &mut self,
        op: Arith,
        x: Value,
        y: Value,
        inst: &Inst,
    ) -> Result<Value, Refusal> {
        let na = self.either_na(x, y);
        let value = match op {
            Arith::Add | Arith::Sub | Arith::Mul => {
                let (wide_x, wide_y) = (self.wide(x), self.wide(y));
                let wide = match op {
                    Arith::Add => self.builder.ins().iadd(wide_x, wide_y),
                    Arith::Sub => self.builder.ins().isub(wide_x, wide_y),
                    _ => self.builder.ins().imul(wide_x, wide_y),
                };
                let low =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedLessThanOrEqual, wide, NA_INTEGER);
                let high = self.builder.ins().icmp_imm_s(
                    IntCC::SignedGreaterThan,
                    wide,
                    i64::from(i32::MAX),
                );
                let outside = self.builder.ins().bor(low, high);
                let present = self.builder.ins().icmp_imm_s(IntCC::Equal, na, 0);
                let overflow = self.builder.ins().band(outside, present);
                self.fail_if(overflow, &inst.fail_drops)?;
                self.builder.ins().ireduce(I32, wide)
            }
            Arith::Mod | Arith::IntDiv => {
                // `x %% 0L` and `x %/% 0L` are `NA`; the divisor is made safe for the machine
                // division, whose result is then discarded.
                let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, y, 0);
                let na = self.builder.ins().bor(na, zero);
                let one = self.builder.ins().iconst(I32, 1);
                let safe_y = self.builder.ins().select(na, one, y);
                let nothing = self.builder.ins().iconst(I32, 0);
                let safe_x = self.builder.ins().select(na, nothing, x);
                let remainder = self.builder.ins().srem(safe_x, safe_y);
                // A remainder with the divisor's opposite sign moves to the divisor's side.
                let nonzero = self.builder.ins().icmp_imm_s(IntCC::NotEqual, remainder, 0);
                let signs = self.builder.ins().bxor(remainder, safe_y);
                let opposite = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, signs, 0);
                let adjust = self.builder.ins().band(nonzero, opposite);
                let value = if op == Arith::Mod {
                    let moved = self.builder.ins().iadd(remainder, safe_y);
                    self.builder.ins().select(adjust, moved, remainder)
                } else {
                    let quotient = self.builder.ins().sdiv(safe_x, safe_y);
                    let floored = self.builder.ins().iadd_imm_s(quotient, -1);
                    self.builder.ins().select(adjust, floored, quotient)
                };
                return Ok(self.na_or(na, value));
            }
            Arith::Min => self.builder.ins().smin(x, y),
            Arith::Max => self.builder.ins().smax(x, y),
            Arith::Div | Arith::Pow | Arith::NumPow | Arith::DivTrunc | Arith::RemTrunc => {
                return Err(Refusal::new("an integer division or power"));
            }
        };
        Ok(self.na_or(na, value))
    }

    /// `I64` arithmetic as the standard library does it: an overflow, a division by zero and a
    /// negative power give up, so the interpreter raises the error. The operands lie in the
    /// two ranges, which decide the checks an operator needs.
    fn i64_arith(
        &mut self,
        op: Arith,
        x: Value,
        y: Value,
        (x_range, y_range): (Range, Range),
        inst: &Inst,
    ) -> Result<Value, Refusal> {
        Ok(match op {
            Arith::Add | Arith::Sub | Arith::Mul => {
                let fits = exact_bounds(op, x_range, y_range).is_some_and(|(lo, hi)| {
                    lo >= i128::from(i64::MIN) && hi <= i128::from(i64::MAX)
                });
                if fits {
                    return Ok(match op {
                        Arith::Add => self.builder.ins().iadd(x, y),
                        Arith::Sub => self.builder.ins().isub(x, y),
                        _ => self.builder.ins().imul(x, y),
                    });
                }
                let (value, overflow) = match op {
                    Arith::Add => self.builder.ins().sadd_overflow(x, y),
                    Arith::Sub => self.builder.ins().ssub_overflow(x, y),
                    _ => self.builder.ins().smul_overflow(x, y),
                };
                self.fail_if(overflow, &inst.fail_drops)?;
                value
            }
            Arith::Mod | Arith::IntDiv => {
                // A positive divisor is neither zero nor -1. A power of two needs no division:
                // the low bits are the floored remainder of a negative number too, and an
                // arithmetic shift rounds down.
                if let Some(divisor) = self.builder.constant(y).filter(|&divisor| divisor > 0)
                    && divisor & (divisor - 1) == 0
                {
                    return Ok(if op == Arith::Mod {
                        self.builder.ins().band_imm_u(x, divisor - 1)
                    } else {
                        let bits = self.iconst(i64::from(divisor.trailing_zeros()));
                        self.builder.ins().sshr(x, bits)
                    });
                }
                if y_range.lo > 0 {
                    // The machine's division rounds toward zero, which is the floor unless
                    // the dividend is negative. Then the remainder is negative or zero, which
                    // its sign bit says, and a negative one moves up by the divisor while the
                    // quotient moves down by one.
                    if x_range.lo >= 0 {
                        return Ok(if op == Arith::Mod {
                            self.builder.ins().srem(x, y)
                        } else {
                            self.builder.ins().sdiv(x, y)
                        });
                    }
                    let remainder = self.builder.ins().srem(x, y);
                    let sign = self.iconst(63);
                    let negative = self.builder.ins().sshr(remainder, sign);
                    return Ok(if op == Arith::Mod {
                        let moved = self.builder.ins().band(negative, y);
                        self.builder.ins().iadd(remainder, moved)
                    } else {
                        let quotient = self.builder.ins().sdiv(x, y);
                        self.builder.ins().iadd(quotient, negative)
                    });
                }
                let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, y, 0);
                self.fail_if(zero, &inst.fail_drops)?;
                let minus_one = self.builder.ins().icmp_imm_s(IntCC::Equal, y, -1);
                if op == Arith::IntDiv {
                    let lowest = self.builder.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                    let overflow = self.builder.ins().band(lowest, minus_one);
                    self.fail_if(overflow, &inst.fail_drops)?;
                }
                // Every remainder by -1 is 0, and dividing the most negative value by -1 would
                // trap, so the remainder divides by 1 instead.
                let one = self.iconst(1);
                let safe_y = self.builder.ins().select(minus_one, one, y);
                let remainder = self.builder.ins().srem(x, safe_y);
                let nonzero = self.builder.ins().icmp_imm_s(IntCC::NotEqual, remainder, 0);
                let signs = self.builder.ins().bxor(remainder, y);
                let opposite = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, signs, 0);
                let floored = self.builder.ins().band(nonzero, opposite);
                if op == Arith::Mod {
                    let moved = self.builder.ins().iadd(remainder, y);
                    self.builder.ins().select(floored, moved, remainder)
                } else {
                    let quotient = self.builder.ins().sdiv(x, y);
                    let lower = self.builder.ins().iadd_imm_s(quotient, -1);
                    self.builder.ins().select(floored, lower, quotient)
                }
            }
            Arith::Pow => {
                let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let out = self.builder.ins().stack_addr(I64, slot, 0);
                let status = self.call(Helper::PowerI64, &[I64, I64, I64], &[I32], &[x, y, out])[0];
                self.fail_if(status, &inst.fail_drops)?;
                self.builder.ins().stack_load(I64, I64, slot, 0)
            }
            Arith::Min => self.builder.ins().smin(x, y),
            Arith::Max => self.builder.ins().smax(x, y),
            Arith::DivTrunc | Arith::RemTrunc => {
                if y_range.lo <= 0 && y_range.hi >= 0 {
                    let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, y, 0);
                    self.fail_if(zero, &inst.fail_drops)?;
                }
                if y_range.lo <= -1 && y_range.hi >= -1 && x_range.lo == i64::MIN {
                    let minus_one = self.builder.ins().icmp_imm_s(IntCC::Equal, y, -1);
                    if op == Arith::DivTrunc {
                        let lowest = self.builder.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                        let overflow = self.builder.ins().band(lowest, minus_one);
                        self.fail_if(overflow, &inst.fail_drops)?;
                    } else {
                        // Every remainder by -1 is 0, and the machine traps on the most
                        // negative value by -1, so the remainder divides by 1 instead.
                        let one = self.iconst(1);
                        let safe_y = self.builder.ins().select(minus_one, one, y);
                        return Ok(self.builder.ins().srem(x, safe_y));
                    }
                }
                if op == Arith::DivTrunc {
                    self.builder.ins().sdiv(x, y)
                } else {
                    self.builder.ins().srem(x, y)
                }
            }
            Arith::Div => return Err(Refusal::new("a `/` of I64s")),
            Arith::NumPow => return Err(Refusal::new("a `Num.pow` of I64s")),
        })
    }

    /// A math function of a double. One that turns a number into `NaN` gives up, so the
    /// interpreter warns as R does.
    fn real_math(&mut self, op: Math, x: Value, inst: &Inst) -> Result<Value, Refusal> {
        let helper = match op {
            Math::Neg => return Ok(self.builder.ins().fneg(x)),
            Math::Abs => return Ok(self.builder.ins().fabs(x)),
            Math::Floor => return Ok(self.builder.ins().floor(x)),
            Math::Ceiling => return Ok(self.builder.ins().ceil(x)),
            Math::Trunc => return Ok(self.builder.ins().trunc(x)),
            Math::Sqrt => None,
            Math::Exp => Some(Helper::RealExp),
            Math::Log => Some(Helper::RealLog),
            Math::Log2 => Some(Helper::RealLog2),
            Math::Log10 => Some(Helper::RealLog10),
            Math::Sin => Some(Helper::RealSin),
            Math::Cos => Some(Helper::RealCos),
            Math::Tan => Some(Helper::RealTan),
            Math::Round => Some(Helper::RealRound),
            Math::Sign => Some(Helper::RealSign),
            Math::RoundAway => Some(Helper::RealRoundAway),
            Math::Atan => Some(Helper::RealAtan),
            Math::Asin => Some(Helper::RealAsin),
            Math::Acos => Some(Helper::RealAcos),
        };
        let value = match helper {
            None => self.builder.ins().sqrt(x),
            Some(helper) => self.call(helper, &[F64], &[F64], &[x])[0],
        };
        if matches!(op, Math::Exp | Math::Round | Math::Sign | Math::RoundAway) {
            return Ok(value);
        }
        let produced = self.builder.ins().fcmp(FloatCC::Unordered, value, value);
        let number = self.builder.ins().fcmp(FloatCC::Ordered, x, x);
        let warns = self.builder.ins().band(produced, number);
        self.fail_if(warns, &inst.fail_drops)?;
        Ok(value)
    }

    fn reduce(
        &mut self,
        reduce: Reduce,
        item: Item,
        x: Value,
        inst: &Inst,
    ) -> Result<Value, Refusal> {
        Ok(match (reduce, item) {
            (Reduce::Sum, Item::Dbl) => self.call(Helper::SumDbl, &[I64], &[F64], &[x])[0],
            (Reduce::ListSum, _) => self.call(Helper::ListSumDbl, &[I64], &[F64], &[x])[0],
            (Reduce::Sum, Item::I64) => self.reduce_status(Helper::SumI64, I64, x, inst)?,
            (_, Item::I64) => return Err(Refusal::new("an I64 reduction other than a sum")),
            (Reduce::Prod, _) => self.call(Helper::ProdDbl, &[I64], &[F64], &[x])[0],
            (Reduce::Mean, _) => self.call(Helper::MeanDbl, &[I64], &[F64], &[x])[0],
            (Reduce::Sum, _) => self.reduce_status(Helper::SumInt, I32, x, inst)?,
            (Reduce::Max, Item::Dbl) => self.reduce_status(Helper::MaxDbl, F64, x, inst)?,
            (Reduce::Min, Item::Dbl) => self.reduce_status(Helper::MinDbl, F64, x, inst)?,
            (Reduce::Max, _) => self.reduce_status(Helper::MaxInt, I32, x, inst)?,
            (Reduce::Min, _) => self.reduce_status(Helper::MinInt, I32, x, inst)?,
            (Reduce::Any, _) => self.reduce_status(Helper::Any, I32, x, inst)?,
            (Reduce::All, _) => self.reduce_status(Helper::All, I32, x, inst)?,
        })
    }

    /// A reduction that writes its value through a pointer and returns a status. Status 1 means
    /// the value is `NA` and stands, and status 2 (an empty list, or an overflow) gives up.
    fn reduce_status(
        &mut self,
        helper: Helper,
        result: Machine,
        x: Value,
        inst: &Inst,
    ) -> Result<Value, Refusal> {
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            8,
            3,
        ));
        let out = self.builder.ins().stack_addr(I64, slot, 0);
        let status = self.call(helper, &[I64, I64], &[I32], &[x, out])[0];
        let fails = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, status, 2);
        self.fail_if(fails, &inst.fail_drops)?;
        Ok(self.builder.ins().stack_load(I64, result, slot, 0))
    }

    /// The address of item `index` (1-based) of the list at `pointer`, giving up out of range.
    fn item_address(
        &mut self,
        pointer: Value,
        index: Value,
        item: Item,
        inst: &Inst,
    ) -> Result<Value, Refusal> {
        let length = self
            .builder
            .ins()
            .load(I64, MemFlagsData::trusted(), pointer, LENGTH_OFFSET);
        let wide = self.wide(index);
        let offset = self.builder.ins().iadd_imm_s(wide, -1);
        // An index below 1, `NA` included, wraps to a huge unsigned offset.
        let outside = self
            .builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, offset, length);
        self.fail_if(outside, &inst.fail_drops)?;
        Ok(self.item_address_unchecked(pointer, index, item))
    }

    fn item_address_unchecked(&mut self, pointer: Value, index: Value, item: Item) -> Value {
        let data = self
            .builder
            .ins()
            .load(I64, MemFlagsData::trusted(), pointer, DATA_OFFSET);
        let wide = self.wide(index);
        let offset = self.builder.ins().iadd_imm_s(wide, -1);
        let scaled = self.builder.ins().imul_imm_s(offset, item_size(item));
        self.builder.ins().iadd(data, scaled)
    }

    fn load_item(&mut self, address: Value, item: Item) -> SmallVec<[Value; 2]> {
        let flags = MemFlagsData::trusted();
        match item {
            Item::Int | Item::Lgl => {
                SmallVec::from_slice(&[self.builder.ins().load(I32, flags, address, 0)])
            }
            Item::Dbl => SmallVec::from_slice(&[self.builder.ins().load(F64, flags, address, 0)]),
            Item::I64 => SmallVec::from_slice(&[self.builder.ins().load(I64, flags, address, 0)]),
            Item::Str => SmallVec::from_slice(&[
                self.builder.ins().load(I64, flags, address, 0),
                self.builder.ins().load(I64, flags, address, 8),
            ]),
            Item::Record(_) | Item::Tag(_) | Item::Array(_) => {
                SmallVec::from_slice(&[self.builder.ins().load(I64, flags, address, 0)])
            }
        }
    }

    fn store_item(&mut self, address: Value, item: Item, values: &[Value]) {
        let flags = MemFlagsData::trusted();
        self.builder.ins().store(flags, values[0], address, 0);
        if item == Item::Str {
            self.builder.ins().store(flags, values[1], address, 8);
        }
    }

    /// The list at `pointer` with a count of one, which may be written in place: itself when it
    /// is owned and unshared, else a copy from the runtime.
    fn unique(&mut self, pointer: Value, owned: bool) -> Value {
        let owned_flag = self.iconst(i64::from(owned));
        if !owned {
            return self.call_words(Helper::ListUnshare, I64, &[pointer, owned_flag]);
        }
        let count = self
            .builder
            .ins()
            .load(I32, MemFlagsData::trusted(), pointer, 0);
        let alone = self.builder.ins().icmp_imm_s(IntCC::Equal, count, 1);
        let (copy, done) = (self.builder.create_block(), self.builder.create_block());
        self.builder.append_block_param(done, I64);
        self.builder.ins().brif(alone, done, &[pointer], copy, &[]);
        self.builder.switch_to_block(copy);
        let copied = self.call_words(Helper::ListUnshare, I64, &[pointer, owned_flag]);
        self.builder.ins().jump(done, &[copied]);
        self.builder.switch_to_block(done);
        self.builder.block_params(done)[0]
    }

    /// Gives the caller the field at `address` of the box at `pointer`: moved out of a box with a
    /// count of one, whose slot is left empty for its drop to skip, else counted again.
    fn take(&mut self, pointer: Value, address: Value, ty: Type, values: &[Value]) {
        let count = self
            .builder
            .ins()
            .load(I32, MemFlagsData::trusted(), pointer, 0);
        let alone = self.builder.ins().icmp_imm_s(IntCC::Equal, count, 1);
        let (take, share, done) = (
            self.builder.create_block(),
            self.builder.create_block(),
            self.builder.create_block(),
        );
        self.builder.ins().brif(alone, take, &[], share, &[]);
        self.builder.switch_to_block(take);
        let empty = self.iconst(0);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), empty, address, 0);
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(share);
        self.dup(ty, values);
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
    }

    /// A new box of `shape` holding the fields, which it takes over: a field that does not die
    /// here is counted again.
    fn construct(
        &mut self,
        shape: u32,
        fields: &[Local],
        inst: &Inst,
    ) -> Result<SmallVec<[Value; 2]>, Refusal> {
        let layout = shapes::shape(shape);
        if layout.words == 0 {
            return Ok(SmallVec::from_slice(&[
                self.word(Slot::Constant(shape), || shapes::constant(shape) as i64)
            ]));
        }
        let arguments = [
            self.word(Slot::Shape(shape), || i64::from(shape)),
            self.iconst(i64::from(layout.objects)),
            self.iconst(i64::from(layout.strings)),
            self.iconst(i64::from(layout.words)),
        ];
        let pointer = self.call_words(Helper::BoxNew, I64, &arguments);
        for (position, (&field, (&item, &offset))) in fields
            .iter()
            .zip(layout.fields.iter().zip(&layout.offsets))
            .enumerate()
        {
            let values = self.get(field)?;
            if !inst.moves(position) {
                self.dup(self.function.ty(field), &values);
            }
            let address = self
                .builder
                .ins()
                .iadd_imm_s(pointer, (values::BOX_FIELDS + 8 * offset as usize) as i64);
            self.store_item(address, item, &values);
        }
        Ok(SmallVec::from_slice(&[pointer]))
    }

    /// A call of an R function through [`crate::ry_call_r`], which reads the arguments' words
    /// from a stack slot and writes the converted result's words after them.
    fn call_r(
        &mut self,
        index: usize,
        arguments: &[Local],
        inst: &Inst,
    ) -> Result<SmallVec<[Value; 2]>, Refusal> {
        let site = *self
            .r_calls
            .get(index)
            .ok_or(Refusal::new("a call of a missing R call"))?;
        let result = match inst.dest {
            Some(dest) => words(self.function.ty(dest)),
            None => SmallVec::new(),
        };
        let mut inputs: SmallVec<[Value; 8]> = SmallVec::new();
        for &argument in arguments {
            inputs.extend(self.get(argument)?);
        }
        let size = 8 * (inputs.len() + 2) as u32;
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            3,
        ));
        for (position, &word) in inputs.iter().enumerate() {
            self.builder
                .ins()
                .stack_store(I64, word, slot, 8 * position as i32);
        }
        let input = self.builder.ins().stack_addr(I64, slot, 0);
        let output_offset = 8 * inputs.len() as i32;
        let output = self.builder.ins().stack_addr(I64, slot, output_offset);
        let site = match self.table {
            None => self.iconst(site as i64),
            Some(_) => self.word(Slot::Site(site as u32), || 0),
        };
        let status = self.call(Helper::CallR, &[I64; 3], &[I32], &[site, input, output])[0];
        let failed = self.builder.ins().icmp_imm_s(IntCC::NotEqual, status, 0);
        self.fail_if(failed, &inst.fail_drops)?;
        Ok(result
            .iter()
            .enumerate()
            .map(|(position, &machine)| {
                self.builder.ins().stack_load(
                    I64,
                    machine,
                    slot,
                    output_offset + 8 * position as i32,
                )
            })
            .collect())
    }

    fn call_typed(
        &mut self,
        index: usize,
        arguments: &[Local],
        inst: &Inst,
    ) -> Result<SmallVec<[Value; 2]>, Refusal> {
        let budget = self
            .budget
            .ok_or(Refusal::new("a function without its budget"))?;
        let budget = self.builder.use_var(budget);
        let budget = self.builder.ins().iadd_imm_s(budget, -1);
        let mut words: SmallVec<[Value; 8]> = SmallVec::from_slice(&[budget]);
        for &argument in arguments {
            words.extend(self.get(argument)?);
        }
        let call = if index == SELF {
            self.builder.ins().call(self.own, &words)
        } else {
            let callee = self
                .callees
                .get(index)
                .ok_or(Refusal::new("a call of a missing function"))?;
            let entry = callee.entry;
            let signature = signature(&callee.params, callee.result);
            self.call_entry(entry, signature, &words)
        };
        let results: SmallVec<[Value; 3]> =
            self.builder.inst_results(call).iter().copied().collect();
        let Some((&status, value)) = results.split_first() else {
            return Err(Refusal::new("a typed call without a status"));
        };
        let failed = self.builder.ins().icmp_imm_s(IntCC::NotEqual, status, 0);
        self.fail_if(failed, &inst.fail_drops)?;
        Ok(SmallVec::from_slice(value))
    }
}

/// The signature of a [`Frees`] entry.
fn free_signature() -> Signature {
    let mut signature = Signature::new(CallConv::Preserving);
    signature.params.push(AbiParam::new(I64));
    signature
}

/// The runtime's code for lists of `item`.
fn kind(item: Item) -> i64 {
    i64::from(match item {
        Item::Int => values::INT,
        Item::Dbl => values::DBL,
        Item::Lgl => values::LGL,
        Item::Str => values::STR,
        Item::I64 => values::I64,
        Item::Record(_) | Item::Tag(_) | Item::Array(_) => values::OBJECT,
    })
}

fn item_size(item: Item) -> i64 {
    match item {
        Item::Int | Item::Lgl => 4,
        Item::Dbl | Item::I64 | Item::Record(_) | Item::Tag(_) | Item::Array(_) => 8,
        Item::Str => 16,
    }
}

fn float_condition(op: Compare) -> FloatCC {
    match op {
        Compare::Eq => FloatCC::Equal,
        Compare::Ne => FloatCC::NotEqual,
        Compare::Lt => FloatCC::LessThan,
        Compare::Le => FloatCC::LessThanOrEqual,
        Compare::Gt => FloatCC::GreaterThan,
        Compare::Ge => FloatCC::GreaterThanOrEqual,
    }
}

fn integer_condition(op: Compare) -> IntCC {
    match op {
        Compare::Eq => IntCC::Equal,
        Compare::Ne => IntCC::NotEqual,
        Compare::Lt => IntCC::SignedLessThan,
        Compare::Le => IntCC::SignedLessThanOrEqual,
        Compare::Gt => IntCC::SignedGreaterThan,
        Compare::Ge => IntCC::SignedGreaterThanOrEqual,
    }
}

extern "C" fn real_power(x: f64, y: f64) -> f64 {
    interp::rmath::r_pow(x, y)
}

extern "C" fn real_platform_power(x: f64, y: f64) -> f64 {
    x.powf(y)
}

extern "C" fn real_modulo(x: f64, y: f64) -> f64 {
    interp::long_double::real_modulo(x, y)
}

extern "C" fn modulus_loses_accuracy(x: f64, y: f64) -> u32 {
    u32::from(interp::long_double::modulus_loses_accuracy(x, y))
}

extern "C" fn real_integer_divide(x: f64, y: f64) -> f64 {
    interp::long_double::real_integer_divide(x, y)
}

/// `max(x, y)` of doubles: `NA` wins over `NaN`, which wins over any number.
extern "C" fn real_max(x: f64, y: f64) -> f64 {
    extreme(x, y, f64::max)
}

extern "C" fn real_min(x: f64, y: f64) -> f64 {
    extreme(x, y, f64::min)
}

fn extreme(x: f64, y: f64, pick: fn(f64, f64) -> f64) -> f64 {
    if x.to_bits() == NA_REAL_BITS || y.to_bits() == NA_REAL_BITS {
        f64::from_bits(NA_REAL_BITS)
    } else if x.is_nan() {
        x
    } else if y.is_nan() {
        y
    } else {
        pick(x, y)
    }
}

extern "C" fn real_exp(x: f64) -> f64 {
    x.exp()
}

extern "C" fn real_log(x: f64) -> f64 {
    x.ln()
}

extern "C" fn real_log2(x: f64) -> f64 {
    x.log2()
}

extern "C" fn real_log10(x: f64) -> f64 {
    x.log10()
}

extern "C" fn real_sin(x: f64) -> f64 {
    x.sin()
}

extern "C" fn real_cos(x: f64) -> f64 {
    x.cos()
}

extern "C" fn real_tan(x: f64) -> f64 {
    x.tan()
}

extern "C" fn real_atan(x: f64) -> f64 {
    x.atan()
}

extern "C" fn real_asin(x: f64) -> f64 {
    x.asin()
}

extern "C" fn real_acos(x: f64) -> f64 {
    x.acos()
}

extern "C" fn real_round_away(x: f64) -> f64 {
    x.round()
}

extern "C" fn real_round(x: f64) -> f64 {
    interp::rmath::fround(x, 0.0)
}

extern "C" fn round_digits(x: f64, digits: f64) -> f64 {
    interp::rmath::fround(x, digits)
}

extern "C" fn real_sign(x: f64) -> f64 {
    if x.is_nan() {
        x
    } else if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

extern "C" fn libcall_floor(x: f64) -> f64 {
    x.floor()
}

extern "C" fn libcall_ceil(x: f64) -> f64 {
    x.ceil()
}

extern "C" fn libcall_trunc(x: f64) -> f64 {
    x.trunc()
}

extern "C" fn libcall_nearbyint(x: f64) -> f64 {
    x.round_ties_even()
}

extern "C" fn libcall_fma(x: f64, y: f64, z: f64) -> f64 {
    x.mul_add(y, z)
}
