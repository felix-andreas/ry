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
            Arith, Block as IrBlock, Compare, Constant, Function, Inst, Item, Local, Math, Op,
            Reduce, Terminator, Text, Type,
        },
        lower::SELF,
        values,
    },
    heap::object::NA_REAL_BITS,
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    ssa::{
        AbiParam, Block, Builder, CallConv, FloatCC, FuncId, FuncRef, IntCC, MemFlagsData, SigRef,
        Signature, StackSlotData, StackSlotKind, Type as Machine, Value, Variable,
        types::{F64, I32, I64},
    },
    std::sync::atomic::AtomicBool,
};

/// A compiled function another one may call: its code's address and its machine signature.
#[derive(Clone, Debug)]
pub struct Callee {
    pub address: usize,
    pub params: SmallVec<[Type; 4]>,
    pub result: Type,
}

/// The addresses of a compiled function and of its trampoline.
#[derive(Clone, Copy, Debug)]
pub struct Code {
    pub address: usize,
    pub trampoline: usize,
}

/// Calls a trampoline: the call budget, the argument words and where the value's words go.
pub type Trampoline = unsafe extern "C" fn(i64, *const u64, *mut u64) -> i64;

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
        Type::Null | Type::Vector { .. } | Type::List(_) | Type::Dict { .. } => {
            SmallVec::from_slice(&[I64])
        }
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

/// Compiles `function`, whose calls of other functions index `callees`.
pub fn compile(function: &Function, callees: &[Callee]) -> Result<Code, Refusal> {
    ssa::with_module(|module| -> Result<Code, Refusal> {
        let params = &function.locals[..function.params];
        let own_signature = signature(params, function.result);
        let id = module.declare(&own_signature)?;
        let mut builder = Builder::new(own_signature.clone());
        let own = builder.import_function(id, own_signature);
        let mut emitter = Emitter {
            builder,
            function,
            callees,
            own,
            host: CallConv::Host,
            variables: Vec::new(),
            blocks: Vec::new(),
            signatures: FxHashMap::default(),
            budget: None,
        };
        emitter.emit_function()?;
        emitter.builder.seal_all_blocks();
        let compiled = emitter.builder.finalize();
        if std::env::var_os("RY_TYPED_DUMP").is_some() {
            eprintln!("{compiled}");
        }
        module.define(id, &compiled)?;
        let trampoline = trampoline(module, id, params, function.result)?;
        module.finalize()?;
        Ok(Code {
            address: module.address(id),
            trampoline: module.address(trampoline),
        })
    })
}

/// A function in the host's convention that calls the compiled function `inner` with
/// arguments read from an array of words and stores its value's words.
fn trampoline(
    module: &mut ssa::Module,
    inner: FuncId,
    params: &[Type],
    result: Type,
) -> Result<FuncId, Refusal> {
    let mut outer = Signature::new(CallConv::Host);
    outer.params.extend([AbiParam::new(I64); 3]);
    outer.returns.push(AbiParam::new(I64));
    let id = module.declare(&outer)?;
    let mut builder = Builder::new(outer);
    let callee = builder.import_function(inner, signature(params, result));
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let &[budget, input, output] = builder.block_params(entry) else {
        return Err(Refusal("a trampoline without its parameters"));
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
        return Err(Refusal("a typed call without a status"));
    };
    for (index, &word) in value.iter().enumerate() {
        builder
            .ins()
            .store(MemFlagsData::trusted(), word, output, 8 * index as i32);
    }
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
    module.define(id, &builder.finalize())?;
    Ok(id)
}

type SignatureKey = (SmallVec<[Machine; 8]>, SmallVec<[Machine; 3]>);

struct Emitter<'a> {
    builder: Builder,
    function: &'a Function,
    callees: &'a [Callee],
    own: FuncRef,
    host: CallConv,
    variables: Vec<SmallVec<[Variable; 2]>>,
    blocks: Vec<Block>,
    signatures: FxHashMap<SignatureKey, SigRef>,
    budget: Option<Variable>,
}

/// A runtime function's address, for a call from compiled code.
macro_rules! address {
    ($function:path) => {
        $function as *const () as usize
    };
}

impl Emitter<'_> {
    fn emit_function(&mut self) -> Result<(), Refusal> {
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let parameters: SmallVec<[Value; 8]> =
            self.builder.block_params(entry).iter().copied().collect();
        let Some((&budget, mut arguments)) = parameters.split_first() else {
            return Err(Refusal("a function without its budget"));
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
                        .ok_or(Refusal("a function without an argument"))?;
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
        // The budget counts the calls still allowed below this one, so the deepest recursion
        // gives up instead of overflowing the machine stack.
        let exhausted = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::SignedLessThan, budget, 1);
        self.fail_if(exhausted, &[])?;
        let first = *self
            .blocks
            .first()
            .ok_or(Refusal("a function without blocks"))?;
        self.builder.ins().jump(first, &[]);
        let function = self.function;
        for (index, block) in function.blocks.iter().enumerate() {
            self.builder.switch_to_block(self.blocks[index]);
            self.emit_block(block)?;
        }
        Ok(())
    }

    fn emit_block(&mut self, block: &IrBlock) -> Result<(), Refusal> {
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
                if words(ty) != words(self.function.result)
                    || (ty != self.function.result
                        && !matches!((ty, self.function.result), (Type::Bool, Type::Lgl)))
                {
                    return Err(Refusal("a `return` of another type than the result"));
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
            .ok_or(Refusal("a jump to a missing block"))
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
            .ok_or(Refusal("a missing local"))?
            .clone();
        Ok(variables
            .iter()
            .map(|&variable| self.builder.use_var(variable))
            .collect())
    }

    /// The single machine word of `local`.
    fn one(&mut self, local: Local) -> Result<Value, Refusal> {
        let values = self.get(local)?;
        values
            .first()
            .copied()
            .ok_or(Refusal("a local without a value"))
    }

    fn set(&mut self, local: Local, values: &[Value]) -> Result<(), Refusal> {
        let variables = self
            .variables
            .get(local.0 as usize)
            .ok_or(Refusal("a missing local"))?
            .clone();
        if variables.len() != values.len() {
            return Err(Refusal("a value with the wrong number of words"));
        }
        for (&variable, &value) in variables.iter().zip(values) {
            self.builder.def_var(variable, value);
        }
        Ok(())
    }

    /// Calls the runtime function at `address` in the host's convention.
    fn call(
        &mut self,
        address: usize,
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
        let callee = self.builder.ins().iconst(I64, address as i64);
        let call = self
            .builder
            .ins()
            .call_indirect(signature, callee, arguments);
        self.builder.inst_results(call).iter().copied().collect()
    }

    /// Calls a runtime function whose parameters are all 64-bit words and which returns one
    /// value of `returns`.
    fn call_words(&mut self, address: usize, returns: Machine, arguments: &[Value]) -> Value {
        let params: SmallVec<[Machine; 8]> = arguments
            .iter()
            .map(|&argument| self.builder.value_type(argument))
            .collect();
        let results = self.call(address, &params, &[returns], arguments);
        results[0]
    }

    fn iconst(&mut self, value: i64) -> Value {
        self.builder.ins().iconst(I64, value)
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
            Type::Int | Type::Lgl | Type::Bool | Type::I64 => {
                let wide = self.wide(values[0]);
                (wide, self.iconst(0))
            }
            _ => return Err(Refusal("an item that is not a scalar")),
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
                self.count(heap, values[0], Some(address!(values::ry_str_free)));
            }
            ty if ty.is_object() => {
                let live = self.builder.ins().icmp_imm_s(IntCC::NotEqual, values[0], 0);
                self.count(live, values[0], Some(address!(values::ry_list_free)));
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
    fn count(&mut self, live: Value, pointer: Value, free: Option<usize>) {
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
                self.call(free, &[I64], &[], &[pointer]);
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
                Constant::Str(lo, hi) => {
                    SmallVec::from_slice(&[self.iconst(lo as i64), self.iconst(hi as i64)])
                }
                Constant::Null => SmallVec::from_slice(&[self.iconst(0)]),
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
                                address!(modulus_loses_accuracy),
                                &[F64, F64],
                                &[I32],
                                &[x, y],
                            )[0];
                            self.fail_if(lost, &inst.fail_drops)?;
                        }
                        self.real_arith(*op, x, y)
                    }
                    Item::I64 => self.i64_arith(*op, x, y, inst)?,
                    _ => self.integer_arith(*op, x, y, inst)?,
                };
                SmallVec::from_slice(&[value])
            }
            Op::Compare(op, item, a, b) => {
                let value = match item {
                    Item::Str => {
                        let (a, b) = (self.get(*a)?, self.get(*b)?);
                        let equal = self.call(
                            address!(values::ry_str_equal),
                            &[I64; 4],
                            &[I32],
                            &[a[0], a[1], b[0], b[1]],
                        )[0];
                        match op {
                            Compare::Eq => equal,
                            Compare::Ne => self.builder.ins().bxor_imm_s(equal, 1),
                            _ => return Err(Refusal("an ordering comparison of strings")),
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
                            _ => return Err(Refusal("an I64 math function")),
                        }
                    }
                    _ => {
                        let na = self.is_na_integer(x);
                        let value = match op {
                            Math::Neg => self.builder.ins().ineg(x),
                            Math::Abs => self.builder.ins().iabs(x),
                            _ => return Err(Refusal("an integer math function")),
                        };
                        self.na_or(na, value)
                    }
                };
                SmallVec::from_slice(&[value])
            }
            Op::RoundDigits(a, digits) => {
                let (x, digits) = (self.one(*a)?, self.one(*digits)?);
                let rounded =
                    self.call(address!(round_digits), &[F64, F64], &[F64], &[x, digits])[0];
                SmallVec::from_slice(&[rounded])
            }
            Op::ToDbl(a) => {
                let x = self.one(*a)?;
                let converted = self.builder.ins().fcvt_from_sint(F64, x);
                let value = if matches!(ty(*a), Type::Bool | Type::I64) {
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
                    _ => return Err(Refusal("an I64 from a value that is not a number")),
                };
                SmallVec::from_slice(&[value])
            }
            Op::LglToInt(a) => SmallVec::from_slice(&[self.one(*a)?]),
            Op::Truth(a) => {
                let x = self.one(*a)?;
                let na = self.is_na_integer(x);
                self.fail_if(na, &inst.fail_drops)?;
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
                    .ok_or(Refusal("an index into a non-vector"))?;
                let pointer = self.one(*a)?;
                let index = self.one(*index)?;
                let address = self.item_address(pointer, index, item, inst)?;
                let values = self.load_item(address, item);
                if item == Item::Str {
                    self.dup(Type::Str, &values);
                }
                values
            }
            Op::Set(a, index, value) => {
                let item = ty(*a)
                    .items()
                    .ok_or(Refusal("an assignment into a non-vector"))?;
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
                if item == Item::Str {
                    if !inst.moves(2) {
                        self.dup(Type::Str, &values);
                    }
                    let old = self.load_item(address, item);
                    self.drop(Type::Str, &old);
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
                let list = self.call_words(address!(values::ry_list_new), I64, &[kind, count]);
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
                let list = self.call_words(address!(values::ry_list_seq_len), I64, &[count]);
                SmallVec::from_slice(&[list])
            }
            Op::Range(from, to) => {
                let (from, to) = (self.one(*from)?, self.one(*to)?);
                let na = self.either_na(from, to);
                self.fail_if(na, &inst.fail_drops)?;
                let (from, to) = (self.wide(from), self.wide(to));
                let list = self.call_words(address!(values::ry_list_seq), I64, &[from, to]);
                SmallVec::from_slice(&[list])
            }
            Op::RangeI64(from, to) => {
                let (from, to) = (self.one(*from)?, self.one(*to)?);
                let list = self.call_words(address!(values::ry_list_range_i64), I64, &[from, to]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::Repeat(value, times) => {
                let item = ty(*value)
                    .item()
                    .ok_or(Refusal("a `rep` of a non-scalar"))?;
                let times = self.one(*times)?;
                let negative = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::SignedLessThan, times, 0);
                self.fail_if(negative, &inst.fail_drops)?;
                let (w0, w1) = self.item_words(*value)?;
                let kind = self.iconst(kind(item));
                let times = self.wide(times);
                let list =
                    self.call_words(address!(values::ry_list_rep), I64, &[kind, w0, w1, times]);
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
                        self.call_words(
                            address!(values::ry_arith),
                            I64,
                            &[operation, kind, x, ox, y, oy],
                        )
                    }
                    (true, false) => {
                        let x = self.one(*a)?;
                        let ox = self.owned(inst, 0);
                        let (y, _) = self.item_words(*b)?;
                        self.call_words(
                            address!(values::ry_arith_vs),
                            I64,
                            &[operation, kind, x, ox, y],
                        )
                    }
                    _ => {
                        let (x, _) = self.item_words(*a)?;
                        let y = self.one(*b)?;
                        let oy = self.owned(inst, 1);
                        self.call_words(
                            address!(values::ry_arith_sv),
                            I64,
                            &[operation, kind, x, y, oy],
                        )
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
                let status = self.call_words(address!(values::ry_elementwise), I32, &arguments);
                self.fail_if(status, &inst.fail_drops)?;
                let result = match (*sum, item) {
                    (true, Item::Dbl) => self.builder.ins().stack_load(I64, F64, out, 0),
                    _ => self.builder.ins().stack_load(I64, I64, out, 0),
                };
                SmallVec::from_slice(&[result])
            }
            Op::VectorCompare(op, item, a, b) => {
                let operation = self.iconst(*op as i64);
                let kind = self.iconst(kind(*item));
                let list = match (ty(*a).items().is_some(), ty(*b).items().is_some()) {
                    (true, true) => {
                        let (x, y) = (self.one(*a)?, self.one(*b)?);
                        let (ox, oy) = (self.owned(inst, 0), self.owned(inst, 1));
                        self.call_words(
                            address!(values::ry_compare),
                            I64,
                            &[operation, kind, x, ox, y, oy],
                        )
                    }
                    (true, false) => {
                        let x = self.one(*a)?;
                        let ox = self.owned(inst, 0);
                        let (y0, y1) = self.item_words(*b)?;
                        self.call_words(
                            address!(values::ry_compare_vs),
                            I64,
                            &[operation, kind, x, ox, y0, y1],
                        )
                    }
                    _ => {
                        let (x0, x1) = self.item_words(*a)?;
                        let y = self.one(*b)?;
                        let oy = self.owned(inst, 1);
                        self.call_words(
                            address!(values::ry_compare_sv),
                            I64,
                            &[operation, kind, x0, x1, y, oy],
                        )
                    }
                };
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::VectorLogic(and, a, b) => {
                let operation = self.iconst(if *and { 0 } else { 1 });
                let (x, y) = (self.one(*a)?, self.one(*b)?);
                let (ox, oy) = (self.owned(inst, 0), self.owned(inst, 1));
                let list =
                    self.call_words(address!(values::ry_logic), I64, &[operation, x, ox, y, oy]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::VectorNot(a) => {
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let list = self.call_words(address!(values::ry_not), I64, &[x, owned]);
                SmallVec::from_slice(&[list])
            }
            Op::VectorMath(op, _, a) => {
                let operation = self.iconst(*op as i64);
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let list = self.call_words(address!(values::ry_math), I64, &[operation, x, owned]);
                self.fail_if_null(list, inst)?;
                SmallVec::from_slice(&[list])
            }
            Op::VectorRound(a, digits) => {
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let digits = self.one(*digits)?;
                let digits = self.builder.ins().fcvt_to_sint_sat(I32, digits);
                let list = self.call(
                    address!(values::ry_round),
                    &[I64, I64, I32],
                    &[I64],
                    &[x, owned, digits],
                )[0];
                SmallVec::from_slice(&[list])
            }
            Op::Coerce(a, item) => {
                let x = self.one(*a)?;
                let owned = self.owned(inst, 0);
                let kind = self.iconst(kind(*item));
                let list =
                    self.call_words(address!(values::ry_list_coerce), I64, &[x, owned, kind]);
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
                    address!(values::ry_str_concat),
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
                    address!(values::ry_list_paste),
                    I64,
                    &[base, count, separator[0], separator[1]],
                );
                SmallVec::from_slice(&[list])
            }
            Op::Join(a, separator) => {
                let x = self.one(*a)?;
                let separator = self.get(*separator)?;
                let text = self.call(
                    address!(values::ry_list_join),
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
                        address!(values::ry_str_nchar),
                        &[I64; 2],
                        &[I32],
                        &[x[0], x[1]],
                    )),
                    Text::Upper | Text::Lower => {
                        let owned = self.owned(inst, 0);
                        let function = if *text == Text::Upper {
                            address!(values::ry_str_toupper)
                        } else {
                            address!(values::ry_str_tolower)
                        };
                        SmallVec::from_slice(&self.call(
                            function,
                            &[I64; 3],
                            &[I64, I64],
                            &[x[0], x[1], owned],
                        ))
                    }
                }
            }
            Op::VectorText(text, a) => {
                let x = self.one(*a)?;
                let list = match text {
                    Text::Nchar => self.call_words(address!(values::ry_list_nchar), I64, &[x]),
                    Text::Upper | Text::Lower => {
                        let owned = self.owned(inst, 0);
                        let function = if *text == Text::Upper {
                            address!(values::ry_list_toupper)
                        } else {
                            address!(values::ry_list_tolower)
                        };
                        self.call_words(function, I64, &[x, owned])
                    }
                };
                SmallVec::from_slice(&[list])
            }
            Op::Affix(starts, a, affix) => {
                let (x, affix) = (self.get(*a)?, self.get(*affix)?);
                let function = if *starts {
                    address!(values::ry_str_starts_with)
                } else {
                    address!(values::ry_str_ends_with)
                };
                SmallVec::from_slice(&self.call(
                    function,
                    &[I64; 4],
                    &[I32],
                    &[x[0], x[1], affix[0], affix[1]],
                ))
            }
            Op::VectorAffix(starts, a, affix) => {
                let x = self.one(*a)?;
                let affix = self.get(*affix)?;
                let function = if *starts {
                    address!(values::ry_list_starts_with)
                } else {
                    address!(values::ry_list_ends_with)
                };
                let list = self.call_words(function, I64, &[x, affix[0], affix[1]]);
                SmallVec::from_slice(&[list])
            }
            Op::Format(a) => {
                let x = self.one(*a)?;
                self.fail_if_na(x, ty(*a), &inst.fail_drops)?;
                let text = match ty(*a) {
                    Type::Dbl => {
                        self.call(address!(values::ry_format_dbl), &[F64], &[I64, I64], &[x])
                    }
                    _ => {
                        let x = self.wide(x);
                        self.call(address!(values::ry_format_int), &[I64], &[I64, I64], &[x])
                    }
                };
                SmallVec::from_slice(&text)
            }
            Op::FormatFixed(a, decimals) => {
                let x = self.one(*a)?;
                self.fail_if_na(x, ty(*a), &inst.fail_drops)?;
                let decimals = self.iconst(i64::from(*decimals));
                SmallVec::from_slice(&self.call(
                    address!(values::ry_format_fixed),
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
                    address!(values::ry_format_padded_int),
                    &[I64; 3],
                    &[I64, I64],
                    &[x, width, zero],
                ))
            }
            Op::CallTyped(index, arguments) => self.call_typed(*index, arguments, inst)?,
            Op::Poll => {
                let flag = &interp::unwind::INTERRUPT_REQUESTED as *const AtomicBool as i64;
                let address = self.iconst(flag);
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

    fn real_arith(&mut self, op: Arith, x: Value, y: Value) -> Value {
        let helper = match op {
            Arith::Add => return self.builder.ins().fadd(x, y),
            Arith::Sub => return self.builder.ins().fsub(x, y),
            Arith::Mul => return self.builder.ins().fmul(x, y),
            Arith::Div => return self.builder.ins().fdiv(x, y),
            Arith::Pow => address!(real_power),
            Arith::NumPow => address!(real_platform_power),
            Arith::Mod => address!(real_modulo),
            Arith::IntDiv => address!(real_integer_divide),
            Arith::Min => address!(real_min),
            Arith::Max => address!(real_max),
        };
        self.call(helper, &[F64, F64], &[F64], &[x, y])[0]
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
            Arith::Div | Arith::Pow | Arith::NumPow => {
                return Err(Refusal("an integer division or power"));
            }
        };
        Ok(self.na_or(na, value))
    }

    /// `I64` arithmetic as the standard library does it: an overflow, a division by zero and a
    /// negative power give up, so the interpreter raises the error.
    fn i64_arith(&mut self, op: Arith, x: Value, y: Value, inst: &Inst) -> Result<Value, Refusal> {
        Ok(match op {
            Arith::Add | Arith::Sub | Arith::Mul => {
                let (value, overflow) = match op {
                    Arith::Add => self.builder.ins().sadd_overflow(x, y),
                    Arith::Sub => self.builder.ins().ssub_overflow(x, y),
                    _ => self.builder.ins().smul_overflow(x, y),
                };
                self.fail_if(overflow, &inst.fail_drops)?;
                value
            }
            Arith::Mod | Arith::IntDiv => {
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
                let status = self.call(
                    address!(values::ry_power_i64),
                    &[I64, I64, I64],
                    &[I32],
                    &[x, y, out],
                )[0];
                self.fail_if(status, &inst.fail_drops)?;
                self.builder.ins().stack_load(I64, I64, slot, 0)
            }
            Arith::Min => self.builder.ins().smin(x, y),
            Arith::Max => self.builder.ins().smax(x, y),
            Arith::Div => return Err(Refusal("a `/` of I64s")),
            Arith::NumPow => return Err(Refusal("a `Num.pow` of I64s")),
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
            Math::Exp => Some(address!(real_exp)),
            Math::Log => Some(address!(real_log)),
            Math::Log2 => Some(address!(real_log2)),
            Math::Log10 => Some(address!(real_log10)),
            Math::Sin => Some(address!(real_sin)),
            Math::Cos => Some(address!(real_cos)),
            Math::Tan => Some(address!(real_tan)),
            Math::Round => Some(address!(real_round)),
            Math::Sign => Some(address!(real_sign)),
            Math::RoundAway => Some(address!(real_round_away)),
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
            (Reduce::Sum, Item::Dbl) => {
                self.call(address!(values::ry_sum_dbl), &[I64], &[F64], &[x])[0]
            }
            (Reduce::Sum, Item::I64) => {
                self.reduce_status(address!(values::ry_sum_i64), I64, x, inst)?
            }
            (_, Item::I64) => return Err(Refusal("an I64 reduction other than a sum")),
            (Reduce::Prod, _) => self.call(address!(values::ry_prod_dbl), &[I64], &[F64], &[x])[0],
            (Reduce::Mean, _) => self.call(address!(values::ry_mean_dbl), &[I64], &[F64], &[x])[0],
            (Reduce::Sum, _) => self.reduce_status(address!(values::ry_sum_int), I32, x, inst)?,
            (Reduce::Max, Item::Dbl) => {
                self.reduce_status(address!(values::ry_max_dbl), F64, x, inst)?
            }
            (Reduce::Min, Item::Dbl) => {
                self.reduce_status(address!(values::ry_min_dbl), F64, x, inst)?
            }
            (Reduce::Max, _) => self.reduce_status(address!(values::ry_max_int), I32, x, inst)?,
            (Reduce::Min, _) => self.reduce_status(address!(values::ry_min_int), I32, x, inst)?,
            (Reduce::Any, _) => self.reduce_status(address!(values::ry_any), I32, x, inst)?,
            (Reduce::All, _) => self.reduce_status(address!(values::ry_all), I32, x, inst)?,
        })
    }

    /// A reduction that writes its value through a pointer and returns a status. Status 1 means
    /// the value is `NA` and stands, and status 2 (an empty list, or an overflow) gives up.
    fn reduce_status(
        &mut self,
        address: usize,
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
        let status = self.call(address, &[I64, I64], &[I32], &[x, out])[0];
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
            return self.call_words(
                address!(values::ry_list_unshare),
                I64,
                &[pointer, owned_flag],
            );
        }
        let count = self
            .builder
            .ins()
            .load(I32, MemFlagsData::trusted(), pointer, 0);
        let alone = self.builder.ins().icmp_imm_s(IntCC::Equal, count, 1);
        let (copy, done) = (self.builder.create_block(), self.builder.create_block());
        self.builder.append_block_param(done, I64);
        self.builder
            .ins()
            .brif(alone, done, &[pointer], copy, &[]);
        self.builder.switch_to_block(copy);
        let copied = self.call_words(
            address!(values::ry_list_unshare),
            I64,
            &[pointer, owned_flag],
        );
        self.builder.ins().jump(done, &[copied]);
        self.builder.switch_to_block(done);
        self.builder.block_params(done)[0]
    }

    fn call_typed(
        &mut self,
        index: usize,
        arguments: &[Local],
        inst: &Inst,
    ) -> Result<SmallVec<[Value; 2]>, Refusal> {
        let budget = self
            .budget
            .ok_or(Refusal("a function without its budget"))?;
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
                .ok_or(Refusal("a call of a missing function"))?;
            let signature = self
                .builder
                .import_signature(signature(&callee.params, callee.result));
            let address = self.iconst(callee.address as i64);
            self.builder.ins().call_indirect(signature, address, &words)
        };
        let results: SmallVec<[Value; 3]> =
            self.builder.inst_results(call).iter().copied().collect();
        let Some((&status, value)) = results.split_first() else {
            return Err(Refusal("a typed call without a status"));
        };
        let failed = self.builder.ins().icmp_imm_s(IntCC::NotEqual, status, 0);
        self.fail_if(failed, &inst.fail_drops)?;
        Ok(SmallVec::from_slice(value))
    }
}

/// The runtime's code for lists of `item`.
fn kind(item: Item) -> i64 {
    i64::from(match item {
        Item::Int => values::INT,
        Item::Dbl => values::DBL,
        Item::Lgl => values::LGL,
        Item::Str => values::STR,
        Item::I64 => values::I64,
    })
}

fn item_size(item: Item) -> i64 {
    match item {
        Item::Int | Item::Lgl => 4,
        Item::Dbl | Item::I64 => 8,
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
    interp::arith::real_modulo(x, y)
}

extern "C" fn modulus_loses_accuracy(x: f64, y: f64) -> u32 {
    u32::from(interp::arith::modulus_loses_accuracy(x, y))
}

extern "C" fn real_integer_divide(x: f64, y: f64) -> f64 {
    interp::arith::real_integer_divide(x, y)
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
