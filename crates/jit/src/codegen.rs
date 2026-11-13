//! Machine code for a [`Plan`], through Cranelift.
//!
//! Every tier-0 register and every variable slot becomes Cranelift variables, one per machine
//! representation (`i32` for integers and logicals, `f64` for doubles), and the frontend builds
//! the SSA form from the definitions. Boxed values never live only in a machine register: a
//! boxed register is stored to its place in the register window and a boxed slot value to its
//! root cell, which is what roots them, so every object compiled code holds is visible to the
//! collector at every allocation, and every object handed to a helper is a valid `SEXP`.
//!
//! Each guard that can fail branches to a cold deoptimization block. That block boxes the
//! unboxed values tier 0 will read, writes dirty variables back, stores the loop states and
//! asks the interpreter to continue at the guarded instruction (or after it, when the
//! instruction already ran).

use {
    crate::{
        analysis::{
            self, Action, Argument, Effect, Fast, Fold, Intrinsic, Numeric, Plan, Refusal, StateId,
            StateRef, Store, Ty, jump_target, status,
        },
        native,
        runtime::{self, Activation, Exit, offset, outcome, quiet, reason, representation},
    },
    heap::object::{
        INTEGER_PREFIX, LAZY_RECORD, LOGICAL_PREFIX, NA_INTEGER, NA_REAL_BITS, RECORD_DATA_OFFSET,
        RECORD_LAYOUT_OFFSET, RECORD_UNBOXED_OFFSET, REFCOUNT_MAX, Sexp, SexpType, UNBOXED_SLOTS,
        VECTOR_DATA_OFFSET, header_layout,
    },
    interp::{
        annotations,
        bytecode::{self, Arith, Op, interp_layout},
        values,
    },
    rustc_hash::FxHashMap,
    smallvec::SmallVec,
    ssa::{
        AbiParam, Allocator, Block as Label, Builder, CallConv, FloatCC, Ieee64, IntCC,
        MemFlagsData, SigRef, Signature, StackSlot, StackSlotData, StackSlotKind, Type, Value,
        Variable,
        types::{F64, I8, I32, I64},
    },
    std::sync::atomic::AtomicBool,
};

/// The compiled entry point.
pub type Entry = unsafe extern "C" fn(*mut Activation) -> *mut heap::object::Header;

/// Header offsets, as `heap::object` lays objects out.
pub(crate) const KIND: i32 = 0;
pub(crate) const FLAGS: i32 = 2;
const REFCOUNT: i32 = 6;
const ATTRIB: i32 = 8;
pub(crate) const LENGTH: i32 = 16;
/// An ALTREP instance's first data slot and its class.
const CAR: i32 = 16;
const TAG: i32 = 32;
pub(crate) const DATA: i32 = VECTOR_DATA_OFFSET as i32;
const ALTREP_FLAG: i64 = 1 << 1;
/// ry's `Array<T>`: its own JIT types (`Ty::IntArr` and so on), and it fails every guard for a
/// plain vector, whose operator and indexing rules differ.
const ARRAY_FLAG: i64 = 1 << 4;
pub(crate) const OBJECT_FLAG: i64 = 1 << 0;

/// The subscripts of an element assignment: one position, or a matrix's row and column.
#[derive(Clone, Copy)]
enum Subscript {
    Vector(usize),
    Matrix(usize, usize),
}

/// The length an element read checks its position against: an ordinary vector's integer
/// length, or a compact sequence's double one.
#[derive(Clone, Copy)]
enum Length {
    Integer(Value),
    Double(Value),
}

/// Compiles `plan` and returns its entry point, its deoptimization exits and the size of its
/// machine code in bytes.
pub unsafe fn compile(plan: &Plan, code: Sexp) -> Result<(Entry, Box<[Exit]>, u32), Refusal> {
    let mut signature = Signature::new(CallConv::Host);
    signature.params.push(AbiParam::new(I64));
    signature.returns.push(AbiParam::new(I64));
    let mut translator = Translator::new(plan, code, Builder::new(signature), CallConv::Host);
    // SAFETY: the plan was made for `code`, whose constants the translation reads.
    let exits = unsafe { translator.translate() }?;
    translator.builder.seal_all_blocks();
    let function = translator.builder.finalize();
    if std::env::var_os("RY_JIT_DUMP").is_some() {
        eprintln!("{function}");
    }
    // Keeping values in registers pays where code repeats, so a body with a loop gets the
    // backtracking allocator: on loop kernels its code runs 1.5 to 2.4 times faster. Code
    // without a loop is mostly helper calls, where single-pass code runs as fast, and that
    // allocator compiles in about two thirds of the time.
    let allocator = if plan.loops > 0 {
        Allocator::Backtracking
    } else {
        Allocator::SinglePass
    };
    let (address, size) = ssa::with_module(allocator, |module| -> Result<_, Refusal> {
        let id = module.declare(&function.signature)?;
        let size = module.define(id, &function)?;
        module.finalize()?;
        Ok((module.address(id), size))
    })?;
    // SAFETY: the function was built with exactly this signature.
    let entry = unsafe { std::mem::transmute::<*const u8, Entry>(address as *const u8) };
    Ok((entry, exits, size))
}

/// A cold block that hands the activation back to the interpreter.
struct Deopt {
    label: Label,
    pc: usize,
    state: StateId,
    /// A register the state has unboxed that holds a boxed value here.
    boxed: Option<usize>,
    /// The instruction whose live registers apply.
    live: usize,
    why: i32,
}

/// An edge that converts representations before reaching its target block.
struct PendingEdge {
    label: Label,
    state: StateId,
    target: usize,
}

struct Translator<'p> {
    plan: &'p Plan<'p>,
    code: Sexp,
    builder: Builder,
    call_conv: CallConv,
    signatures: FxHashMap<(Vec<Type>, Option<Type>), SigRef>,
    activation: Value,
    window: Value,
    loops: Value,
    visible: Value,
    integers: Vec<Variable>,
    doubles: Vec<Variable>,
    slot_integers: Vec<Variable>,
    slot_doubles: Vec<Variable>,
    /// Each slot's run-time status, one 4-byte cell per slot. A status lives in memory rather
    /// than in a variable because nearly every slot's status is live across the whole body,
    /// so as variables they gave every merge a parameter per slot: construction and register
    /// allocation then grew with merges times slots, which dominated the compile time of
    /// large bodies.
    statuses: StackSlot,
    loop_indexes: Vec<Variable>,
    loop_counts: Vec<Variable>,
    /// The first element and increment of each loop's compact sequence, as in
    /// [`runtime::LOOP_WORDS`]; an increment of 0 reads the sequence's storage.
    loop_firsts: Vec<Variable>,
    loop_steps: Vec<Variable>,
    labels: Vec<Option<Label>>,
    deopts: Vec<Deopt>,
    edges: Vec<PendingEdge>,
    nil: i64,
    /// The position of the first instruction of the block being translated.
    block_start: usize,
}

/// An object tier 0 already holds for a value compiled code keeps unboxed.
#[derive(Clone, Copy)]
enum Known {
    Constant(Sexp),
    Slot(usize),
}

/// A value's [`representation`] in a deoptimization exit.
fn exit_kind(ty: Ty) -> i32 {
    match ty {
        Ty::Int => representation::INTEGER,
        Ty::Lgl => representation::LOGICAL,
        Ty::Dbl => representation::DOUBLE,
        Ty::Num => representation::NUM,
        _ => representation::BOXED,
    }
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

impl<'p> Translator<'p> {
    fn new(
        plan: &'p Plan<'p>,
        code: Sexp,
        mut builder: Builder,
        call_conv: CallConv,
    ) -> Translator<'p> {
        let mut declare = |count: usize, ty: Type| {
            (0..count)
                .map(|_| builder.declare_var(ty))
                .collect::<Vec<_>>()
        };
        let integers = declare(plan.registers + plan.temporaries, I32);
        let doubles = declare(plan.registers + plan.temporaries, F64);
        let slot_integers = declare(plan.slots.len(), I32);
        let slot_doubles = declare(plan.slots.len(), F64);
        let loop_indexes = declare(plan.loops, I64);
        let loop_counts = declare(plan.loops, I64);
        let loop_firsts = declare(plan.loops, F64);
        let loop_steps = declare(plan.loops, F64);
        let placeholder = Value::from_u32(0);
        Translator {
            plan,
            code,
            builder,
            call_conv,
            signatures: FxHashMap::default(),
            activation: placeholder,
            window: placeholder,
            loops: placeholder,
            visible: placeholder,
            integers,
            doubles,
            slot_integers,
            slot_doubles,
            statuses: StackSlot::from_u32(0),
            loop_indexes,
            loop_counts,
            loop_firsts,
            loop_steps,
            labels: Vec::new(),
            deopts: Vec::new(),
            edges: Vec::new(),
            nil: values::nil().as_ptr() as i64,
            block_start: 0,
        }
    }

    unsafe fn translate(&mut self) -> Result<Box<[Exit]>, Refusal> {
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        self.activation = self.builder.block_params(entry)[0];
        self.window = self
            .builder
            .ins()
            .load(I64, flags(), self.activation, offset::REGISTERS);
        self.loops = self
            .builder
            .ins()
            .load(I64, flags(), self.activation, offset::LOOPS);
        self.visible = self
            .builder
            .ins()
            .load(I64, flags(), self.activation, offset::VISIBLE);
        let entry_pc = self
            .builder
            .ins()
            .load(I64, flags(), self.activation, offset::ENTRY_PC);
        let zero32 = self.builder.ins().iconst(I32, 0);
        let zero64 = self.builder.ins().iconst(I64, 0);
        let zero_double = self.builder.ins().f64const(0.0);
        for variable in self.integers.iter().chain(&self.slot_integers) {
            self.builder.def_var(*variable, zero32);
        }
        self.statuses = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (4 * self.plan.slots.len()).max(4) as u32,
            2,
        ));
        for slot in 0..self.plan.slots.len() {
            self.set_status(slot, zero32);
        }
        for variable in self
            .doubles
            .iter()
            .chain(&self.slot_doubles)
            .chain(&self.loop_firsts)
            .chain(&self.loop_steps)
        {
            self.builder.def_var(*variable, zero_double);
        }
        for variable in self.loop_indexes.iter().chain(&self.loop_counts) {
            self.builder.def_var(*variable, zero64);
        }
        self.labels = self
            .plan
            .blocks
            .iter()
            .map(|block| block.entry.map(|_| self.builder.create_block()))
            .collect();
        let start = self
            .labels
            .first()
            .copied()
            .flatten()
            .ok_or(Refusal("the body has no reachable code"))?;
        // Dispatch on where execution starts: the top, or a loop header.
        let mut osr = Vec::new();
        for &header in &self.plan.loop_headers {
            let Some(label) = self.labels[header] else {
                continue;
            };
            let pc = self.plan.insts[self.plan.blocks[header].start].pc;
            let enter = self.builder.create_block();
            let next = self.builder.create_block();
            let here = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::Equal, entry_pc, pc as i64);
            self.builder.ins().brif(here, enter, &[], next, &[]);
            self.builder.switch_to_block(next);
            osr.push((enter, header, label));
        }
        self.builder.ins().jump(start, &[]);
        let declined = self.builder.create_block();
        for (enter, header, label) in osr {
            self.builder.switch_to_block(enter);
            self.osr_entry(header, label, declined)?;
        }
        self.builder.switch_to_block(declined);
        self.builder.set_cold_block(declined);
        let sentinel = self.builder.ins().iconst(I64, runtime::DECLINED as i64);
        self.builder.ins().return_(&[sentinel]);

        for index in 0..self.plan.blocks.len() {
            let Some(label) = self.labels[index] else {
                continue;
            };
            self.builder.switch_to_block(label);
            let (start, end) = (self.plan.blocks[index].start, self.plan.blocks[index].end);
            self.block_start = start;
            for position in start..end {
                let terminated = self.instruction(position)?;
                if position + 1 == end && !terminated {
                    let next = self
                        .plan
                        .insts
                        .get(end)
                        .ok_or(Refusal("the body falls off its end"))?
                        .pc;
                    let target = self.edge(position, next)?;
                    self.builder.ins().jump(target, &[]);
                    self.fill_edges()?;
                }
            }
        }
        Ok(self.emit_deopts())
    }

    /// Enters at a loop header with tier 0's registers in the window and every variable in the
    /// environment, declining when a register's value does not have the type the loop assumes.
    unsafe fn osr_entry(
        &mut self,
        header: usize,
        label: Label,
        declined: Label,
    ) -> Result<(), Refusal> {
        for index in 0..self.plan.loops {
            let words = (8 * runtime::LOOP_WORDS * index) as i32;
            let loop_index = self.builder.ins().load(I64, flags(), self.loops, words);
            let count = self.builder.ins().load(I64, flags(), self.loops, words + 8);
            let first = self
                .builder
                .ins()
                .load(F64, flags(), self.loops, words + 16);
            let step = self
                .builder
                .ins()
                .load(F64, flags(), self.loops, words + 24);
            self.builder.def_var(self.loop_indexes[index], loop_index);
            self.builder.def_var(self.loop_counts[index], count);
            self.builder.def_var(self.loop_firsts[index], first);
            self.builder.def_var(self.loop_steps[index], step);
        }
        // A loop's sequence register holds a compact sequence, which no vector type's check
        // accepts, exactly when tier 0 gave the loop an increment.
        let sequences: Vec<(usize, usize)> = self
            .plan
            .actions
            .iter()
            .filter_map(|action| match *action {
                Action::ForInit { index, src, .. } => Some((src, index)),
                _ => None,
            })
            .collect();
        let entry = self.plan.state(
            self.plan.blocks[header]
                .entry
                .ok_or(Refusal("an unreachable loop header"))?,
        );
        for (register, &ty) in entry.registers.iter().enumerate() {
            if matches!(ty, Ty::Bottom | Ty::Boxed) {
                continue;
            }
            let value = self.load_register(register);
            let mut fits = self.check(value, ty);
            if let Some(&(_, index)) = sequences.iter().find(|&&(src, _)| src == register) {
                let step = self.builder.use_var(self.loop_steps[index]);
                let compact = self.compact_sequence_fits(value, ty, step);
                fits = self.builder.ins().bor(fits, compact);
            }
            let next = self.builder.create_block();
            self.builder.ins().brif(fits, next, &[], declined, &[]);
            self.builder.switch_to_block(next);
            if ty == Ty::Num {
                let (unboxed, flag) = self.unbox_num(value);
                self.def_register(register, ty, unboxed);
                self.builder.def_var(self.integers[register], flag);
            } else if ty.is_unboxed() {
                let unboxed = self.unbox(value, ty);
                self.def_register(register, ty, unboxed);
            }
        }
        self.builder.ins().jump(label, &[]);
        Ok(())
    }

    /// Translates one instruction; returns whether it emitted the block's terminator.
    unsafe fn instruction(&mut self, position: usize) -> Result<bool, Refusal> {
        let plan = self.plan;
        let Some(before) = plan.before(position) else {
            return Ok(false);
        };
        for &register in plan.boxings(position) {
            let boxed = self.box_register(register, Ty::Num);
            self.store_register(register, boxed);
        }
        self.action(position, before, plan.actions[position])
    }

    /// The run-time status of `slot`, one of the `status::RUNTIME_*` values or 0 for not loaded.
    fn status(&mut self, slot: usize) -> Value {
        self.builder
            .ins()
            .stack_load(I64, I32, self.statuses, 4 * slot as i32)
    }

    fn set_status(&mut self, slot: usize, status: Value) {
        self.builder
            .ins()
            .stack_store(I64, status, self.statuses, 4 * slot as i32);
    }

    /// Translates one action with the state before it; returns whether it emitted the block's
    /// terminator. The steps of an inlined call come here too, with the call's position, so
    /// every exit they take leaves at the call.
    unsafe fn action(
        &mut self,
        position: usize,
        before: StateRef<'p>,
        action: Action<'p>,
    ) -> Result<bool, Refusal> {
        let plan = self.plan;
        let inst = &plan.insts[position];
        let pc = inst.pc;
        match action {
            Action::Skip | Action::FallThrough => {}
            Action::Clear { register, range } => {
                let nil = self
                    .builder
                    .ins()
                    .iconst(I64, interp::heap().known().nil.as_ptr() as i64);
                for register in std::iter::once(register).chain(range.0..range.1) {
                    self.store_register(register, nil);
                }
            }
            Action::Srcref(srcref) => {
                let srcref = self.builder.ins().iconst(I64, srcref.as_ptr() as i64);
                let at = interp_layout::SRCREF as i32 - interp_layout::VISIBLE as i32;
                self.builder.ins().store(flags(), srcref, self.visible, at);
            }
            Action::Constant { dst, value, ty } => {
                match ty {
                    Ty::Int | Ty::Lgl => {
                        let constant = self
                            .builder
                            .ins()
                            .iconst(I32, values::ints(value)[0] as i64);
                        self.builder.def_var(self.integers[dst], constant);
                    }
                    Ty::Dbl => {
                        let bits = values::reals(value)[0].to_bits();
                        let constant = self.builder.ins().f64const(Ieee64::with_bits(bits));
                        self.builder.def_var(self.doubles[dst], constant);
                    }
                    _ => {
                        let pointer = self.builder.ins().iconst(I64, value.as_ptr() as i64);
                        self.store_register(dst, pointer);
                    }
                }
                self.set_visible(&before, true);
            }
            Action::GetVar {
                dst,
                slot,
                load,
                forcing,
                volatile,
            } => {
                self.set_visible(&before, true);
                if let Some(ty) = load {
                    self.load_slot(position, &before, slot, ty, forcing, volatile, Some(dst))?;
                }
                let ty = load.unwrap_or(before.slots[slot].ty);
                if ty.in_variables() {
                    let value = self.use_slot(slot, ty);
                    self.def_register(dst, ty, value);
                    if ty == Ty::Num {
                        let flag = self.builder.use_var(self.slot_integers[slot]);
                        self.builder.def_var(self.integers[dst], flag);
                    }
                } else {
                    let value = self.load_slot_cell(slot);
                    self.store_register(dst, value);
                }
            }
            Action::SetVar { slot, src, expand } => {
                let ty = match before.registers[src] {
                    Ty::Bottom => Ty::Boxed,
                    ty => ty,
                };
                let value = if ty.in_variables() {
                    self.use_register(src, ty)
                } else {
                    self.load_register(src)
                };
                let (ty, value) = if expand {
                    let (ty, kind) = if ty == Ty::IntSeq {
                        (Ty::IntVec, SexpType::Integer)
                    } else {
                        (Ty::DblVec, SexpType::Real)
                    };
                    let pc = self.plan.insts[position].pc;
                    let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
                    let kind = self.builder.ins().iconst(I64, kind as i64);
                    let contents = self
                        .call(
                            runtime::jit_sequence_contents as *const () as usize,
                            &[I64, I64, I64],
                            Some(I64),
                            &[self.activation, value, kind],
                        )
                        .ok_or(Refusal("a helper without a result"))?;
                    // The sequence passed its guard, so only an error makes the copy fail.
                    let copied = self.builder.ins().icmp_imm_s(IntCC::NotEqual, contents, 0);
                    self.guard(copied, failed);
                    (ty, contents)
                } else {
                    (ty, value)
                };
                if ty == Ty::Num {
                    let flag = self.builder.use_var(self.integers[src]);
                    self.builder.def_var(self.slot_integers[slot], flag);
                }
                self.assign_slot(&before, slot, ty, value);
                self.set_visible(&before, false);
            }
            Action::GetFun { dst, function } => {
                let pointer = self.builder.ins().iconst(I64, function.as_ptr() as i64);
                self.store_register(dst, pointer);
            }
            Action::GetFunSlot { dst, slot, load } => {
                if let Some(ty) = load {
                    self.load_slot(position, &before, slot, ty, false, false, None)?;
                }
                let value = self.load_slot_cell(slot);
                self.store_register(dst, value);
            }
            Action::Inline { steps } => {
                for &(step, state) in steps {
                    self.action(position, self.plan.state(state), step)?;
                }
                self.set_visible(&before, true);
            }
            action @ Action::Traverse { .. } => {
                self.traverse(position, &before, action)?;
                self.set_visible(&before, true);
            }
            Action::Call {
                steps,
                function,
                call,
                args,
                entry,
                dst,
            } => {
                self.direct_call(position, before, steps, function, call, args, entry, dst)?;
            }
            Action::NativeCall {
                steps,
                native,
                args,
                dst,
            } => {
                for &(step, state) in steps {
                    self.action(position, self.plan.state(state), step)?;
                }
                let budget = self
                    .call(
                        runtime::jit_depth_budget as *const () as usize,
                        &[],
                        Some(I64),
                        &[],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                let mut values: SmallVec<[Value; 5]> = SmallVec::new();
                values.push(budget);
                for argument in args {
                    values.push(match *argument {
                        Argument::Slot(slot) => self.load_slot_cell(slot),
                        Argument::Unboxed { register, ty } if ty.in_variables() => {
                            self.use_register(register, ty)
                        }
                        Argument::Unboxed { register, .. } => self.load_register(register),
                    });
                }
                let signature = self.builder.import_signature(native::signature(
                    &native.params,
                    native.result,
                    self.call_conv,
                ));
                let address = self.builder.ins().iconst(I64, native.address as i64);
                let call = self
                    .builder
                    .ins()
                    .call_indirect(signature, address, &values);
                let &[status, value] = self.builder.inst_results(call) else {
                    return Err(Refusal("a native call without a status and a value"));
                };
                // Nothing the native function did is visible, so tier 0 runs the call again
                // and reports what went wrong.
                let failed = self.deopt(inst.pc, before.id, None, position, reason::HANDOFF);
                let ok = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, status, native::OK);
                self.guard(ok, failed);
                self.def_register(dst, native.result, value);
                self.set_visible(&before, true);
            }
            Action::Peek { slot, ty } => self.peek(position, &before, slot, ty)?,
            Action::Construct {
                dst,
                layout,
                base,
                count,
            } => {
                let buffer = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    (16 * count.max(1)) as u32,
                    3,
                ));
                for index in 0..count {
                    let register = base + index;
                    let ty = before.registers[register];
                    let (kind, bits) = match ty {
                        Ty::Dbl => {
                            let value = self.use_register(register, ty);
                            (
                                representation::DOUBLE,
                                self.builder.ins().bitcast(I64, MemFlagsData::new(), value),
                            )
                        }
                        Ty::Int | Ty::Lgl => {
                            let value = self.use_register(register, ty);
                            let kind = if ty == Ty::Int {
                                representation::INTEGER
                            } else {
                                representation::LOGICAL
                            };
                            (kind, self.builder.ins().sextend(I64, value))
                        }
                        _ => (representation::BOXED, self.load_register(register)),
                    };
                    let kind = self.builder.ins().iconst(I64, kind as i64);
                    self.builder
                        .ins()
                        .stack_store(I64, kind, buffer, (16 * index) as i32);
                    self.builder
                        .ins()
                        .stack_store(I64, bits, buffer, (16 * index + 8) as i32);
                }
                let pointer = self.builder.ins().stack_addr(I64, buffer, 0);
                let layout = self.builder.ins().iconst(I32, i64::from(layout));
                let count = iconst(&mut self.builder, count);
                let value = self
                    .call(
                        runtime::jit_construct as *const () as usize,
                        &[I64, I32, I64, I64],
                        Some(I64),
                        &[self.activation, layout, pointer, count],
                    )
                    .ok_or(Refusal("the record helper returned nothing"))?;
                self.store_register(dst, value);
                self.set_visible(&before, true);
            }
            Action::Move { dst, src, ty } => {
                // Only a scalar lives in a variable; any other value lives in the window.
                if ty.in_variables() {
                    let value = self.use_register(src, ty);
                    self.def_register(dst, ty, value);
                    if ty == Ty::Num {
                        let flag = self.builder.use_var(self.integers[src]);
                        self.builder.def_var(self.integers[dst], flag);
                    }
                } else {
                    let value = self.load_register(src);
                    self.store_register(dst, value);
                }
            }
            Action::Step {
                effect,
                objects,
                dst,
                fast,
            } => self.step(position, &before, effect, objects, dst, fast)?,
            Action::Prologue { dst } => {
                self.box_reads(position, &before);
                let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
                self.flush_all(&before, failed);
                let validate = self.builder.ins().iconst(I32, 1);
                let result = self.call_step(pc, validate)?;
                let after = self.after_with_boxed(position, Some(dst))?;
                let next = self.next_pc(position)?;
                let invalidated =
                    self.deopt(next, after.0, after.1, position + 1, reason::SPECULATION);
                let returned = self.builder.create_block();
                let declined = self.builder.create_block();
                let finished =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::Equal, result, outcome::RETURNED as i64);
                self.builder
                    .ins()
                    .brif(finished, returned, &[], declined, &[]);
                self.builder.switch_to_block(returned);
                let value = self.load_register(dst);
                self.builder.ins().return_(&[value]);
                self.builder.switch_to_block(declined);
                let continued = self.builder.create_block();
                self.branch_on_outcome(result, continued, failed, invalidated);
                self.builder.switch_to_block(continued);
                self.forget_slots(&before);
            }
            Action::Fused { dst, ty, reads } => {
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                self.fused(position, &before, (dst, ty), reads, target)?;
            }
            Action::Branch => {
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let label = self.edge(position, target)?;
                self.builder.ins().jump(label, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::BranchUnlessBuiltin { function } => {
                let value = self.load_register(function);
                let kind = self.builder.ins().uload8(I32, flags(), value, KIND);
                let builtin =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::Equal, kind, SexpType::Builtin as i64);
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(builtin, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::BranchUnlessSame { function, expected } => {
                let value = self.load_register(function);
                let same =
                    self.builder
                        .ins()
                        .icmp_imm_u(IntCC::Equal, value, expected.as_ptr() as i64);
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(same, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::BranchIfObject { src } => {
                let value = self.load_register(src);
                let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
                let object = self.builder.ins().band_imm_u(bits, OBJECT_FLAG);
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(object, taken, &[], fall, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::GuardNotObject { src } => {
                let value = self.load_register(src);
                let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
                let object = self.builder.ins().band_imm_u(bits, OBJECT_FLAG);
                let plain = self.builder.ins().icmp_imm_s(IntCC::Equal, object, 0);
                let pc = self.plan.insts[position].pc;
                let failed = self.deopt(pc, before.id, None, position, reason::SPECULATION);
                self.guard(plain, failed);
            }
            Action::BranchUnlessShape { src, layout } => {
                let value = self.load_register(src);
                let usable = self.plain_record(value);
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                let record = self.builder.create_block();
                self.builder.ins().brif(usable, record, &[], taken, &[]);
                self.builder.switch_to_block(record);
                // Only a record has a layout word, so it is read after the kind check. A lazy
                // record has its layout's shape, so the branch ignores the lazy bit; its fields
                // are read through `Intrinsic::Field`, which hands a lazy record off.
                let word = self
                    .builder
                    .ins()
                    .load(I32, flags(), value, RECORD_LAYOUT_OFFSET as i32);
                let id = self.builder.ins().band_imm_u(word, i64::from(!LAZY_RECORD));
                let same = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, id, i64::from(layout));
                self.builder.ins().brif(same, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::BranchUnlessTag { src, tag } => {
                let value = self.load_register(src);
                let found = self
                    .call(
                        runtime::jit_plain_tag as *const () as usize,
                        &[I64],
                        Some(I64),
                        &[value],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                let same = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, found, i64::from(tag));
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(same, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::BranchUnlessLiteral { src, pattern } => {
                self.box_reads(position, &before);
                let value = self.load_register(src);
                let pattern = self.builder.ins().iconst(I64, pattern.as_ptr() as i64);
                let matched = self
                    .call(
                        runtime::jit_literal_matches as *const () as usize,
                        &[I64, I64],
                        Some(I32),
                        &[value, pattern],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(matched, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::Jump => {
                let target = jump_target(inst).ok_or(Refusal("a jump without a target"))?;
                if target <= pc && inst.op == interp::bytecode::Op::Jump {
                    // A raised interrupt flag hands the activation to tier 0 at this jump, whose
                    // poll decides whether to deliver a pending interrupt.
                    let flag = &interp::unwind::INTERRUPT_REQUESTED as *const AtomicBool as i64;
                    let address = self.builder.ins().iconst(I64, flag);
                    let requested = self.builder.ins().uload8(I32, flags(), address, 0);
                    let interrupted = self.deopt(pc, before.id, None, position, reason::INTERRUPT);
                    let calm = self.builder.ins().icmp_imm_s(IntCC::Equal, requested, 0);
                    self.guard(calm, interrupted);
                }
                let label = self.edge(position, target)?;
                self.builder.ins().jump(label, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::JumpUnless { src } => {
                let call = runtime_constant(self.code, inst.operand(1));
                let truth = self.condition(position, &before, src, call)?;
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(truth, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::Arithmetic {
                arith,
                numeric,
                dst,
                left,
                right,
            } => {
                self.arithmetic(position, &before, arith, numeric, dst, left, right)?;
                self.set_visible(&before, true);
            }
            Action::Select {
                dst,
                condition,
                then,
                otherwise,
                ty,
            } => {
                let flag = self.use_register(condition, Ty::Lgl);
                let answered =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::NotEqual, flag, NA_INTEGER as i64);
                self.hand_off_unless(position, &before, answered);
                let then = self.use_register(then, ty);
                let otherwise = self.use_register(otherwise, ty);
                let value = self.builder.ins().select(flag, then, otherwise);
                self.def_register(dst, ty, value);
                self.set_visible(&before, true);
            }
            Action::Comparison {
                arith,
                numeric,
                dst,
                left,
                right,
            } => {
                let result = self.comparison(&before, arith, numeric, left, right);
                self.builder.def_var(self.integers[dst], result);
                self.set_visible(&before, true);
            }
            Action::Intrinsic {
                intrinsic,
                dst,
                args,
                result,
            } => {
                self.intrinsic(position, &before, intrinsic, dst, args, result)?;
                self.set_visible(&before, true);
            }
            Action::Return { src } => {
                let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
                self.flush_all(&before, failed);
                let ty = before.registers[src];
                let value = if ty.is_unboxed() {
                    let unboxed = self.use_register(src, ty);
                    self.box_value(ty, unboxed)
                } else {
                    self.load_register(src)
                };
                self.builder.ins().return_(&[value]);
                return Ok(true);
            }
            Action::ForInit { index, src, refine } => {
                let ty = before.registers[src];
                let zero = self.builder.ins().f64const(0.0);
                self.builder.def_var(self.loop_firsts[index], zero);
                self.builder.def_var(self.loop_steps[index], zero);
                let (sequence, count) = if ty.is_unboxed() {
                    let unboxed = self.use_register(src, ty);
                    let boxed = self.box_value(ty, unboxed);
                    self.store_register(src, boxed);
                    (boxed, self.builder.ins().load(I64, flags(), boxed, LENGTH))
                } else if matches!(ty, Ty::IntSeq | Ty::DblSeq) {
                    let value = self.load_register(src);
                    let (count, first, step) = self.sequence_info(value, ty);
                    let count = self.builder.ins().fcvt_to_sint_sat(I64, count);
                    self.builder.def_var(self.loop_firsts[index], first);
                    self.builder.def_var(self.loop_steps[index], step);
                    (value, count)
                } else {
                    let value = self.load_register(src);
                    match refine {
                        Some(expected) => {
                            // A compact sequence such as `1:n` is ALTREP, which the check
                            // rejects, so the helper prepares it as tier 0 does: it keeps a
                            // compact sequence and stores its first element and increment in
                            // the loop's words, and expands any other ALTREP vector.
                            let fits = self.check(value, expected);
                            let prepare = self.builder.create_block();
                            let join = self.builder.create_block();
                            for ty in [I64, I64, F64, F64] {
                                self.builder.append_block_param(join, ty);
                            }
                            let ordinary = self.builder.create_block();
                            self.builder.ins().brif(fits, ordinary, &[], prepare, &[]);
                            self.builder.switch_to_block(ordinary);
                            let length = self.builder.ins().load(I64, flags(), value, LENGTH);
                            self.builder.ins().jump(join, &[value, length, zero, zero]);
                            self.builder.switch_to_block(prepare);
                            let loop_index = self.builder.ins().iconst(I64, index as i64);
                            let prepared = self
                                .call(
                                    runtime::jit_loop_sequence as *const () as usize,
                                    &[I64, I64, I64],
                                    Some(I64),
                                    &[self.activation, value, loop_index],
                                )
                                .ok_or(Refusal("a helper without a result"))?;
                            let words = (8 * runtime::LOOP_WORDS * index) as i32;
                            let length =
                                self.builder.ins().load(I64, flags(), self.loops, words + 8);
                            let first =
                                self.builder
                                    .ins()
                                    .load(F64, flags(), self.loops, words + 16);
                            let step =
                                self.builder
                                    .ins()
                                    .load(F64, flags(), self.loops, words + 24);
                            let fits = self.check(prepared, expected);
                            let compact = self.compact_sequence_fits(prepared, expected, step);
                            let fits = self.builder.ins().bor(fits, compact);
                            let failed =
                                self.deopt(pc, before.id, None, position, reason::SPECULATION);
                            self.guard(fits, failed);
                            self.store_register(src, prepared);
                            self.builder
                                .ins()
                                .jump(join, &[prepared, length, first, step]);
                            self.builder.switch_to_block(join);
                            let params = self.builder.block_params(join).to_vec();
                            self.builder.def_var(self.loop_firsts[index], params[2]);
                            self.builder.def_var(self.loop_steps[index], params[3]);
                            (params[0], params[1])
                        }
                        None => (value, self.builder.ins().load(I64, flags(), value, LENGTH)),
                    }
                };
                // An empty loop binds its variable to NULL, a type the loop's slot does not
                // take, so tier 0 runs it.
                let none = self.builder.ins().iconst(I64, 0);
                let nonempty = self.builder.ins().icmp(IntCC::NotEqual, count, none);
                let empty = self.deopt(pc, before.id, None, position, reason::HANDOFF);
                self.guard(nonempty, empty);
                // The body must see the sequence as shared, as in the interpreter, so that
                // modifying the variable it came from copies it.
                self.increment(sequence);
                let zero = self.builder.ins().iconst(I64, 0);
                self.builder.def_var(self.loop_counts[index], count);
                self.builder.def_var(self.loop_indexes[index], zero);
            }
            Action::ForStep { index, src, slot } => {
                let current = self.builder.use_var(self.loop_indexes[index]);
                let count = self.builder.use_var(self.loop_counts[index]);
                let done = self
                    .builder
                    .ins()
                    .icmp(IntCC::SignedGreaterThanOrEqual, current, count);
                let target = jump_target(inst).ok_or(Refusal("a loop without an exit"))?;
                let exit = self.edge(position, target)?;
                let body = self.builder.create_block();
                self.builder.ins().brif(done, exit, &[], body, &[]);
                self.fill_edges()?;
                self.builder.switch_to_block(body);
                let sequence = self.load_register(src);
                let element_ty = before.registers[src].element();
                let width = if matches!(element_ty, Ty::Int | Ty::Lgl) {
                    4
                } else {
                    8
                };
                let scaled = self.builder.ins().imul_imm_s(current, width);
                let address = self.builder.ins().iadd(sequence, scaled);
                let next_index = self.builder.ins().iadd_imm_s(current, 1);
                self.builder.def_var(self.loop_indexes[index], next_index);
                match before.registers[src] {
                    Ty::IntVec | Ty::DblVec => {
                        // A compact sequence computes its element from the first element and
                        // the increment; any other vector loads it from its storage.
                        let load_ty = if width == 4 { I32 } else { F64 };
                        let step = self.builder.use_var(self.loop_steps[index]);
                        let compact = {
                            let zero = self.builder.ins().f64const(0.0);
                            self.builder.ins().fcmp(FloatCC::NotEqual, step, zero)
                        };
                        let computed = self.builder.create_block();
                        let stored = self.builder.create_block();
                        let join = self.builder.create_block();
                        self.builder.append_block_param(join, load_ty);
                        self.builder.ins().brif(compact, computed, &[], stored, &[]);
                        self.builder.switch_to_block(computed);
                        let first = self.builder.use_var(self.loop_firsts[index]);
                        let position = self.builder.ins().fcvt_from_sint(F64, current);
                        let offset = self.builder.ins().fmul(position, step);
                        let number = self.builder.ins().fadd(first, offset);
                        let element = if load_ty == I32 {
                            self.builder.ins().fcvt_to_sint_sat(I32, number)
                        } else {
                            number
                        };
                        self.builder.ins().jump(join, &[element]);
                        self.builder.switch_to_block(stored);
                        let element = self.builder.ins().load(load_ty, flags(), address, DATA);
                        self.builder.ins().jump(join, &[element]);
                        self.builder.switch_to_block(join);
                        let element = self.builder.block_params(join)[0];
                        self.assign_slot(&before, slot, element_ty, element);
                    }
                    Ty::LglVec => {
                        let element = self.builder.ins().load(I32, flags(), address, DATA);
                        self.assign_slot(&before, slot, element_ty, element);
                    }
                    Ty::List => {
                        let element = self.builder.ins().load(I64, flags(), address, DATA);
                        self.assign_slot(&before, slot, Ty::Boxed, element);
                    }
                    _ => return Err(Refusal("a loop over a sequence of unknown type")),
                }
                let next = self.next_pc(position)?;
                let label = self.edge(position, next)?;
                self.builder.ins().jump(label, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::ForEnd { src } => {
                let sequence = self.load_register(src);
                let yes = self.builder.ins().iconst(I8, 1);
                self.decrement_if(sequence, yes);
            }
            Action::Visible(flag) => self.set_visible(&before, flag),
            Action::LogicLeft { dst, src, is_and } => {
                let call = runtime_constant(self.code, inst.operand(2));
                let value = self.logical(position, &before, src, call, is_and, false)?;
                let decided =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::Equal, value, if is_and { 0 } else { 1 });
                let decided_block = self.builder.create_block();
                let undecided_block = self.builder.create_block();
                self.builder
                    .ins()
                    .brif(decided, decided_block, &[], undecided_block, &[]);
                self.builder.switch_to_block(decided_block);
                self.builder.def_var(self.integers[dst], value);
                self.set_visible(&before, true);
                let target =
                    jump_target(inst).ok_or(Refusal("a logic operator without a target"))?;
                let label = self.edge(position, target)?;
                self.builder.ins().jump(label, &[]);
                self.fill_edges()?;
                self.builder.switch_to_block(undecided_block);
                self.builder.def_var(self.integers[src], value);
                let next = self.next_pc(position)?;
                let label = self.edge(position, next)?;
                self.builder.ins().jump(label, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::LogicRight {
                dst,
                left,
                right,
                is_and,
            } => {
                if before.registers[left] != Ty::Lgl {
                    return Err(Refusal(
                        "a logic operator whose left operand is not a logical scalar",
                    ));
                }
                let call = runtime_constant(self.code, inst.operand(3));
                let left_value = self.builder.use_var(self.integers[left]);
                let right_value = self.logical(position, &before, right, call, is_and, true)?;
                let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
                let left_na = self.builder.ins().icmp(IntCC::Equal, left_value, na);
                let right_na = self.builder.ins().icmp(IntCC::Equal, right_value, na);
                let any_na = self.builder.ins().bor(left_na, right_na);
                let (decisive, decided_value, otherwise) =
                    if is_and { (0, 0, 1) } else { (1, 1, 0) };
                let decides = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, right_value, decisive);
                let decided_value = self.builder.ins().iconst(I32, decided_value);
                let otherwise = self.builder.ins().iconst(I32, otherwise);
                let undecided = self.builder.ins().select(any_na, na, otherwise);
                let result = self.builder.ins().select(decides, decided_value, undecided);
                self.builder.def_var(self.integers[dst], result);
                self.set_visible(&before, true);
            }
            Action::SubAssign {
                dst,
                slot,
                index,
                value,
                store,
            } => {
                self.sub_assign(
                    position,
                    &before,
                    dst,
                    slot,
                    Subscript::Vector(index),
                    value,
                    store,
                )?;
                self.set_visible(&before, false);
            }
            Action::SubAssignMatrix {
                dst,
                slot,
                row,
                column,
                value,
            } => {
                let subscript = Subscript::Matrix(row, column);
                let store = Store::Checked;
                self.sub_assign(position, &before, dst, slot, subscript, value, store)?;
                self.set_visible(&before, false);
            }
        }
        Ok(false)
    }

    // Steps: instructions tier 0 runs.

    unsafe fn step(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        effect: Effect,
        objects: &[usize],
        dst: Option<(usize, Ty)>,
        fast: Fast,
    ) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        self.box_reads(position, before);
        let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
        // Tier 0's fast handler runs no R code, so where it applies the step needs none of the
        // write-back, validation and reload below, and the compiled op costs no more than
        // tier 0's.
        let refining = self.builder.create_block();
        if fast != Fast::No {
            if let Fast::Store(slot) = fast {
                self.flush_slot(before, slot, failed);
            }
            let [handled, stepped, generic] = self.fast_step(pc, true, failed)?;
            self.builder.switch_to_block(handled);
            // The handler stores into the frame's object in place, so a variable held as that
            // object stays valid, and only an unboxed copy goes stale.
            if let Fast::Store(slot) = fast
                && before.slots[slot].ty.in_variables()
            {
                let zero = self.builder.ins().iconst(I32, 0);
                self.set_status(slot, zero);
            }
            self.builder.ins().jump(refining, &[]);
            self.builder.switch_to_block(stepped);
            if let Fast::Store(slot) = fast {
                let zero = self.builder.ins().iconst(I32, 0);
                self.set_status(slot, zero);
            }
            self.builder.ins().jump(refining, &[]);
            self.builder.switch_to_block(generic);
        }
        let validate = match effect {
            Effect::Pure => self.builder.ins().iconst(I32, 0),
            Effect::Opaque => {
                self.flush_all(before, failed);
                self.builder.ins().iconst(I32, 1)
            }
            Effect::PureUnlessObserved => {
                let mut any = self
                    .call(
                        runtime::jit_handlers_present as *const () as usize,
                        &[],
                        Some(I32),
                        &[],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                for &register in objects {
                    let value = self.load_register(register);
                    let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
                    let observed = self
                        .builder
                        .ins()
                        .band_imm_u(bits, OBJECT_FLAG | ALTREP_FLAG);
                    any = self.builder.ins().bor(any, observed);
                    let kind = self.builder.ins().uload8(I32, flags(), value, KIND);
                    let environment = self.builder.ins().icmp_imm_s(
                        IntCC::Equal,
                        kind,
                        SexpType::Environment as i64,
                    );
                    let environment = self.builder.ins().uextend(I32, environment);
                    any = self.builder.ins().bor(any, environment);
                }
                let dispatching = self.builder.create_block();
                let join = self.builder.create_block();
                self.builder.append_block_param(join, I32);
                let zero = self.builder.ins().iconst(I32, 0);
                self.builder
                    .ins()
                    .brif(any, dispatching, &[], join, &[zero]);
                self.builder.switch_to_block(dispatching);
                self.flush_all(before, failed);
                let one = self.builder.ins().iconst(I32, 1);
                self.builder.ins().jump(join, &[one]);
                self.builder.switch_to_block(join);
                self.builder.block_params(join)[0]
            }
        };
        let result = self.call_step(pc, validate)?;
        let after = self.after_with_boxed(position, dst.map(|(register, _)| register))?;
        let next = self.next_pc(position)?;
        let invalidated = self.deopt(next, after.0, after.1, position + 1, reason::SPECULATION);
        let continued = self.builder.create_block();
        self.branch_on_outcome(result, continued, failed, invalidated);
        self.builder.switch_to_block(continued);
        match effect {
            Effect::Pure => {}
            Effect::Opaque => self.forget_slots(before),
            Effect::PureUnlessObserved => {
                let forgetting = self.builder.create_block();
                let joined = self.builder.create_block();
                self.builder.set_cold_block(forgetting);
                self.builder
                    .ins()
                    .brif(validate, forgetting, &[], joined, &[]);
                self.builder.switch_to_block(forgetting);
                self.forget_slots(before);
                self.builder.ins().jump(joined, &[]);
                self.builder.switch_to_block(joined);
            }
        }
        self.builder.ins().jump(refining, &[]);
        self.builder.switch_to_block(refining);
        if let Some((register, ty)) = dst {
            self.refine(register, ty, next, after, position + 1);
        }
        Ok(())
    }

    /// Runs a fused pass in tier 0. When it computes the expression, its value in `dst` is
    /// checked to have type `ty` and control jumps to `target`; when it declines, control goes
    /// on to the ordinary code. Either way the variables are written back before the pass and
    /// reloaded after it, since it reads them from the frame and may force a promise.
    unsafe fn fused(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        (dst, ty): (usize, Ty),
        reads: &[usize],
        target: usize,
    ) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        self.box_registers(position, reads.iter().copied(), before);
        let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
        self.flush_all(before, failed);
        let pc_value = iconst(&mut self.builder, pc);
        let result = self
            .call(
                runtime::jit_fused as *const () as usize,
                &[I64, I64],
                Some(I32),
                &[self.activation, pc_value],
            )
            .ok_or(Refusal("a helper without a result"))?;
        self.forget_slots(before);
        let taken_state = self
            .plan
            .edges(position)
            .iter()
            .find(|(pc, _)| *pc == target)
            .map(|&(_, state)| state)
            .ok_or(Refusal("an edge the analysis did not see"))?;
        let fused = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, result, outcome::FUSED as i64);
        let taken = self.builder.create_block();
        let declined = self.builder.create_block();
        self.builder.ins().brif(fused, taken, &[], declined, &[]);

        self.builder.switch_to_block(taken);
        if !matches!(ty, Ty::Boxed | Ty::Bottom) {
            let value = self.load_register(dst);
            let fits = self.check(value, ty);
            let mismatch = self.deopt(
                target,
                taken_state,
                Some(dst),
                position,
                reason::SPECULATION,
            );
            self.guard(fits, mismatch);
            if ty.is_unboxed() {
                let unboxed = self.unbox(value, ty);
                self.def_register(dst, ty, unboxed);
            }
        }
        let label = self.edge(position, target)?;
        self.builder.ins().jump(label, &[]);

        self.builder.switch_to_block(declined);
        // A pass that computed the expression and then found the code invalid resumes tier 0 at
        // the target, where the expression's value already is.
        let stale =
            self.builder
                .ins()
                .icmp_imm_s(IntCC::Equal, result, outcome::FUSED_INVALIDATED as i64);
        let resume = self.deopt(
            target,
            taken_state,
            Some(dst),
            position,
            reason::SPECULATION,
        );
        let ordinary = self.builder.create_block();
        self.builder.ins().brif(stale, resume, &[], ordinary, &[]);
        self.builder.switch_to_block(ordinary);
        let after = self.after_with_boxed(position, None)?;
        let next = self.next_pc(position)?;
        let invalidated = self.deopt(next, after.0, after.1, position + 1, reason::SPECULATION);
        let continued = self.builder.create_block();
        self.branch_on_outcome(result, continued, failed, invalidated);
        self.fill_edges()?;
        self.builder.switch_to_block(continued);
        Ok(())
    }

    /// Calls a closure with arguments evaluated in compiled code. The callee may reach this
    /// frame, so the variables are written back before the call and forgotten after it.
    #[allow(clippy::too_many_arguments)]
    unsafe fn direct_call(
        &mut self,
        position: usize,
        before: StateRef<'p>,
        steps: &'p [(Action<'p>, StateId)],
        function: Sexp,
        call: Sexp,
        args: &[Argument],
        entry: annotations::Entry,
        dst: (usize, Ty),
    ) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let after = self.after_with_boxed(position, Some(dst.0))?;
        let next = self.next_pc(position)?;
        let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
        let invalidated = self.deopt(next, after.0, after.1, position + 1, reason::SPECULATION);
        let continued = self.builder.create_block();
        // A calling handler could run R code during the callee, before it forces an argument
        // that this call evaluates early, so then tier 0's instruction makes the call lazily.
        // Code running under a test harness always has handlers, which is why this stays in
        // compiled code rather than leaving for tier 0.
        let handlers = self
            .call(
                runtime::jit_handlers_present as *const () as usize,
                &[],
                Some(I32),
                &[],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let observed = self.builder.create_block();
        let unobserved = self.builder.create_block();
        self.builder
            .ins()
            .brif(handlers, observed, &[], unobserved, &[]);
        self.builder.switch_to_block(observed);
        self.box_reads(position, &before);
        self.flush_all(&before, failed);
        let validate = self.builder.ins().iconst(I32, 1);
        let result = self.call_step(pc, validate)?;
        self.branch_on_outcome(result, continued, failed, invalidated);
        self.builder.switch_to_block(unobserved);
        for &(step, state) in steps {
            self.action(position, self.plan.state(state), step)?;
        }
        self.flush_all(&before, failed);
        let buffer = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (16 * args.len().max(1)) as u32,
            3,
        ));
        for (index, argument) in args.iter().enumerate() {
            let (kind, bits) = match *argument {
                Argument::Slot(slot) => (representation::BOXED, self.load_slot_cell(slot)),
                Argument::Unboxed { register, ty } => {
                    let value = self.use_register(register, ty);
                    let (kind, bits) = match ty {
                        Ty::Dbl => (
                            representation::DOUBLE,
                            self.builder.ins().bitcast(I64, MemFlagsData::new(), value),
                        ),
                        Ty::Int => (
                            representation::INTEGER,
                            self.builder.ins().sextend(I64, value),
                        ),
                        _ => (
                            representation::LOGICAL,
                            self.builder.ins().sextend(I64, value),
                        ),
                    };
                    (kind, bits)
                }
            };
            let kind = self.builder.ins().iconst(I64, kind as i64);
            self.builder
                .ins()
                .stack_store(I64, kind, buffer, (16 * index) as i32);
            self.builder
                .ins()
                .stack_store(I64, bits, buffer, (16 * index + 8) as i32);
        }
        let pointer = self.builder.ins().stack_addr(I64, buffer, 0);
        let function = self.builder.ins().iconst(I64, function.as_ptr() as i64);
        let call = self.builder.ins().iconst(I64, call.as_ptr() as i64);
        let count = iconst(&mut self.builder, args.len());
        let register = iconst(&mut self.builder, dst.0);
        let signature = match entry {
            annotations::Entry::Unchecked => std::ptr::null_mut(),
            annotations::Entry::Checked(None) => values::nil().as_ptr(),
            annotations::Entry::Checked(Some(signature)) => signature.as_ptr(),
        };
        let signature = self.builder.ins().iconst(I64, signature as i64);
        let result = self
            .call(
                runtime::jit_call as *const () as usize,
                &[I64, I64, I64, I64, I64, I64, I64],
                Some(I32),
                &[
                    self.activation,
                    function,
                    call,
                    pointer,
                    count,
                    signature,
                    register,
                ],
            )
            .ok_or(Refusal("a helper without a result"))?;
        self.branch_on_outcome(result, continued, failed, invalidated);
        self.builder.switch_to_block(continued);
        self.forget_slots(&before);
        self.refine(dst.0, dst.1, next, after, position + 1);
        Ok(())
    }

    /// Continues at `continued` when a helper's `result` is [`outcome::OK`], and otherwise
    /// deoptimizes to `failed` after an error or to `invalidated` after the code was invalidated.
    fn branch_on_outcome(
        &mut self,
        result: Value,
        continued: Label,
        failed: Label,
        invalidated: Label,
    ) {
        let ok = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, result, outcome::OK as i64);
        let problem = self.builder.create_block();
        self.builder.ins().brif(ok, continued, &[], problem, &[]);
        self.builder.switch_to_block(problem);
        self.builder.set_cold_block(problem);
        let errored = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, result, outcome::FAILED as i64);
        self.builder
            .ins()
            .brif(errored, failed, &[], invalidated, &[]);
    }

    /// Checks that the value an instruction left in `register` has type `ty` and unboxes it,
    /// deoptimizing to `next` otherwise.
    unsafe fn refine(
        &mut self,
        register: usize,
        ty: Ty,
        next: usize,
        after: (StateId, Option<usize>),
        live: usize,
    ) {
        // A closure an instruction just made has its type by construction.
        if matches!(ty, Ty::Boxed | Ty::Bottom | Ty::Closure(..)) {
            return;
        }
        let value = self.load_register(register);
        let fits = self.check(value, ty);
        let failed = self.deopt(next, after.0, after.1, live, reason::SPECULATION);
        self.guard(fits, failed);
        if ty.is_unboxed() {
            let unboxed = self.unbox(value, ty);
            self.def_register(register, ty, unboxed);
        }
    }

    /// Boxes the unboxed registers the instruction at `position` reads, so tier 0 finds them.
    unsafe fn box_reads(&mut self, position: usize, before: &StateRef<'p>) {
        self.box_registers(
            position,
            analysis::reads(&self.plan.insts[position]),
            before,
        );
    }

    /// Where tier 0 would find the object for the unboxed `register` at `position`: the
    /// constant pool's object or a variable's frame cell, when an earlier instruction of the
    /// same block loaded the register from there and the instructions since write only other
    /// registers. A step between them may rebind the variable, but then it also forgets the
    /// variable, which the run-time status check in [`Self::box_for_tier0`] sees.
    fn known_object(&self, position: usize, register: usize) -> Option<Known> {
        // A syntax-function or dispatch check that always passes ends a block, as `x[i]` ends
        // one between reading `x` and reading `i`; the block after it is still reached only
        // from that check, so the search continues into the block before.
        let mut start = self.block_start;
        while let Some(block) = self
            .plan
            .insts
            .get(start)
            .and_then(|inst| self.plan.block_at(inst.pc))
            && self.plan.continues(block)
            && matches!(self.plan.actions.get(start.wrapping_sub(1)), Some(Action::FallThrough))
            && let Some(previous) = block.checked_sub(1).and_then(|previous| self.plan.blocks.get(previous))
        {
            start = previous.start;
        }
        for earlier in (start..position).rev() {
            match *self.plan.actions.get(earlier)? {
                Action::Constant { dst, value, .. } if dst == register => {
                    return Some(Known::Constant(value));
                }
                Action::GetVar { dst, slot, .. } if dst == register => {
                    return Some(Known::Slot(slot));
                }
                Action::Step {
                    dst: Some((dst, _)),
                    ..
                }
                | Action::GetFun { dst, .. }
                    if dst == register =>
                {
                    return None;
                }
                Action::Constant { .. }
                | Action::GetVar { forcing: false, .. }
                | Action::GetFun { .. }
                | Action::Step { dst: Some(_), .. }
                | Action::Srcref(_)
                | Action::FallThrough
                | Action::Skip => {}
                _ => return None,
            }
        }
        None
    }

    /// The boxed form of the unboxed `register` for tier 0. A constant is the pool's own object,
    /// and a variable whose frame cell holds the object it was unboxed from reuses that object,
    /// so neither allocates.
    unsafe fn box_for_tier0(
        &mut self,
        position: usize,
        register: usize,
        ty: Ty,
        before: &StateRef<'p>,
    ) -> Value {
        let slot = match self.known_object(position, register) {
            Some(Known::Constant(constant)) => {
                return self.builder.ins().iconst(I64, constant.as_ptr() as i64);
            }
            Some(Known::Slot(slot)) if before.slots[slot].may_be(status::LOCAL) => slot,
            _ => {
                let value = self.use_register(register, ty);
                return self.box_value(ty, value);
            }
        };
        // A variable compiled code assigned is written back first, once, so this and every
        // later boxing of it reuse the frame's object instead of allocating.
        let current = before.slots[slot];
        if current.may_be(status::DIRTY) && current.ty.in_variables() {
            let pc = self.plan.insts[position].pc;
            let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
            let status = self.status(slot);
            let dirty = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
            let flush = self.builder.create_block();
            let flushed = self.builder.create_block();
            self.builder.set_cold_block(flush);
            self.builder.ins().brif(dirty, flush, &[], flushed, &[]);
            self.builder.switch_to_block(flush);
            let result = self.call_flush(slot, current.ty);
            let ok = self.builder.ins().icmp_imm_s(IntCC::NotEqual, result, 0);
            self.guard(ok, failed);
            let clean = self.builder.ins().iconst(I32, status::RUNTIME_LOCAL);
            self.set_status(slot, clean);
            self.builder.ins().jump(flushed, &[]);
            self.builder.switch_to_block(flushed);
        }
        // A clean variable's cell holds the object it was unboxed from, unless that is a
        // promise, whose value is what the register holds.
        let kind = match ty {
            Ty::Int => SexpType::Integer,
            Ty::Dbl => SexpType::Real,
            _ => SexpType::Logical,
        };
        let cell = self.load_slot_cell(slot);
        let status = self.status(slot);
        let local = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_LOCAL);
        let held = self.builder.ins().uload8(I32, flags(), cell, KIND);
        let same_kind = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, held, kind as i64);
        let reusable = self.builder.ins().band(local, same_kind);
        let boxing = self.builder.create_block();
        let join = self.builder.create_block();
        self.builder.append_block_param(join, I64);
        self.builder.set_cold_block(boxing);
        self.builder
            .ins()
            .brif(reusable, join, &[cell], boxing, &[]);
        self.builder.switch_to_block(boxing);
        let value = self.use_register(register, ty);
        let boxed = self.box_value(ty, value);
        self.builder.ins().jump(join, &[boxed]);
        self.builder.switch_to_block(join);
        self.builder.block_params(join)[0]
    }

    /// Writes the unboxed values of `registers` to the window, for tier 0 to read.
    unsafe fn box_registers(
        &mut self,
        position: usize,
        registers: impl IntoIterator<Item = usize>,
        before: &StateRef<'p>,
    ) {
        for register in registers {
            let ty = before
                .registers
                .get(register)
                .copied()
                .unwrap_or(Ty::Bottom);
            if !ty.is_unboxed() {
                continue;
            }
            let boxed = self.box_for_tier0(position, register, ty, before);
            self.store_register(register, boxed);
        }
    }

    /// Leaves for tier 0 at the instruction, the slow path of an inline fast path. Tier 0 then
    /// runs it with the frame written back, so a warning handler sees every variable, and
    /// re-enters compiled code at the next loop back edge.
    fn hand_off(&mut self, position: usize, before: &StateRef<'p>) {
        let pc = self.plan.insts[position].pc;
        let handoff = self.deopt(pc, before.id, None, position, reason::HANDOFF);
        self.builder.ins().jump(handoff, &[]);
    }

    /// The loop of an [`Action::Traverse`]. A list being built lives in the register window at
    /// `dst`, which roots it while the loop allocates, and the helpers that fill it read it
    /// from there; tier 0 overwrites that register when it runs the call after a hand-off.
    unsafe fn traverse(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        action: Action<'p>,
    ) -> Result<(), Refusal> {
        let Action::Traverse {
            traversal,
            collection,
            start,
            accumulator,
            expected,
            items,
            parts,
            prelude,
            steps,
            result: (result, result_ty),
            broke,
            dst,
        } = action
        else {
            return Err(Refusal("a traversal without its loop"));
        };
        if let Some((callback, function)) = expected {
            let held = self.load_register(callback);
            let same = self
                .builder
                .ins()
                .icmp_imm_u(IntCC::Equal, held, function.as_ptr() as i64);
            self.hand_off_unless(position, before, same);
        }
        for &(step, state) in prelude {
            self.action(position, self.plan.state(state), step)?;
        }
        let object = self.load_register(collection);
        let (length, keys, stored) = match parts {
            // The library reads any list without an Array bit or ALTREP methods the same way,
            // its attributes included, so only those two are checked.
            None => {
                let kind = self.builder.ins().uload8(I32, flags(), object, KIND);
                let is_list =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::Equal, kind, SexpType::List as i64);
                let bits = self.builder.ins().uload8(I32, flags(), object, FLAGS);
                let special = self
                    .builder
                    .ins()
                    .band_imm_u(bits, ALTREP_FLAG | ARRAY_FLAG);
                let ordinary = self.builder.ins().icmp_imm_s(IntCC::Equal, special, 0);
                let plain = self.builder.ins().band(is_list, ordinary);
                self.hand_off_unless(position, before, plain);
                let length = self.builder.ins().load(I64, flags(), object, LENGTH);
                (length, object, object)
            }
            Some((keys, stored)) => {
                let dict = self
                    .builder
                    .ins()
                    .iconst(I32, i64::from(traversal.source == analysis::Source::Dict));
                let key = match items[0] {
                    Some((_, Ty::Int)) => SexpType::Integer as i64,
                    Some((_, Ty::Dbl)) => SexpType::Real as i64,
                    Some((_, Ty::Lgl)) => SexpType::Logical as i64,
                    _ => -1,
                };
                let key = self.builder.ins().iconst(I32, key);
                let keys_register = iconst(&mut self.builder, keys);
                let stored_register = iconst(&mut self.builder, stored);
                let length = self
                    .call(
                        runtime::jit_collection_parts as *const () as usize,
                        &[I64, I64, I32, I32, I64, I64],
                        Some(I64),
                        &[
                            self.activation,
                            object,
                            dict,
                            key,
                            keys_register,
                            stored_register,
                        ],
                    )
                    .ok_or(Refusal("the collection helper returned nothing"))?;
                let found =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, length, 0);
                self.hand_off_unless(position, before, found);
                let keys = self.load_register(keys);
                let stored = self.load_register(stored);
                (length, keys, stored)
            }
        };
        if let Some((initial, register)) = accumulator {
            let ty = before.registers[initial];
            let value = self.use_register(initial, ty);
            self.def_register(register, ty, value);
        }
        let first = match start {
            None if traversal.backwards => self.builder.ins().iadd_imm_s(length, -1),
            None => self.builder.ins().iconst(I64, 0),
            // `List.walk_from` takes a whole position of 1 or more; tier 0 raises the library's
            // error for anything else.
            Some((register, ty)) => {
                let value = self.use_register(register, ty);
                let whole = match ty {
                    Ty::Dbl => {
                        let whole = self.builder.ins().fcvt_to_sint_sat(I64, value);
                        let back = self.builder.ins().fcvt_from_sint(F64, whole);
                        let exact = self.builder.ins().fcmp(FloatCC::Equal, back, value);
                        self.hand_off_unless(position, before, exact);
                        // Tier 0 reads the position as an R integer, so a larger one is an error.
                        let fits = self.builder.ins().icmp_imm_s(
                            IntCC::SignedLessThanOrEqual,
                            whole,
                            i32::MAX as i64,
                        );
                        self.hand_off_unless(position, before, fits);
                        whole
                    }
                    _ => self.builder.ins().sextend(I64, value),
                };
                let positive =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, whole, 1);
                self.hand_off_unless(position, before, positive);
                self.builder.ins().iadd_imm_s(whole, -1)
            }
        };
        let destination = iconst(&mut self.builder, dst);
        if matches!(traversal.fold, Fold::Map | Fold::Filter(_)) {
            let made = self
                .call(
                    runtime::jit_list_new as *const () as usize,
                    &[I64, I64, I64],
                    Some(I32),
                    &[self.activation, length, destination],
                )
                .ok_or(Refusal("the list helper returned nothing"))?;
            let made = self.builder.ins().icmp_imm_s(IntCC::NotEqual, made, 0);
            self.hand_off_unless(position, before, made);
        }
        let at_var = self.builder.declare_var(I64);
        let count = self.builder.declare_var(I64);
        let answer = self.builder.declare_var(I32);
        let found = self.builder.declare_var(I64);
        self.builder.def_var(at_var, first);
        let zero = self.builder.ins().iconst(I64, 0);
        self.builder.def_var(count, zero);
        let none = self.builder.ins().iconst(I64, -1);
        self.builder.def_var(found, none);
        let initial_answer = self
            .builder
            .ins()
            .iconst(I32, i64::from(traversal.fold == Fold::All));
        self.builder.def_var(answer, initial_answer);
        let header = self.builder.create_block();
        let body = self.builder.create_block();
        let next = self.builder.create_block();
        let exit = self.builder.create_block();
        self.builder.ins().jump(header, &[]);
        self.builder.switch_to_block(header);
        let at = self.builder.use_var(at_var);
        let more = if traversal.backwards {
            self.builder
                .ins()
                .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, at, 0)
        } else {
            self.builder.ins().icmp(IntCC::SignedLessThan, at, length)
        };
        self.builder.ins().brif(more, body, &[], exit, &[]);
        self.builder.switch_to_block(body);
        let offset = self.builder.ins().imul_imm_s(at, 8);
        // A list's element, or a dictionary's value: either way a boxed item of a plain list.
        let item = match parts {
            None => {
                let address = self.builder.ins().iadd(keys, offset);
                Some(self.builder.ins().load(I64, flags(), address, DATA))
            }
            Some(_) if traversal.source == analysis::Source::Dict && items[1].is_some() => {
                let address = self.builder.ins().iadd(stored, offset);
                Some(self.builder.ins().load(I64, flags(), address, DATA))
            }
            Some(_) => None,
        };
        match parts {
            None => {
                if let (Some((register, ty)), Some(item)) = (items[0], item) {
                    self.unbox_item(position, before, item, register, ty);
                }
            }
            Some(_) => {
                // The helper checked the keys vector has the key's type.
                if let Some((register, ty)) = items[0] {
                    let key = match ty {
                        Ty::Dbl => {
                            let address = self.builder.ins().iadd(keys, offset);
                            self.builder.ins().load(F64, flags(), address, DATA)
                        }
                        _ => {
                            let offset = self.builder.ins().imul_imm_s(at, 4);
                            let address = self.builder.ins().iadd(keys, offset);
                            self.builder.ins().load(I32, flags(), address, DATA)
                        }
                    };
                    self.def_register(register, ty, key);
                }
                if let (Some((register, ty)), Some(item)) = (items[1], item) {
                    self.unbox_item(position, before, item, register, ty);
                }
            }
        }
        for &(step, state) in steps {
            self.action(position, self.plan.state(state), step)?;
        }
        let value = self.use_register(result, result_ty);
        if traversal.is_predicate() {
            // `NA` is not an answer: tier 0 raises the library's error for it.
            let answered = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::NotEqual, value, NA_INTEGER as i64);
            self.hand_off_unless(position, before, answered);
        }
        match traversal.fold {
            Fold::Walk { .. } => {
                if let Some((_, register)) = accumulator {
                    self.def_register(register, result_ty, value);
                }
                match broke {
                    Some(broke) => {
                        let flag = self.use_register(broke, Ty::Lgl);
                        self.builder.ins().brif(flag, exit, &[], next, &[]);
                    }
                    None => {
                        self.builder.ins().jump(next, &[]);
                    }
                }
            }
            Fold::Map => {
                let (kind, bits) = match result_ty {
                    Ty::Dbl => (
                        representation::DOUBLE,
                        self.builder.ins().bitcast(I64, MemFlagsData::new(), value),
                    ),
                    Ty::Int => (
                        representation::INTEGER,
                        self.builder.ins().sextend(I64, value),
                    ),
                    _ => (
                        representation::LOGICAL,
                        self.builder.ins().sextend(I64, value),
                    ),
                };
                self.store_item(position, before, destination, at, kind, bits)?;
                self.builder.ins().jump(next, &[]);
            }
            Fold::Filter(keep) => {
                let item = item.ok_or(Refusal("a filter without its element"))?;
                let store = self.builder.create_block();
                let condition = if keep { IntCC::NotEqual } else { IntCC::Equal };
                let passes = self.builder.ins().icmp_imm_s(condition, value, 0);
                self.builder.ins().brif(passes, store, &[], next, &[]);
                self.builder.switch_to_block(store);
                let kept = self.builder.use_var(count);
                self.store_item(
                    position,
                    before,
                    destination,
                    kept,
                    representation::BOXED,
                    item,
                )?;
                let kept = self.builder.ins().iadd_imm_s(kept, 1);
                self.builder.def_var(count, kept);
                self.builder.ins().jump(next, &[]);
            }
            Fold::Any | Fold::All | Fold::Find { .. } => {
                let decided = self.builder.create_block();
                let stops = if traversal.fold == Fold::All {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let stop = self.builder.ins().icmp_imm_s(stops, value, 0);
                self.builder.ins().brif(stop, decided, &[], next, &[]);
                self.builder.switch_to_block(decided);
                let flipped = self
                    .builder
                    .ins()
                    .iconst(I32, i64::from(traversal.fold != Fold::All));
                self.builder.def_var(answer, flipped);
                self.builder.def_var(found, at);
                self.builder.ins().jump(exit, &[]);
            }
            Fold::CountIf => {
                let passed = self.builder.ins().sextend(I64, value);
                let counted = self.builder.use_var(count);
                let counted = self.builder.ins().iadd(counted, passed);
                self.builder.def_var(count, counted);
                self.builder.ins().jump(next, &[]);
            }
        }
        self.builder.switch_to_block(next);
        let at = self.builder.use_var(at_var);
        let step = if traversal.backwards { -1 } else { 1 };
        let following = self.builder.ins().iadd_imm_s(at, step);
        self.builder.def_var(at_var, following);
        self.builder.ins().jump(header, &[]);
        self.builder.switch_to_block(exit);
        match traversal.fold {
            Fold::Walk { .. } => {
                if let Some((_, register)) = accumulator {
                    let value = self.use_register(register, result_ty);
                    self.def_register(dst, result_ty, value);
                }
            }
            Fold::Map => {}
            Fold::Filter(_) => {
                let kept = self.builder.use_var(count);
                let done = self
                    .call(
                        runtime::jit_list_prefix as *const () as usize,
                        &[I64, I64, I64],
                        Some(I32),
                        &[self.activation, destination, kept],
                    )
                    .ok_or(Refusal("the list helper returned nothing"))?;
                let done = self.builder.ins().icmp_imm_s(IntCC::NotEqual, done, 0);
                self.hand_off_unless(position, before, done);
            }
            Fold::Any | Fold::All => {
                let value = self.builder.use_var(answer);
                self.def_register(dst, Ty::Lgl, value);
            }
            Fold::CountIf => {
                let counted = self.builder.use_var(count);
                let done = self
                    .call(
                        runtime::jit_count as *const () as usize,
                        &[I64, I64, I64],
                        Some(I32),
                        &[self.activation, destination, counted],
                    )
                    .ok_or(Refusal("the count helper returned nothing"))?;
                let done = self.builder.ins().icmp_imm_s(IntCC::NotEqual, done, 0);
                self.hand_off_unless(position, before, done);
            }
            Fold::Find { index } => {
                let list = iconst(&mut self.builder, collection);
                let at = self.builder.use_var(found);
                let index = self.builder.ins().iconst(I32, i64::from(index));
                let done = self
                    .call(
                        runtime::jit_found as *const () as usize,
                        &[I64, I64, I64, I64, I32],
                        Some(I32),
                        &[self.activation, destination, list, at, index],
                    )
                    .ok_or(Refusal("the find helper returned nothing"))?;
                let done = self.builder.ins().icmp_imm_s(IntCC::NotEqual, done, 0);
                self.hand_off_unless(position, before, done);
            }
        }
        Ok(())
    }

    /// Checks a boxed item a traversal reads has the type its callback expects, handing the
    /// call to tier 0 when it does not, and unboxes it into `register`.
    fn unbox_item(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        item: Value,
        register: usize,
        ty: Ty,
    ) {
        let fits = self.check(item, ty);
        self.hand_off_unless(position, before, fits);
        let unboxed = self.unbox(item, ty);
        self.def_register(register, ty, unboxed);
    }

    /// Stores an item into the list a traversal builds in register `destination`, handing the
    /// call to tier 0 if boxing it fails.
    fn store_item(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        destination: Value,
        index: Value,
        kind: i32,
        bits: Value,
    ) -> Result<(), Refusal> {
        let kind = self.builder.ins().iconst(I32, i64::from(kind));
        let stored = self
            .call(
                runtime::jit_list_store as *const () as usize,
                &[I64, I64, I64, I32, I64],
                Some(I32),
                &[self.activation, destination, index, kind, bits],
            )
            .ok_or(Refusal("the list helper returned nothing"))?;
        let stored = self.builder.ins().icmp_imm_s(IntCC::NotEqual, stored, 0);
        self.hand_off_unless(position, before, stored);
        Ok(())
    }

    // Arithmetic.

    #[allow(clippy::too_many_arguments)]
    unsafe fn arithmetic(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        arith: Arith,
        numeric: Numeric,
        dst: usize,
        left: usize,
        right: usize,
    ) -> Result<(), Refusal> {
        let (left_ty, right_ty) = (before.registers[left], before.registers[right]);
        match numeric {
            Numeric::Int => {
                let a = self.use_register(left, left_ty);
                let b = self.use_register(right, right_ty);
                let (value, overflow) = match arith {
                    Arith::Add => self.builder.ins().sadd_overflow(a, b),
                    Arith::Subtract => self.builder.ins().ssub_overflow(a, b),
                    Arith::Multiply => self.builder.ins().smul_overflow(a, b),
                    _ => return Err(Refusal("integer arithmetic the JIT does not inline")),
                };
                let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
                let a_na = self.builder.ins().icmp(IntCC::Equal, a, na);
                let b_na = self.builder.ins().icmp(IntCC::Equal, b, na);
                let any_na = self.builder.ins().bor(a_na, b_na);
                let result_na = self.builder.ins().icmp(IntCC::Equal, value, na);
                let bad = self.builder.ins().bor(overflow, result_na);
                // An `NA` operand gives `NA`; otherwise overflow takes the slow path, which warns.
                let not_na = self.builder.ins().bxor_imm_u(any_na, 1);
                let slow = self.builder.ins().band(bad, not_na);
                let value = self.builder.ins().select(any_na, na, value);
                self.builder.def_var(self.integers[dst], value);
                let slow_block = self.builder.create_block();
                let join = self.builder.create_block();
                self.builder.ins().brif(slow, slow_block, &[], join, &[]);
                self.builder.switch_to_block(slow_block);
                self.builder.set_cold_block(slow_block);
                self.hand_off(position, before);
                self.builder.switch_to_block(join);
            }
            Numeric::Dbl => {
                let a = self.as_double(left, left_ty);
                let b = self.as_double(right, right_ty);
                let value = match arith {
                    Arith::Add => self.builder.ins().fadd(a, b),
                    Arith::Subtract => self.builder.ins().fsub(a, b),
                    Arith::Multiply => self.builder.ins().fmul(a, b),
                    Arith::Divide => self.builder.ins().fdiv(a, b),
                    Arith::Power => self
                        .call(
                            runtime::jit_power as *const () as usize,
                            &[F64, F64],
                            Some(F64),
                            &[a, b],
                        )
                        .ok_or(Refusal("a helper without a result"))?,
                    _ => return Err(Refusal("double arithmetic the JIT does not inline")),
                };
                self.builder.def_var(self.doubles[dst], value);
            }
        }
        Ok(())
    }

    unsafe fn comparison(
        &mut self,
        before: &StateRef<'p>,
        arith: Arith,
        numeric: Numeric,
        left: usize,
        right: usize,
    ) -> Value {
        let (left_ty, right_ty) = (before.registers[left], before.registers[right]);
        let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
        let (flag, missing) = match numeric {
            Numeric::Int => {
                let a = self.use_register(left, left_ty);
                let b = self.use_register(right, right_ty);
                let condition = match arith {
                    Arith::Equal => IntCC::Equal,
                    Arith::NotEqual => IntCC::NotEqual,
                    Arith::Less => IntCC::SignedLessThan,
                    Arith::Greater => IntCC::SignedGreaterThan,
                    Arith::LessEqual => IntCC::SignedLessThanOrEqual,
                    _ => IntCC::SignedGreaterThanOrEqual,
                };
                let flag = self.builder.ins().icmp(condition, a, b);
                let a_na = self.builder.ins().icmp(IntCC::Equal, a, na);
                let b_na = self.builder.ins().icmp(IntCC::Equal, b, na);
                (flag, self.builder.ins().bor(a_na, b_na))
            }
            Numeric::Dbl => {
                let a = self.as_double(left, left_ty);
                let b = self.as_double(right, right_ty);
                let condition = match arith {
                    Arith::Equal => FloatCC::Equal,
                    Arith::NotEqual => FloatCC::OrderedNotEqual,
                    Arith::Less => FloatCC::LessThan,
                    Arith::Greater => FloatCC::GreaterThan,
                    Arith::LessEqual => FloatCC::LessThanOrEqual,
                    _ => FloatCC::GreaterThanOrEqual,
                };
                let flag = self.builder.ins().fcmp(condition, a, b);
                (flag, self.builder.ins().fcmp(FloatCC::Unordered, a, b))
            }
        };
        let flag = self.builder.ins().uextend(I32, flag);
        self.builder.ins().select(missing, na, flag)
    }

    unsafe fn intrinsic(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        intrinsic: Intrinsic,
        dst: usize,
        args: &[usize],
        result: Ty,
    ) -> Result<(), Refusal> {
        let types: Vec<Ty> = args
            .iter()
            .map(|&register| before.registers[register])
            .collect();
        let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
        match intrinsic {
            Intrinsic::Element => {
                let vector = self.load_register(args[0]);
                let sequence = matches!(types[0], Ty::IntSeq | Ty::DblSeq);
                let (length, first, step) = if sequence {
                    let (length, first, step) =
                        self.sequence_info(vector, types[0]);
                    (Length::Double(length), Some(first), Some(step))
                } else {
                    let length = self.builder.ins().load(I64, flags(), vector, LENGTH);
                    (Length::Integer(length), None, None)
                };
                let (index, in_range) = match types[1] {
                    Ty::Int => {
                        let index = self.use_register(args[1], Ty::Int);
                        let index = self.builder.ins().sextend(I64, index);
                        let positive = self.builder.ins().icmp_imm_s(
                            IntCC::SignedGreaterThanOrEqual,
                            index,
                            1,
                        );
                        let within = match length {
                            Length::Integer(length) => {
                                self.builder
                                    .ins()
                                    .icmp(IntCC::SignedLessThanOrEqual, index, length)
                            }
                            Length::Double(length) => {
                                let position = self.builder.ins().fcvt_from_sint(F64, index);
                                self.builder
                                    .ins()
                                    .fcmp(FloatCC::LessThanOrEqual, position, length)
                            }
                        };
                        (index, self.builder.ins().band(positive, within))
                    }
                    _ => {
                        let index = self.use_register(args[1], Ty::Dbl);
                        let one = self.builder.ins().f64const(1.0);
                        let positive =
                            self.builder
                                .ins()
                                .fcmp(FloatCC::GreaterThanOrEqual, index, one);
                        let within = match length {
                            Length::Integer(length) => {
                                self.within_length(index, length, types[0].is_array())
                            }
                            Length::Double(length) => {
                                let one = self.builder.ins().f64const(1.0);
                                let bound = self.builder.ins().fadd(length, one);
                                self.builder.ins().fcmp(FloatCC::LessThan, index, bound)
                            }
                        };
                        let in_range = self.builder.ins().band(positive, within);
                        // Only converted where it is in range, so the conversion cannot trap.
                        let safe = self.builder.ins().select(in_range, index, one);
                        (self.builder.ins().fcvt_to_sint(I64, safe), in_range)
                    }
                };
                let fast = self.builder.create_block();
                let slow = self.builder.create_block();
                let join = self.builder.create_block();
                self.builder.ins().brif(in_range, fast, &[], slow, &[]);
                self.builder.switch_to_block(fast);
                let list = types[0] == Ty::List;
                let width = if !list && matches!(result, Ty::Int | Ty::Lgl) {
                    4
                } else {
                    8
                };
                let offset = self.builder.ins().iadd_imm_s(index, -1);
                let scaled = self.builder.ins().imul_imm_s(offset, width);
                let address = self.builder.ins().iadd(vector, scaled);
                match result {
                    Ty::Int if let (Some(first), Some(step)) = (first, step) => {
                        let position = self.builder.ins().fcvt_from_sint(F64, offset);
                        let distance = self.builder.ins().fmul(position, step);
                        let number = self.builder.ins().fadd(first, distance);
                        let value = self.builder.ins().fcvt_to_sint_sat(I32, number);
                        self.builder.def_var(self.integers[dst], value);
                    }
                    Ty::Dbl if let (Some(first), Some(step)) = (first, step) => {
                        let position = self.builder.ins().fcvt_from_sint(F64, offset);
                        let distance = self.builder.ins().fmul(position, step);
                        let value = self.builder.ins().fadd(first, distance);
                        self.builder.def_var(self.doubles[dst], value);
                    }
                    // A list element speculated to be a scalar is checked, then unboxed; any
                    // other element hands the read to tier 0.
                    Ty::Int | Ty::Lgl | Ty::Dbl if list => {
                        let element = self.builder.ins().load(I64, flags(), address, DATA);
                        let fits = self.check(element, result);
                        self.hand_off_unless(position, before, fits);
                        let value = self.unbox(element, result);
                        self.def_register(dst, result, value);
                    }
                    Ty::Int | Ty::Lgl => {
                        let value = self.builder.ins().load(I32, flags(), address, DATA);
                        self.builder.def_var(self.integers[dst], value);
                    }
                    Ty::Dbl => {
                        let value = self.builder.ins().load(F64, flags(), address, DATA);
                        self.builder.def_var(self.doubles[dst], value);
                    }
                    _ => {
                        let value = self.builder.ins().load(I64, flags(), address, DATA);
                        self.store_register(dst, value);
                    }
                }
                self.builder.ins().jump(join, &[]);
                self.builder.switch_to_block(slow);
                self.builder.set_cold_block(slow);
                self.hand_off(position, before);
                self.builder.switch_to_block(join);
            }
            Intrinsic::MatrixElement => {
                let vector = self.load_register(args[0]);
                let (offset, in_range) =
                    self.matrix_offset(vector, args[1], types[1], args[2], types[2])?;
                let (kind, width) = match result {
                    Ty::Dbl => (SexpType::Real, 8),
                    Ty::Int => (SexpType::Integer, 4),
                    _ => (SexpType::Logical, 4),
                };
                let actual = self.builder.ins().uload8(I32, flags(), vector, KIND);
                let fits = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, actual, kind as i64);
                let readable = self.builder.ins().band(in_range, fits);
                self.hand_off_unless(position, before, readable);
                let scaled = self.builder.ins().imul_imm_s(offset, width);
                let address = self.builder.ins().iadd(vector, scaled);
                if result == Ty::Dbl {
                    let value = self.builder.ins().load(F64, flags(), address, DATA);
                    self.builder.def_var(self.doubles[dst], value);
                } else {
                    let value = self.builder.ins().load(I32, flags(), address, DATA);
                    self.builder.def_var(self.integers[dst], value);
                }
            }
            Intrinsic::Length => {
                let value = if types[0].is_unboxed() {
                    self.builder.ins().iconst(I32, 1)
                } else if types[0] == Ty::IntSeq {
                    // A compact integer sequence is never longer than an integer can count.
                    let vector = self.load_register(args[0]);
                    let (length, _, _) = self.sequence_info(vector, Ty::IntSeq);
                    self.builder.ins().fcvt_to_sint_sat(I32, length)
                } else {
                    let vector = self.load_register(args[0]);
                    let length = if types[0] == Ty::DblSeq {
                        let length = self.sequence_info(vector, Ty::DblSeq).0;
                        self.builder.ins().fcvt_to_sint_sat(I64, length)
                    } else {
                        self.builder.ins().load(I64, flags(), vector, LENGTH)
                    };
                    // Longer vectors have a double length; leave those to tier 0.
                    let fits = self.builder.ins().icmp_imm_s(
                        IntCC::SignedLessThanOrEqual,
                        length,
                        i32::MAX as i64,
                    );
                    let failed = self.deopt(
                        self.plan.insts[position].pc,
                        before.id,
                        None,
                        position,
                        reason::SPECULATION,
                    );
                    self.guard(fits, failed);
                    self.builder.ins().ireduce(I32, length)
                };
                self.builder.def_var(self.integers[dst], value);
            }
            Intrinsic::Negate(Numeric::Int) => {
                let value = self.use_register(args[0], types[0]);
                let missing = self.builder.ins().icmp(IntCC::Equal, value, na);
                let negated = self.builder.ins().ineg(value);
                let result = self.builder.ins().select(missing, na, negated);
                self.builder.def_var(self.integers[dst], result);
            }
            Intrinsic::Negate(Numeric::Dbl) => {
                let value = self.use_register(args[0], Ty::Dbl);
                let result = self.builder.ins().fneg(value);
                self.builder.def_var(self.doubles[dst], result);
            }
            Intrinsic::Abs(Numeric::Int) => {
                let value = self.use_register(args[0], types[0]);
                let missing = self.builder.ins().icmp(IntCC::Equal, value, na);
                let absolute = self.builder.ins().iabs(value);
                let result = self.builder.ins().select(missing, na, absolute);
                self.builder.def_var(self.integers[dst], result);
            }
            Intrinsic::Abs(Numeric::Dbl) => {
                let value = self.use_register(args[0], Ty::Dbl);
                let result = self.builder.ins().fabs(value);
                self.builder.def_var(self.doubles[dst], result);
            }
            Intrinsic::Not | Intrinsic::IsNa => {
                let (zero, missing) = if types[0] == Ty::Dbl {
                    let value = self.use_register(args[0], Ty::Dbl);
                    let zero_constant = self.builder.ins().f64const(0.0);
                    let zero = self
                        .builder
                        .ins()
                        .fcmp(FloatCC::Equal, value, zero_constant);
                    (
                        zero,
                        self.builder.ins().fcmp(FloatCC::Unordered, value, value),
                    )
                } else {
                    let value = self.use_register(args[0], types[0]);
                    let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, value, 0);
                    (zero, self.builder.ins().icmp(IntCC::Equal, value, na))
                };
                let result = if intrinsic == Intrinsic::IsNa {
                    self.builder.ins().uextend(I32, missing)
                } else {
                    let zero = self.builder.ins().uextend(I32, zero);
                    self.builder.ins().select(missing, na, zero)
                };
                self.builder.def_var(self.integers[dst], result);
            }
            Intrinsic::Sqrt | Intrinsic::Log => {
                let value = self.as_double(args[0], types[0]);
                let zero = self.builder.ins().f64const(0.0);
                // A negative argument warns, which tier 0 does.
                let negative = self.builder.ins().fcmp(FloatCC::LessThan, value, zero);
                let fast = self.builder.create_block();
                let slow = self.builder.create_block();
                let join = self.builder.create_block();
                self.builder.ins().brif(negative, slow, &[], fast, &[]);
                self.builder.switch_to_block(fast);
                let result = if intrinsic == Intrinsic::Sqrt {
                    self.builder.ins().sqrt(value)
                } else {
                    self.call(
                        runtime::jit_log as *const () as usize,
                        &[F64],
                        Some(F64),
                        &[value],
                    )
                    .ok_or(Refusal("a helper without a result"))?
                };
                self.builder.def_var(self.doubles[dst], result);
                self.builder.ins().jump(join, &[]);
                self.builder.switch_to_block(slow);
                self.builder.set_cold_block(slow);
                self.hand_off(position, before);
                self.builder.switch_to_block(join);
            }
            Intrinsic::Exp | Intrinsic::Floor | Intrinsic::Ceiling => {
                let value = self.as_double(args[0], types[0]);
                let result = match intrinsic {
                    Intrinsic::Exp => self
                        .call(
                            runtime::jit_exp as *const () as usize,
                            &[F64],
                            Some(F64),
                            &[value],
                        )
                        .ok_or(Refusal("a helper without a result"))?,
                    Intrinsic::Floor => self.builder.ins().floor(value),
                    _ => self.builder.ins().ceil(value),
                };
                self.builder.def_var(self.doubles[dst], result);
            }
            Intrinsic::Modulo(Numeric::Int) | Intrinsic::IntegerDivide(Numeric::Int) => {
                let a = self.use_register(args[0], types[0]);
                let b = self.use_register(args[1], types[1]);
                let helper = if matches!(intrinsic, Intrinsic::Modulo(_)) {
                    runtime::jit_integer_modulo as *const () as usize
                } else {
                    runtime::jit_integer_divide as *const () as usize
                };
                let result = self
                    .call(helper, &[I32, I32], Some(I32), &[a, b])
                    .ok_or(Refusal("a helper without a result"))?;
                self.builder.def_var(self.integers[dst], result);
            }
            Intrinsic::Modulo(Numeric::Dbl) => {
                let a = self.as_double(args[0], types[0]);
                let b = self.as_double(args[1], types[1]);
                // Tier 0 raises the warning, so a modulus that loses accuracy hands off.
                let loses = self
                    .call(
                        runtime::jit_modulus_loses_accuracy as *const () as usize,
                        &[F64, F64],
                        Some(I32),
                        &[a, b],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                let keeps = self.builder.ins().icmp_imm_u(IntCC::Equal, loses, 0);
                self.hand_off_unless(position, before, keeps);
                let result = self
                    .call(
                        runtime::jit_real_modulo as *const () as usize,
                        &[F64, F64],
                        Some(F64),
                        &[a, b],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                self.builder.def_var(self.doubles[dst], result);
            }
            Intrinsic::IntegerDivide(Numeric::Dbl) => {
                let a = self.as_double(args[0], types[0]);
                let b = self.as_double(args[1], types[1]);
                let result = self
                    .call(
                        runtime::jit_real_integer_divide as *const () as usize,
                        &[F64, F64],
                        Some(F64),
                        &[a, b],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                self.builder.def_var(self.doubles[dst], result);
            }
            Intrinsic::CollectionEmpty { set } => {
                let collection = self.load_register(args[0]);
                let set = self.builder.ins().iconst(I32, i64::from(set));
                let count = self
                    .call(
                        runtime::jit_collection_count as *const () as usize,
                        &[I64, I32],
                        Some(I64),
                        &[collection, set],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                // A negative count means the argument is not a collection, so tier 0 raises.
                let counted =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, count, 0);
                self.hand_off_unless(position, before, counted);
                let none = self.builder.ins().icmp_imm_s(IntCC::Equal, count, 0);
                let value = self.builder.ins().uextend(I32, none);
                self.builder.def_var(self.integers[dst], value);
            }
            Intrinsic::CollectionContains { set } => {
                let position_of = self.collection_position(args, &types, set)?;
                let usable =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, position_of, -1);
                self.hand_off_unless(position, before, usable);
                let present =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, position_of, 0);
                let value = self.builder.ins().uextend(I32, present);
                self.builder.def_var(self.integers[dst], value);
            }
            Intrinsic::DictGet => {
                let position_of = self.collection_position(args, &types, false)?;
                let usable =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, position_of, -1);
                self.hand_off_unless(position, before, usable);
                let dict = self.load_register(args[0]);
                let value = self
                    .call(
                        runtime::jit_dict_get as *const () as usize,
                        &[I64, I64, I64],
                        Some(I64),
                        &[self.activation, dict, position_of],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                self.store_register(dst, value);
            }
            Intrinsic::Colon => {
                let from = self.as_double(args[0], types[0]);
                let to = self.as_double(args[1], types[1]);
                let sequence = self
                    .call(
                        runtime::jit_colon as *const () as usize,
                        &[I64, F64, F64],
                        Some(I64),
                        &[self.activation, from, to],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                let made = self.builder.ins().icmp_imm_s(IntCC::NotEqual, sequence, 0);
                self.hand_off_unless(position, before, made);
                self.store_register(dst, sequence);
            }
            Intrinsic::Field { layout, index } => {
                let record = self.load_register(args[0]);
                let usable = self.plain_record(record);
                self.hand_off_unless(position, before, usable);
                // Only a record has a layout word, so it is read after the kind check.
                let id = self
                    .builder
                    .ins()
                    .load(I32, flags(), record, RECORD_LAYOUT_OFFSET as i32);
                let same = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, id, i64::from(layout));
                self.hand_off_unless(position, before, same);
                let offset = i32::try_from(RECORD_DATA_OFFSET as u64 + u64::from(index) * 8)
                    .map_err(|_| Refusal("a record field past the addressable slots"))?;
                let raw = self.builder.ins().load(I64, flags(), record, offset);
                let unboxed = if (index as usize) < UNBOXED_SLOTS {
                    let mask =
                        self.builder
                            .ins()
                            .load(I64, flags(), record, RECORD_UNBOXED_OFFSET as i32);
                    let bit = self.builder.ins().band_imm_u(mask, 1i64 << index);
                    self.builder.ins().icmp_imm_s(IntCC::NotEqual, bit, 0)
                } else {
                    self.builder.ins().iconst(I8, 0)
                };
                match result {
                    Ty::Int | Ty::Lgl | Ty::Dbl => {
                        let upper = self.builder.ins().ushr_imm_u(raw, 32);
                        let tagged = |prefix: u32, builder: &mut Builder| {
                            builder
                                .ins()
                                .icmp_imm_s(IntCC::Equal, upper, i64::from(prefix))
                        };
                        let fits = match result {
                            Ty::Int => tagged(INTEGER_PREFIX, &mut self.builder),
                            Ty::Lgl => tagged(LOGICAL_PREFIX, &mut self.builder),
                            _ => {
                                let integer = tagged(INTEGER_PREFIX, &mut self.builder);
                                let logical = tagged(LOGICAL_PREFIX, &mut self.builder);
                                let either = self.builder.ins().bor(integer, logical);
                                self.builder.ins().icmp_imm_s(IntCC::Equal, either, 0)
                            }
                        };
                        let usable = self.builder.ins().band(unboxed, fits);
                        self.hand_off_unless(position, before, usable);
                        if result == Ty::Dbl {
                            let value = self.builder.ins().bitcast(F64, MemFlagsData::new(), raw);
                            self.builder.def_var(self.doubles[dst], value);
                        } else {
                            let value = self.builder.ins().ireduce(I32, raw);
                            self.builder.def_var(self.integers[dst], value);
                        }
                    }
                    _ => {
                        // A raw slot would have to be boxed, which tier 0 does.
                        let handle = self.builder.ins().icmp_imm_s(IntCC::Equal, unboxed, 0);
                        self.hand_off_unless(position, before, handle);
                        self.store_register(dst, raw);
                    }
                }
            }
        }
        Ok(())
    }

    /// Continues when `condition` holds and otherwise hands the instruction to tier 0, which
    /// raises whatever error the compiled fast path declined to.
    fn hand_off_unless(&mut self, position: usize, before: &StateRef<'p>, condition: Value) {
        let fast = self.builder.create_block();
        let slow = self.builder.create_block();
        self.builder.ins().brif(condition, fast, &[], slow, &[]);
        self.builder.switch_to_block(slow);
        self.builder.set_cold_block(slow);
        self.hand_off(position, before);
        self.builder.switch_to_block(fast);
    }

    /// The key's position in the collection in `args[0]`, as [`runtime::jit_collection_position`]
    /// reports it. An unboxed key travels unboxed, so the probe allocates nothing.
    fn collection_position(
        &mut self,
        args: &[usize],
        types: &[Ty],
        set: bool,
    ) -> Result<Value, Refusal> {
        let collection = self.load_register(args[0]);
        let set = self.builder.ins().iconst(I32, i64::from(set));
        let no_integer = self.builder.ins().iconst(I32, 0);
        let no_double = self.builder.ins().f64const(0.0);
        let no_value = self.builder.ins().iconst(I64, 0);
        let (kind, integer, double, value) = match types[1] {
            Ty::Int => (
                runtime::key::INTEGER,
                self.use_register(args[1], Ty::Int),
                no_double,
                no_value,
            ),
            Ty::Dbl => (
                runtime::key::DOUBLE,
                no_integer,
                self.use_register(args[1], Ty::Dbl),
                no_value,
            ),
            _ => (
                runtime::key::VALUE,
                no_integer,
                no_double,
                self.load_register(args[1]),
            ),
        };
        let kind = self.builder.ins().iconst(I32, i64::from(kind));
        self.call(
            runtime::jit_collection_position as *const () as usize,
            &[I64, I32, I32, I32, F64, I64],
            Some(I64),
            &[collection, set, kind, integer, double, value],
        )
        .ok_or(Refusal("a helper without a result"))
    }

    // Conditions.

    /// The truth of `if`'s condition as a Cranelift boolean, deoptimizing with the error for
    /// `NA` and anything else tier 0 rejects.
    unsafe fn condition(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        src: usize,
        call: Sexp,
    ) -> Result<Value, Refusal> {
        let ty = before.registers[src];
        let pc = self.plan.insts[position].pc;
        let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
        let (truth, valid) = match ty {
            Ty::Int | Ty::Lgl => {
                let value = self.use_register(src, ty);
                let truth = self.builder.ins().icmp_imm_s(IntCC::NotEqual, value, 0);
                (
                    truth,
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::NotEqual, value, NA_INTEGER as i64),
                )
            }
            Ty::Dbl => {
                let value = self.use_register(src, ty);
                let zero = self.builder.ins().f64const(0.0);
                let truth = self
                    .builder
                    .ins()
                    .fcmp(FloatCC::OrderedNotEqual, value, zero);
                (
                    truth,
                    self.builder.ins().fcmp(FloatCC::Ordered, value, value),
                )
            }
            _ => {
                let value = self.load_register(src);
                let call = self.builder.ins().iconst(I64, call.as_ptr() as i64);
                let result = self
                    .call(
                        runtime::jit_condition as *const () as usize,
                        &[I64, I64, I64],
                        Some(I32),
                        &[self.activation, value, call],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                let valid = self.builder.ins().icmp_imm_s(IntCC::NotEqual, result, 2);
                self.guard(valid, failed);
                return Ok(self.builder.ins().icmp_imm_s(IntCC::NotEqual, result, 0));
            }
        };
        // `NA`: box it, so tier 0 raises the error for the value it would have seen.
        let fine = self.builder.create_block();
        let missing = self.builder.create_block();
        self.builder.ins().brif(valid, fine, &[], missing, &[]);
        self.builder.switch_to_block(missing);
        self.builder.set_cold_block(missing);
        let value = self.use_register(src, ty);
        let boxed = self.box_value(ty, value);
        self.store_register(src, boxed);
        let call = self.builder.ins().iconst(I64, call.as_ptr() as i64);
        self.call(
            runtime::jit_condition as *const () as usize,
            &[I64, I64, I64],
            Some(I32),
            &[self.activation, boxed, call],
        );
        self.builder.ins().jump(failed, &[]);
        self.builder.switch_to_block(fine);
        Ok(truth)
    }

    /// An operand of `&&` or `||` as a logical `i32`.
    unsafe fn logical(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        src: usize,
        call: Sexp,
        is_and: bool,
        right: bool,
    ) -> Result<Value, Refusal> {
        let ty = before.registers[src];
        let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
        Ok(match ty {
            Ty::Lgl => self.use_register(src, ty),
            Ty::Int => {
                let value = self.use_register(src, ty);
                let missing = self.builder.ins().icmp(IntCC::Equal, value, na);
                let truth = self.builder.ins().icmp_imm_s(IntCC::NotEqual, value, 0);
                let truth = self.builder.ins().uextend(I32, truth);
                self.builder.ins().select(missing, na, truth)
            }
            Ty::Dbl => {
                let value = self.use_register(src, ty);
                let missing = self.builder.ins().fcmp(FloatCC::Unordered, value, value);
                let zero = self.builder.ins().f64const(0.0);
                let truth = self
                    .builder
                    .ins()
                    .fcmp(FloatCC::OrderedNotEqual, value, zero);
                let truth = self.builder.ins().uextend(I32, truth);
                self.builder.ins().select(missing, na, truth)
            }
            _ => {
                let pc = self.plan.insts[position].pc;
                let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
                let value = self.load_register(src);
                let call = self.builder.ins().iconst(I64, call.as_ptr() as i64);
                let is_and = self.builder.ins().iconst(I32, is_and as i64);
                let right = self.builder.ins().iconst(I32, right as i64);
                let result = self
                    .call(
                        runtime::jit_logic as *const () as usize,
                        &[I64, I64, I64, I32, I32],
                        Some(I32),
                        &[self.activation, value, call, is_and, right],
                    )
                    .ok_or(Refusal("a helper without a result"))?;
                let valid = self.builder.ins().icmp_imm_s(IntCC::NotEqual, result, 2);
                self.guard(valid, failed);
                result
            }
        })
    }

    // Element assignment.

    unsafe fn sub_assign(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        dst: usize,
        slot: usize,
        subscript: Subscript,
        value: usize,
        store: Store,
    ) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let value_ty = before.registers[value];
        let checked = store == Store::Checked;
        let target = if checked {
            Ty::Boxed
        } else {
            before.slots[slot].ty
        };
        let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
        let mismatch = self.deopt(pc, before.id, None, position, reason::SPECULATION);
        // An unloaded variable is loaded first: a loop that calls a function every iteration
        // forgets its variables every time.
        if before.slots[slot].may_be(status::NOT_LOADED) {
            let status = self.status(slot);
            let unloaded = self.builder.ins().icmp_imm_s(IntCC::Equal, status, 0);
            let load = self.builder.create_block();
            let loaded = self.builder.create_block();
            self.builder.ins().brif(unloaded, load, &[], loaded, &[]);
            self.builder.switch_to_block(load);
            let slot_constant = iconst(&mut self.builder, slot);
            let result = self
                .call(
                    runtime::jit_load_slot as *const () as usize,
                    &[I64, I64],
                    Some(I32),
                    &[self.activation, slot_constant],
                )
                .ok_or(Refusal("a helper without a result"))?;
            let error =
                self.builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, result, outcome::LOAD_FAILED as i64);
            let not_error = self.builder.create_block();
            self.builder.ins().brif(error, failed, &[], not_error, &[]);
            self.builder.switch_to_block(not_error);
            let invalid = self.builder.ins().icmp_imm_s(
                IntCC::Equal,
                result,
                outcome::LOAD_INVALIDATED as i64,
            );
            let valid = self.builder.ins().bxor_imm_u(invalid, 1);
            self.guard(valid, mismatch);
            let vector = self.load_slot_cell(slot);
            let fits = self.check(vector, target);
            self.guard(fits, mismatch);
            self.set_status(slot, result);
            self.builder.ins().jump(loaded, &[]);
            self.builder.switch_to_block(loaded);
        }
        let vector = self.load_slot_cell(slot);
        let length = self.builder.ins().load(I64, flags(), vector, LENGTH);
        if let (Store::List, Subscript::Vector(index)) = (store, subscript) {
            return self.store_list_element(
                position, before, dst, slot, vector, length, index, value, failed,
            );
        }
        // The zero-based element position and whether the store may happen in place, and for
        // a matrix whether it holds doubles, which an integer or logical value converts to.
        let (element_offset, in_range, real) = match subscript {
            Subscript::Vector(index) => {
                let (position_value, in_range) = self.subscript_position(
                    before.registers[index],
                    index,
                    length,
                    target.is_array(),
                );
                // An Array cannot hold a missing value, so storing one is tier 0's to do: it
                // turns the Array into an R vector.
                let in_range = match target {
                    Ty::Boxed => {
                        let accepted = self.accepts_scalar(vector, value_ty);
                        self.builder.ins().band(in_range, accepted)
                    }
                    Ty::DblArr => {
                        let element = self.use_register(value, Ty::Dbl);
                        let present = self.builder.ins().fcmp(FloatCC::Ordered, element, element);
                        self.builder.ins().band(in_range, present)
                    }
                    Ty::IntArr | Ty::LglArr => {
                        let element = self.use_register(value, value_ty);
                        let present = self.builder.ins().icmp_imm_s(
                            IntCC::NotEqual,
                            element,
                            NA_INTEGER as i64,
                        );
                        self.builder.ins().band(in_range, present)
                    }
                    _ => in_range,
                };
                let real = if checked {
                    let kind = self.builder.ins().uload8(I32, flags(), vector, KIND);
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::Equal, kind, SexpType::Real as i64)
                } else {
                    let real = matches!(target, Ty::DblVec | Ty::DblArr);
                    self.builder.ins().iconst(I8, i64::from(real))
                };
                let offset = self.builder.ins().iadd_imm_s(position_value, -1);
                (offset, in_range, real)
            }
            Subscript::Matrix(row, column) => {
                let (offset, in_range) = self.matrix_offset(
                    vector,
                    row,
                    before.registers[row],
                    column,
                    before.registers[column],
                )?;
                let kind = self.builder.ins().uload8(I32, flags(), vector, KIND);
                let real = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, kind, SexpType::Real as i64);
                let accepted = self.kind_accepts(kind, value_ty);
                let in_range = self.builder.ins().band(in_range, accepted);
                (offset, in_range, real)
            }
        };
        let fast = self.builder.create_block();
        let slow = self.builder.create_block();
        let join = self.builder.create_block();
        self.builder.ins().brif(in_range, fast, &[], slow, &[]);

        self.builder.switch_to_block(fast);
        let status = self.status(slot);
        let enclosing =
            self.builder
                .ins()
                .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_ENCLOSING);
        let count = self.builder.ins().uload16(I32, flags(), vector, REFCOUNT);
        let shared_count = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThan, count, 1);
        let shared = self.builder.ins().bor(enclosing, shared_count);
        let copy = self.builder.create_block();
        let store = self.builder.create_block();
        self.builder.append_block_param(store, I64);
        self.builder.ins().brif(shared, copy, &[], store, &[vector]);
        self.builder.switch_to_block(copy);
        let dirty = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
        let dirty = self.builder.ins().uextend(I32, dirty);
        let slot_constant = iconst(&mut self.builder, slot);
        let fresh = self
            .call(
                runtime::jit_duplicate as *const () as usize,
                &[I64, I64, I32],
                Some(I64),
                &[self.activation, slot_constant, dirty],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let copied = self.builder.ins().icmp_imm_s(IntCC::NotEqual, fresh, 0);
        self.guard(copied, failed);
        let dirty_status = self.builder.ins().iconst(I32, status::RUNTIME_DIRTY);
        self.set_status(slot, dirty_status);
        self.builder.ins().jump(store, &[fresh]);
        self.builder.switch_to_block(store);
        let vector = self.builder.block_params(store)[0];
        let store_real = self.builder.create_block();
        let store_integer = self.builder.create_block();
        self.builder
            .ins()
            .brif(real, store_real, &[], store_integer, &[]);
        self.builder.switch_to_block(store_real);
        let element = self.as_double(value, value_ty);
        let scaled = self.builder.ins().imul_imm_s(element_offset, 8);
        let address = self.builder.ins().iadd(vector, scaled);
        self.builder.ins().store(flags(), element, address, DATA);
        self.builder.ins().jump(join, &[]);
        self.builder.switch_to_block(store_integer);
        if value_ty == Ty::Dbl {
            // Only a double vector takes a double, so this block never runs.
            self.builder.ins().jump(join, &[]);
        } else {
            let element = self.use_register(value, value_ty);
            let scaled = self.builder.ins().imul_imm_s(element_offset, 4);
            let address = self.builder.ins().iadd(vector, scaled);
            self.builder.ins().store(flags(), element, address, DATA);
            self.builder.ins().jump(join, &[]);
        }

        self.builder.switch_to_block(slow);
        // Tier 0's fast handler stores into an atomic vector only what the code above does, so
        // with the vector's type known it would only decline.
        let fast = matches!(subscript, Subscript::Vector(_)) && target == Ty::Boxed;
        self.sub_assign_in_tier0(position, before, slot, fast, failed)?;
        self.builder.ins().jump(join, &[]);

        self.builder.switch_to_block(join);
        if value_ty.is_unboxed() {
            let copy = self.use_register(value, value_ty);
            self.def_register(dst, value_ty, copy);
        }
        Ok(())
    }

    /// The slow path of a store compiled code declines, such as one out of range: writes the
    /// frame back, since tier 0 may read any variable while it grows or rejects the vector, has
    /// tier 0 run the assignment, and forgets the variable it rebinds.
    unsafe fn sub_assign_in_tier0(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        slot: usize,
        fast: bool,
        failed: Label,
    ) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let block = self
            .builder
            .current_block()
            .ok_or(Refusal("no current block"))?;
        self.builder.set_cold_block(block);
        self.box_reads(position, before);
        // Tier 0's fast handler takes the stores this code leaves out, such as one into a list
        // with names, and needs only this variable in the frame.
        self.flush_slot(before, slot, failed);
        let [stored, stepped, stepping] = self.fast_step(pc, fast, failed)?;
        let done = self.builder.create_block();
        self.builder.set_cold_block(stepping);
        self.builder.switch_to_block(stepped);
        let unloaded = self.builder.ins().iconst(I32, 0);
        self.set_status(slot, unloaded);
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(stepping);
        self.flush_all(before, failed);
        let zero = self.builder.ins().iconst(I32, 0);
        let result = self.call_step(pc, zero)?;
        let ok = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, result, outcome::OK as i64);
        self.guard(ok, failed);
        let unloaded = self.builder.ins().iconst(I32, 0);
        self.set_status(slot, unloaded);
        self.builder.ins().jump(done, &[]);
        // The handler stores into the frame's object in place, so a variable held as that
        // object stays valid, and only an unboxed copy goes stale.
        self.builder.switch_to_block(stored);
        if before.slots[slot].ty.in_variables() {
            let unloaded = self.builder.ins().iconst(I32, 0);
            self.set_status(slot, unloaded);
        }
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
        Ok(())
    }

    /// `x[[index]] <- value` on the list `vector`: a position up to one past the end goes to
    /// [`runtime::jit_store_list_element`], which stores in place or appends within the list's
    /// capacity, and tier 0 runs whatever that helper declines.
    #[allow(clippy::too_many_arguments)]
    unsafe fn store_list_element(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        dst: usize,
        slot: usize,
        vector: Value,
        length: Value,
        index: usize,
        value: usize,
        failed: Label,
    ) -> Result<(), Refusal> {
        let value_ty = before.registers[value];
        let bound = self.builder.ins().iadd_imm_s(length, 1);
        let (index_value, in_range) =
            self.subscript_position(before.registers[index], index, bound, false);
        // A variable of an enclosing frame is copied into this frame first, which tier 0 does.
        let status = self.status(slot);
        let local =
            self.builder
                .ins()
                .icmp_imm_s(IntCC::NotEqual, status, status::RUNTIME_ENCLOSING);
        let attempt = self.builder.ins().band(in_range, local);
        let fast = self.builder.create_block();
        let slow = self.builder.create_block();
        let join = self.builder.create_block();
        self.builder.ins().brif(attempt, fast, &[], slow, &[]);

        self.builder.switch_to_block(fast);
        let element = if value_ty.is_unboxed() {
            let unboxed = self.use_register(value, value_ty);
            let boxed = self.box_value(value_ty, unboxed);
            let allocated = self.builder.ins().icmp_imm_s(IntCC::NotEqual, boxed, 0);
            self.guard(allocated, failed);
            boxed
        } else {
            self.load_register(value)
        };
        let stored = self
            .call(
                runtime::jit_store_list_element as *const () as usize,
                &[I64, I64, I64],
                Some(I32),
                &[vector, index_value, element],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let stored = self.builder.ins().icmp_imm_s(IntCC::NotEqual, stored, 0);
        self.builder.ins().brif(stored, join, &[], slow, &[]);

        self.builder.switch_to_block(slow);
        self.sub_assign_in_tier0(position, before, slot, true, failed)?;
        self.builder.ins().jump(join, &[]);

        self.builder.switch_to_block(join);
        if value_ty.is_unboxed() {
            let copy = self.use_register(value, value_ty);
            self.def_register(dst, value_ty, copy);
        } else {
            let object = self.load_register(value);
            self.store_register(dst, object);
        }
        Ok(())
    }

    /// Whether `vector` is a plain logical, integer or double vector, without attributes, that
    /// takes a scalar of type `value_ty` without changing its type.
    fn accepts_scalar(&mut self, vector: Value, value_ty: Ty) -> Value {
        let kind = self.builder.ins().uload8(I32, flags(), vector, KIND);
        let accepted = self.kind_accepts(kind, value_ty);
        let attributes = self.builder.ins().load(I64, flags(), vector, ATTRIB);
        let plain = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, attributes, self.nil);
        let bits = self.builder.ins().uload8(I32, flags(), vector, FLAGS);
        let bits = self
            .builder
            .ins()
            .band_imm_u(bits, ALTREP_FLAG | ARRAY_FLAG);
        let ordinary = self.builder.ins().icmp_imm_s(IntCC::Equal, bits, 0);
        let accepted = self.builder.ins().band(accepted, plain);
        self.builder.ins().band(accepted, ordinary)
    }

    /// Whether a vector of `kind` takes a scalar of type `value_ty` without changing its type: a
    /// double goes only into a double vector, an integer also into an integer one, and a logical
    /// into any of the three.
    fn kind_accepts(&mut self, kind: Value, value_ty: Ty) -> Value {
        let real = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, kind, SexpType::Real as i64);
        let integer = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, kind, SexpType::Integer as i64);
        let logical = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, kind, SexpType::Logical as i64);
        match value_ty {
            Ty::Dbl => real,
            Ty::Int => self.builder.ins().bor(real, integer),
            _ => {
                let either = self.builder.ins().bor(real, integer);
                self.builder.ins().bor(either, logical)
            }
        }
    }

    /// The one-based position the subscript in register `index` names and whether it is in
    /// `1..=bound`. A double truncates, as R does, and an `array` rejects a double past `bound`
    /// before truncating. A negative `bound` rejects every subscript.
    fn subscript_position(
        &mut self,
        index_ty: Ty,
        index: usize,
        bound: Value,
        array: bool,
    ) -> (Value, Value) {
        match index_ty {
            Ty::Int => {
                let index_value = self.use_register(index, Ty::Int);
                let index_value = self.builder.ins().sextend(I64, index_value);
                let positive =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, index_value, 1);
                let within =
                    self.builder
                        .ins()
                        .icmp(IntCC::SignedLessThanOrEqual, index_value, bound);
                (index_value, self.builder.ins().band(positive, within))
            }
            _ => {
                let index_value = self.use_register(index, Ty::Dbl);
                let one = self.builder.ins().f64const(1.0);
                let positive =
                    self.builder
                        .ins()
                        .fcmp(FloatCC::GreaterThanOrEqual, index_value, one);
                let within = self.within_length(index_value, bound, array);
                let in_range = self.builder.ins().band(positive, within);
                let safe = self.builder.ins().select(in_range, index_value, one);
                (self.builder.ins().fcvt_to_sint(I64, safe), in_range)
            }
        }
    }

    /// The zero-based position of element `[row, column]` of `vector` and whether `vector` is
    /// an ordinary matrix (see [`runtime::jit_matrix_rows`]) with that element in range.
    fn matrix_offset(
        &mut self,
        vector: Value,
        row: usize,
        row_ty: Ty,
        column: usize,
        column_ty: Ty,
    ) -> Result<(Value, Value), Refusal> {
        let length = self.builder.ins().load(I64, flags(), vector, LENGTH);
        let rows = self
            .call(
                runtime::jit_matrix_rows as *const () as usize,
                &[I64],
                Some(I64),
                &[vector],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let (row_value, row_in_range) = self.subscript_position(row_ty, row, rows, false);
        let (column_value, column_in_range) =
            self.subscript_position(column_ty, column, length, false);
        let row_offset = self.builder.ins().iadd_imm_s(row_value, -1);
        let column_offset = self.builder.ins().iadd_imm_s(column_value, -1);
        let column_start = self.builder.ins().imul(column_offset, rows);
        let offset = self.builder.ins().iadd(column_start, row_offset);
        let within = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, offset, length);
        let in_range = self.builder.ins().band(row_in_range, column_in_range);
        Ok((offset, self.builder.ins().band(in_range, within)))
    }

    // Variables.

    /// Loads a slot whose runtime status may be "not loaded", checking the value has type `ty`.
    /// A `forcing` load may evaluate a default argument in this frame, so the variables are
    /// written back before it and forgotten after it.
    unsafe fn load_slot(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        slot: usize,
        ty: Ty,
        forcing: bool,
        volatile: bool,
        dst: Option<usize>,
    ) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let failed = self.deopt(pc, before.id, None, position, reason::PENDING);
        // Once the load has run, tier 0 resumes after it with the loaded value rather than
        // loading again: the load may have forced a promise, and only that forcing knows whether
        // the value is visible, which `withVisible(x)` reads.
        let mismatch = match dst {
            Some(dst) => {
                let after = self.after_with_boxed(position, Some(dst))?;
                let next = self.next_pc(position)?;
                self.deopt(next, after.0, after.1, position + 1, reason::SPECULATION)
            }
            None => self.deopt(pc, before.id, None, position, reason::SPECULATION),
        };
        let done = self.builder.create_block();
        let load = self.builder.create_block();
        if before.slots[slot].may_be(status::LOADED) {
            let status = self.status(slot);
            let unloaded = self.builder.ins().icmp_imm_s(IntCC::Equal, status, 0);
            self.builder.ins().brif(unloaded, load, &[], done, &[]);
        } else {
            self.builder.ins().jump(load, &[]);
        }
        self.builder.switch_to_block(load);
        let merged = self.builder.create_block();
        let merged_value = self.builder.append_block_param(merged, I64);
        let merged_status = self.builder.append_block_param(merged, I32);
        let inst = &self.plan.insts[position];
        // Only this activation's own `GetVar` may probe: the probe reads the activation's
        // environment and lookup hints, which belong to the outer body, and an inlined callee's
        // loads reach here as other ops.
        let probed = inst.op == Op::GetVar;
        if probed {
            let symbol = runtime_constant(self.code, inst.operand(1));
            let lookup = inst.operand(2);
            let slow = self.builder.create_block();
            self.builder.set_cold_block(slow);
            self.probe_variable(symbol, lookup, slow, merged);
            self.builder.switch_to_block(slow);
        }
        if forcing {
            self.flush_all(before, failed);
        }
        let pc_constant = iconst(&mut self.builder, pc);
        let slot_constant = iconst(&mut self.builder, slot);
        let result = self
            .call(
                runtime::jit_load as *const () as usize,
                &[I64, I64, I64],
                Some(I32),
                &[self.activation, pc_constant, slot_constant],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let error =
            self.builder
                .ins()
                .icmp_imm_s(IntCC::Equal, result, outcome::LOAD_FAILED as i64);
        let not_error = self.builder.create_block();
        self.builder.ins().brif(error, failed, &[], not_error, &[]);
        self.builder.switch_to_block(not_error);
        let invalid =
            self.builder
                .ins()
                .icmp_imm_s(IntCC::Equal, result, outcome::LOAD_INVALIDATED as i64);
        let valid = self.builder.ins().bxor_imm_u(invalid, 1);
        let value = self.load_slot_cell(slot);
        if let Some(dst) = dst {
            self.store_register(dst, value);
        }
        self.guard(valid, mismatch);
        if forcing {
            self.forget_slots(before);
        }
        self.builder.ins().jump(merged, &[value, result]);
        self.builder.switch_to_block(merged);
        let value = merged_value;
        if probed {
            self.store_slot_cell(slot, value);
            if let Some(dst) = dst {
                self.store_register(dst, value);
            }
        }
        let (value, copied) = if matches!(ty, Ty::IntVec | Ty::DblVec) {
            let (value, copied) = self.sequence_contents(value, ty, slot, dst, mismatch)?;
            (value, Some(copied))
        } else {
            (value, None)
        };
        if !matches!(ty, Ty::Boxed | Ty::Bottom) {
            let fits = self.check(value, ty);
            if probed {
                // The inline probe records no feedback, so a value of another type is recorded
                // here for the recompile after the deoptimization to see.
                let fitting = self.builder.create_block();
                let record = self.builder.create_block();
                self.builder.set_cold_block(record);
                self.builder.ins().brif(fits, fitting, &[], record, &[]);
                self.builder.switch_to_block(record);
                let pc_constant = iconst(&mut self.builder, pc);
                self.call(
                    runtime::jit_record as *const () as usize,
                    &[I64, I64, I64],
                    None,
                    &[self.activation, pc_constant, value],
                );
                self.builder.ins().jump(mismatch, &[]);
                self.builder.switch_to_block(fitting);
            } else {
                self.guard(fits, mismatch);
            }
        }
        // The slot holds a copy the frame does not bind, so it reads as an outer variable: an
        // element assignment copies it again and writes the result back, rather than changing
        // the copy in place where the frame never sees it.
        let status = if volatile {
            self.builder.ins().iconst(I32, 0)
        } else {
            match copied {
                Some(copied) => {
                    let outer = self.builder.ins().iconst(I32, status::RUNTIME_ENCLOSING);
                    self.builder.ins().select(copied, outer, merged_status)
                }
                None => merged_status,
            }
        };
        self.set_status(slot, status);
        match ty {
            Ty::Int | Ty::Lgl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_integers[slot], unboxed);
            }
            Ty::Dbl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_doubles[slot], unboxed);
            }
            Ty::Num => {
                let (unboxed, flag) = self.unbox_num(value);
                self.builder.def_var(self.slot_doubles[slot], unboxed);
                self.builder.def_var(self.slot_integers[slot], flag);
            }
            _ => {}
        }
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
        Ok(())
    }

    /// Finds `symbol`'s value as tier 0's lookup does when the variable sits in the activation's
    /// slot frame, or where lookup slot `lookup` last found its binding in the activation's
    /// environment or in an enclosing function frame, and holds a value or a forced promise.
    /// That case runs no code and leaves the frame as it was, so it needs no write-back, no
    /// feedback and no validity check, and it jumps to `merged` with the value and its runtime
    /// status. Everything else branches to `slow`. Only the slot frame's read is inline; the
    /// frame walks are [`runtime::jit_probe`], which keeps each load a few instructions long.
    unsafe fn probe_variable(&mut self, symbol: Sexp, lookup: usize, slow: Label, merged: Label) {
        let outline = self.builder.create_block();
        // The slot table is fixed with the code, so the slot is a constant. A symbol the table
        // does not number is free in the body, and its lookup starts at the closure's
        // environment.
        let slots = self.code.cdr().element(bytecode::SLOTS);
        let slot = (slots != values::nil())
            .then(|| values::ints(slots.element(0)).get(lookup).copied())
            .flatten()
            .unwrap_or(-1);
        if slot >= 0 {
            // A callee can give the slot frame its environment while the activation runs, so
            // every probe reads the header register again.
            let header = self
                .builder
                .ins()
                .load(I64, flags(), self.activation, offset::HEADER);
            let env = self.builder.ins().load(I64, flags(), header, 0);
            let nil = values::nil().as_ptr() as i64;
            let in_slots = self.builder.ins().icmp_imm_u(IntCC::Equal, env, nil);
            let slotted = self.builder.create_block();
            self.builder
                .ins()
                .brif(in_slots, slotted, &[], outline, &[]);
            self.builder.switch_to_block(slotted);
            let value =
                self.builder
                    .ins()
                    .load(I64, flags(), header, (8 * (1 + slot as usize)) as i32);
            let local_status = self.builder.ins().iconst(I32, status::RUNTIME_LOCAL);
            let unbound = values::unbound().as_ptr() as i64;
            for marker in [values::missing_arg().as_ptr() as i64, unbound] {
                let differs = self
                    .builder
                    .ins()
                    .icmp_imm_u(IntCC::NotEqual, value, marker);
                let next = self.builder.create_block();
                self.builder.ins().brif(differs, next, &[], slow, &[]);
                self.builder.switch_to_block(next);
            }
            let kind = self
                .builder
                .ins()
                .uload8(I32, flags(), value, header_layout::KIND as i32);
            let promise =
                self.builder
                    .ins()
                    .icmp_imm_u(IntCC::Equal, kind, SexpType::Promise as i64);
            let forced = self.builder.create_block();
            let plain = self.builder.create_block();
            self.builder.ins().brif(promise, forced, &[], plain, &[]);
            self.builder.switch_to_block(plain);
            self.builder.ins().jump(merged, &[value, local_status]);
            self.builder.switch_to_block(forced);
            let promised =
                self.builder
                    .ins()
                    .load(I64, flags(), value, header_layout::FIRST as i32);
            let evaluated = self
                .builder
                .ins()
                .icmp_imm_u(IntCC::NotEqual, promised, unbound);
            let done = self.builder.create_block();
            self.builder.ins().brif(evaluated, done, &[], slow, &[]);
            self.builder.switch_to_block(done);
            self.builder.ins().jump(merged, &[promised, local_status]);
        } else {
            self.builder.ins().jump(outline, &[]);
        }

        self.builder.switch_to_block(outline);
        let symbol = self.builder.ins().iconst(I64, symbol.as_ptr() as i64);
        let lookup = iconst(&mut self.builder, lookup);
        let Some(found) = self.call(
            runtime::jit_probe as *const () as usize,
            &[I64, I64, I64],
            Some(I64),
            &[self.activation, symbol, lookup],
        ) else {
            self.builder.ins().jump(slow, &[]);
            return;
        };
        let missed = self.builder.ins().icmp_imm_u(IntCC::Equal, found, 0);
        let hit = self.builder.create_block();
        self.builder.ins().brif(missed, slow, &[], hit, &[]);
        self.builder.switch_to_block(hit);
        let value = self.builder.ins().band_imm_u(found, !1);
        let enclosing = self.builder.ins().band_imm_u(found, 1);
        let enclosing = self.builder.ins().ireduce(I32, enclosing);
        // The low bit picks the enclosing status over the local one.
        let step = self
            .builder
            .ins()
            .imul_imm_s(enclosing, status::RUNTIME_ENCLOSING - status::RUNTIME_LOCAL);
        let status = self.builder.ins().iadd_imm_s(step, status::RUNTIME_LOCAL);
        self.builder.ins().jump(merged, &[value, status]);
    }

    /// Loads a slot for an inlined call without forcing anything, handing the call back to
    /// tier 0 when the variable is not a plain value already.
    unsafe fn peek(
        &mut self,
        position: usize,
        before: &StateRef<'p>,
        slot: usize,
        ty: Ty,
    ) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let mismatch = self.deopt(pc, before.id, None, position, reason::SPECULATION);
        let done = self.builder.create_block();
        let load = self.builder.create_block();
        if before.slots[slot].may_be(status::LOADED) {
            let status = self.status(slot);
            let unloaded = self.builder.ins().icmp_imm_s(IntCC::Equal, status, 0);
            self.builder.ins().brif(unloaded, load, &[], done, &[]);
        } else {
            self.builder.ins().jump(load, &[]);
        }
        self.builder.switch_to_block(load);
        let slot_constant = iconst(&mut self.builder, slot);
        let result = self
            .call(
                runtime::jit_peek_slot as *const () as usize,
                &[I64, I64],
                Some(I32),
                &[self.activation, slot_constant],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let declined =
            self.builder
                .ins()
                .icmp_imm_s(IntCC::Equal, result, outcome::LOAD_FAILED as i64);
        let peeked = self.builder.create_block();
        let slow = self.builder.create_block();
        self.builder.ins().brif(declined, slow, &[], peeked, &[]);
        self.builder.switch_to_block(slow);
        self.builder.set_cold_block(slow);
        self.hand_off(position, before);
        self.builder.switch_to_block(peeked);
        let value = self.load_slot_cell(slot);
        let fits = self.check(value, ty);
        self.guard(fits, mismatch);
        self.set_status(slot, result);
        match ty {
            Ty::Int | Ty::Lgl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_integers[slot], unboxed);
            }
            Ty::Dbl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_doubles[slot], unboxed);
            }
            Ty::Num => {
                let (unboxed, flag) = self.unbox_num(value);
                self.builder.def_var(self.slot_doubles[slot], unboxed);
                self.builder.def_var(self.slot_integers[slot], flag);
            }
            _ => {}
        }
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
        Ok(())
    }

    /// Makes `slot` dirty with `value`: unboxed for an unboxed `ty`, else an object the slot
    /// then owns a reference to. Releases the reference the slot held to its old value, if it
    /// was dirty and boxed, after counting the new one, since the two may be the same object.
    unsafe fn assign_slot(&mut self, before: &StateRef<'p>, slot: usize, ty: Ty, value: Value) {
        let old = before.slots[slot];
        let release = (!old.ty.in_variables() && old.ty != Ty::Bottom && old.may_be(status::DIRTY))
            .then(|| {
                let status = self.status(slot);
                let dirty =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
                (self.load_slot_cell(slot), dirty)
            });
        match ty {
            Ty::Int | Ty::Lgl => self.builder.def_var(self.slot_integers[slot], value),
            Ty::Dbl | Ty::Num => self.builder.def_var(self.slot_doubles[slot], value),
            _ => {
                self.store_slot_cell(slot, value);
                self.increment(value);
            }
        }
        if let Some((previous, dirty)) = release {
            self.decrement_if(previous, dirty);
        }
        let dirty = self.builder.ins().iconst(I32, status::RUNTIME_DIRTY);
        self.set_status(slot, dirty);
    }

    /// Writes every dirty slot back to the environment.
    unsafe fn flush_all(&mut self, state: &StateRef<'p>, failed: Label) {
        for slot in 0..state.slots.len() {
            self.flush_slot(state, slot, failed);
        }
    }

    unsafe fn flush_slot(&mut self, state: &StateRef<'p>, slot: usize, failed: Label) {
        let current = state.slots[slot];
        if !current.may_be(status::DIRTY) {
            return;
        }
        let status = self.status(slot);
        let dirty = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
        let flush = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.ins().brif(dirty, flush, &[], done, &[]);
        self.builder.switch_to_block(flush);
        let result = self.call_flush(slot, current.ty);
        let ok = self.builder.ins().icmp_imm_s(IntCC::NotEqual, result, 0);
        self.guard(ok, failed);
        let clean = self.builder.ins().iconst(I32, status::RUNTIME_LOCAL);
        self.set_status(slot, clean);
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
    }

    unsafe fn call_flush(&mut self, slot: usize, ty: Ty) -> Value {
        let (kind, bits) = match ty {
            Ty::Int | Ty::Lgl => {
                let value = self.builder.use_var(self.slot_integers[slot]);
                let kind = if ty == Ty::Int {
                    representation::INTEGER
                } else {
                    representation::LOGICAL
                };
                (kind, self.builder.ins().sextend(I64, value))
            }
            Ty::Dbl => {
                let value = self.builder.use_var(self.slot_doubles[slot]);
                (
                    representation::DOUBLE,
                    self.builder.ins().bitcast(I64, MemFlagsData::new(), value),
                )
            }
            Ty::Num => {
                let value = self.builder.use_var(self.slot_doubles[slot]);
                let flag = self.builder.use_var(self.slot_integers[slot]);
                let integer = self.builder.ins().icmp_imm_s(IntCC::NotEqual, flag, 0);
                let narrowed = self.num_to_integer(value);
                let narrowed = self.builder.ins().sextend(I64, narrowed);
                let bits = self.builder.ins().bitcast(I64, MemFlagsData::new(), value);
                let bits = self.builder.ins().select(integer, narrowed, bits);
                let integer_kind = self
                    .builder
                    .ins()
                    .iconst(I32, representation::INTEGER as i64);
                let double_kind = self
                    .builder
                    .ins()
                    .iconst(I32, representation::DOUBLE as i64);
                let kind = self
                    .builder
                    .ins()
                    .select(integer, integer_kind, double_kind);
                return self.flush_call(slot, kind, bits);
            }
            _ => (representation::BOXED, self.builder.ins().iconst(I64, 0)),
        };
        let kind = self.builder.ins().iconst(I32, kind as i64);
        self.flush_call(slot, kind, bits)
    }

    fn flush_call(&mut self, slot: usize, kind: Value, bits: Value) -> Value {
        let slot_constant = iconst(&mut self.builder, slot);
        self.call(
            runtime::jit_flush as *const () as usize,
            &[I64, I64, I32, I64],
            Some(I32),
            &[self.activation, slot_constant, kind, bits],
        )
        .unwrap_or(kind)
    }

    /// Marks every slot `before` may have loaded as not loaded; the others already are.
    unsafe fn forget_slots(&mut self, before: &StateRef<'p>) {
        let zero = self.builder.ins().iconst(I32, 0);
        for (slot, current) in before.slots.iter().enumerate() {
            if current.may_be(status::LOADED) {
                self.set_status(slot, zero);
            }
        }
    }

    // Edges and deoptimization.

    /// The block to branch to for the edge from `position` to `target`: the target block
    /// itself, or a new block that first converts the values whose representation differs.
    fn edge(&mut self, position: usize, target: usize) -> Result<Label, Refusal> {
        let state = self
            .plan
            .edges(position)
            .iter()
            .find(|(pc, _)| *pc == target)
            .map(|&(_, state)| state)
            .ok_or(Refusal("an edge the analysis did not see"))?;
        let block = self
            .plan
            .block_at(target)
            .ok_or(Refusal("a jump into an instruction"))?;
        let label = self.labels[block].ok_or(Refusal("a jump to unreachable code"))?;
        let entry = self.plan.state(
            self.plan.blocks[block]
                .entry
                .ok_or(Refusal("a jump to unreachable code"))?,
        );
        let from = self.plan.state(state);
        let registers = from
            .registers
            .iter()
            .zip(entry.registers)
            .any(|(from, to)| converts(*from, *to));
        let slots = from
            .slots
            .iter()
            .zip(entry.slots)
            .any(|(from, to)| from.may_be(status::LOADED) && converts(from.ty, to.ty));
        if !registers && !slots {
            return Ok(label);
        }
        let converting = self.builder.create_block();
        self.edges.push(PendingEdge {
            label: converting,
            state,
            target: block,
        });
        Ok(converting)
    }

    unsafe fn fill_edges(&mut self) -> Result<(), Refusal> {
        let edges = std::mem::take(&mut self.edges);
        for edge in edges {
            self.builder.switch_to_block(edge.label);
            let entry = self.plan.state(
                self.plan.blocks[edge.target]
                    .entry
                    .ok_or(Refusal("a jump to unreachable code"))?,
            );
            let state = self.plan.state(edge.state);
            for (register, (&from, &to)) in state.registers.iter().zip(entry.registers).enumerate()
            {
                if !converts(from, to) {
                    continue;
                }
                if to == Ty::Num {
                    let value = self.as_double(register, from);
                    let flag = self.builder.ins().iconst(I32, i64::from(from == Ty::Int));
                    self.def_register(register, Ty::Num, value);
                    self.builder.def_var(self.integers[register], flag);
                } else {
                    let boxed = self.box_register(register, from);
                    self.store_register(register, boxed);
                }
            }
            for (slot, (from, to)) in state.slots.iter().zip(entry.slots).enumerate() {
                if !from.may_be(status::LOADED) || !converts(from.ty, to.ty) {
                    continue;
                }
                if to.ty == Ty::Num {
                    let value = self.slot_as_double(slot, from.ty);
                    let flag = self
                        .builder
                        .ins()
                        .iconst(I32, i64::from(from.ty == Ty::Int));
                    self.builder.def_var(self.slot_doubles[slot], value);
                    self.builder.def_var(self.slot_integers[slot], flag);
                } else {
                    self.box_slot(slot, from.ty);
                }
            }
            let label = self.labels[edge.target].ok_or(Refusal("a jump to unreachable code"))?;
            self.builder.ins().jump(label, &[]);
        }
        Ok(())
    }

    /// Moves a loaded slot's unboxed value into its root cell. A dirty slot owns a reference.
    ///
    /// The box is a fresh object, not the one the frame binds, so a local slot becomes dirty:
    /// an element write into the box edits it in place, and only a write-back makes the frame
    /// see that. The analysis marks the join it flows into as possibly dirty to match.
    unsafe fn box_slot(&mut self, slot: usize, ty: Ty) {
        let status = self.status(slot);
        let loaded = self.builder.ins().icmp_imm_s(IntCC::NotEqual, status, 0);
        let boxing = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.ins().brif(loaded, boxing, &[], done, &[]);
        self.builder.switch_to_block(boxing);
        let boxed = if ty == Ty::Num {
            let value = self.builder.use_var(self.slot_doubles[slot]);
            let flag = self.builder.use_var(self.slot_integers[slot]);
            self.box_num(value, flag)
        } else {
            let value = self.use_slot(slot, ty);
            self.box_value(ty, value)
        };
        self.store_slot_cell(slot, boxed);
        let local = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_LOCAL);
        let dirty_status = self.builder.ins().iconst(I32, status::RUNTIME_DIRTY);
        let status = self.builder.ins().select(local, dirty_status, status);
        self.set_status(slot, status);
        let dirty = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
        self.increment_if(boxed, dirty);
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
    }

    fn deopt(
        &mut self,
        pc: usize,
        state: StateId,
        boxed: Option<usize>,
        live: usize,
        why: i32,
    ) -> Label {
        let label = self.builder.create_block();
        self.deopts.push(Deopt {
            label,
            pc,
            state,
            boxed,
            live,
            why,
        });
        label
    }

    /// The registers and slots `deopt` writes back.
    fn exit(&self, deopt: &Deopt) -> Exit {
        let state = self.plan.state(deopt.state);
        let registers = state
            .registers
            .iter()
            .enumerate()
            .filter(|&(register, ty)| {
                ty.in_variables()
                    && deopt.boxed != Some(register)
                    && self.plan.live.get(deopt.live, register)
            })
            .map(|(register, &ty)| (register as u32, exit_kind(ty)))
            .collect();
        let slots = state
            .slots
            .iter()
            .enumerate()
            .filter(|(_, current)| current.may_be(status::DIRTY))
            .map(|(slot, current)| (slot as u32, exit_kind(current.ty)))
            .collect();
        Exit {
            pc: deopt.pc,
            why: deopt.why,
            registers,
            slots,
        }
    }

    /// Stores the raw values `exit` lists into the scratch area and jumps to the shared exit.
    fn emit_deopt(
        &mut self,
        deopt: &Deopt,
        exit: &Exit,
        number: usize,
        scratch: StackSlot,
        shared: Label,
    ) {
        self.builder.switch_to_block(deopt.label);
        self.builder.set_cold_block(deopt.label);
        let mut cell = 0;
        let mut store = |builder: &mut Builder, value: Value| {
            builder.ins().stack_store(I64, value, scratch, 8 * cell);
            cell += 1;
        };
        for &(register, kind) in &exit.registers {
            let register = register as usize;
            match kind {
                representation::DOUBLE => {
                    let value = self.builder.use_var(self.doubles[register]);
                    store(&mut self.builder, value);
                }
                representation::NUM => {
                    let value = self.builder.use_var(self.doubles[register]);
                    store(&mut self.builder, value);
                    let flag = self.builder.use_var(self.integers[register]);
                    store(&mut self.builder, flag);
                }
                _ => {
                    let value = self.builder.use_var(self.integers[register]);
                    store(&mut self.builder, value);
                }
            }
        }
        for &(slot, kind) in &exit.slots {
            let slot = slot as usize;
            let status = self.status(slot);
            store(&mut self.builder, status);
            match kind {
                representation::BOXED => {}
                representation::DOUBLE => {
                    let value = self.builder.use_var(self.slot_doubles[slot]);
                    store(&mut self.builder, value);
                }
                representation::NUM => {
                    let value = self.builder.use_var(self.slot_doubles[slot]);
                    store(&mut self.builder, value);
                    let flag = self.builder.use_var(self.slot_integers[slot]);
                    store(&mut self.builder, flag);
                }
                _ => {
                    let value = self.builder.use_var(self.slot_integers[slot]);
                    store(&mut self.builder, value);
                }
            }
        }
        // Tier 0 reads a loop's words only while it runs inside that loop, so an exit writes
        // the loops around its pc and no others. Writing every loop made each exit's size grow
        // with the body's loop count, and kept every loop variable live across the whole body.
        // SAFETY: the code object is rooted while it compiles, and its loop table is an
        // integer vector.
        let table = unsafe { values::ints(self.code.cdr().element(bytecode::LOOPS)) };
        for index in 0..self.plan.loops {
            let encloses = table
                .get(4 * index..4 * index + 2)
                .is_none_or(|range| (range[0] as usize..=range[1] as usize).contains(&deopt.pc));
            if !encloses {
                continue;
            }
            let words = (8 * runtime::LOOP_WORDS * index) as i32;
            let state = [
                self.loop_indexes[index],
                self.loop_counts[index],
                self.loop_firsts[index],
                self.loop_steps[index],
            ];
            for (word, variable) in state.into_iter().enumerate() {
                let value = self.builder.use_var(variable);
                self.builder
                    .ins()
                    .store(flags(), value, self.loops, words + 8 * word as i32);
            }
        }
        let number = iconst(&mut self.builder, number);
        self.builder.ins().jump(shared, &[number]);
    }

    /// Emits every deoptimization point and the one exit they share, which calls
    /// [`runtime::jit_deopt_exit`] with the point's number.
    fn emit_deopts(&mut self) -> Box<[Exit]> {
        let deopts = std::mem::take(&mut self.deopts);
        if deopts.is_empty() {
            return Box::default();
        }
        let exits: Box<[Exit]> = deopts.iter().map(|deopt| self.exit(deopt)).collect();
        let cells = exits.iter().map(Exit::cells).max().unwrap_or(0).max(1);
        let scratch = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (8 * cells) as u32,
            3,
        ));
        let shared = self.builder.create_block();
        self.builder.append_block_param(shared, I64);
        self.builder.set_cold_block(shared);
        for (number, (deopt, exit)) in deopts.iter().zip(&exits).enumerate() {
            self.emit_deopt(deopt, exit, number, scratch, shared);
        }
        self.builder.switch_to_block(shared);
        let number = self.builder.block_params(shared)[0];
        let pointer = self.builder.ins().stack_addr(I64, scratch, 0);
        let result = self.call(
            runtime::jit_deopt_exit as *const () as usize,
            &[I64, I64, I64],
            Some(I64),
            &[self.activation, pointer, number],
        );
        let result = result.unwrap_or(number);
        self.builder.ins().return_(&[result]);
        exits
    }

    fn guard(&mut self, condition: Value, failed: Label) {
        self.builder.ins().guard(condition, failed, &[]);
    }

    fn next_pc(&self, position: usize) -> Result<usize, Refusal> {
        self.plan
            .insts
            .get(position + 1)
            .map(|inst| inst.pc)
            .ok_or(Refusal("the body falls off its end"))
    }

    fn after(&self, position: usize) -> Result<StateId, Refusal> {
        let next = self.next_pc(position)?;
        self.plan
            .edges(position)
            .iter()
            .find(|(pc, _)| *pc == next)
            .map(|&(_, state)| state)
            .ok_or(Refusal("an instruction without a fallthrough state"))
    }

    /// The state after an instruction that ran in tier 0, whose destination is still boxed.
    fn after_with_boxed(
        &self,
        position: usize,
        dst: Option<usize>,
    ) -> Result<(StateId, Option<usize>), Refusal> {
        Ok((self.after(position)?, dst))
    }

    fn set_visible(&mut self, before: &StateRef<'p>, flag: bool) {
        if before.visible != Some(flag) {
            let value = self.builder.ins().iconst(I8, flag as i64);
            self.builder.ins().store(flags(), value, self.visible, 0);
        }
    }

    // Values.

    /// Whether `value` is a record (or tuple, tag, dict or set) without a class.
    fn plain_record(&mut self, value: Value) -> Value {
        let kind = self.builder.ins().uload8(I32, flags(), value, KIND);
        let is_record = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, kind, SexpType::Record as i64);
        let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
        let object = self.builder.ins().band_imm_u(bits, OBJECT_FLAG);
        let plain = self.builder.ins().icmp_imm_s(IntCC::Equal, object, 0);
        self.builder.ins().band(is_record, plain)
    }

    fn load_register(&mut self, register: usize) -> Value {
        self.builder
            .ins()
            .load(I64, flags(), self.window, (8 * register) as i32)
    }

    fn store_register(&mut self, register: usize, value: Value) {
        self.builder
            .ins()
            .store(flags(), value, self.window, (8 * register) as i32);
    }

    fn load_slot_cell(&mut self, slot: usize) -> Value {
        self.builder.ins().load(
            I64,
            flags(),
            self.window,
            (8 * (self.plan.registers + slot)) as i32,
        )
    }

    fn store_slot_cell(&mut self, slot: usize, value: Value) {
        self.builder.ins().store(
            flags(),
            value,
            self.window,
            (8 * (self.plan.registers + slot)) as i32,
        );
    }

    fn use_register(&mut self, register: usize, ty: Ty) -> Value {
        match ty {
            Ty::Dbl | Ty::Num => self.builder.use_var(self.doubles[register]),
            _ => self.builder.use_var(self.integers[register]),
        }
    }

    fn def_register(&mut self, register: usize, ty: Ty, value: Value) {
        match ty {
            Ty::Dbl | Ty::Num => self.builder.def_var(self.doubles[register], value),
            _ => self.builder.def_var(self.integers[register], value),
        }
    }

    fn use_slot(&mut self, slot: usize, ty: Ty) -> Value {
        match ty {
            Ty::Dbl | Ty::Num => self.builder.use_var(self.slot_doubles[slot]),
            _ => self.builder.use_var(self.slot_integers[slot]),
        }
    }

    /// A register's value as a double, converting integers and logicals with `NA` kept.
    fn as_double(&mut self, register: usize, ty: Ty) -> Value {
        let value = self.use_register(register, ty);
        if matches!(ty, Ty::Dbl | Ty::Num) {
            return value;
        }
        self.integer_as_double(value)
    }

    /// A slot's value as a double, like [`Self::as_double`].
    fn slot_as_double(&mut self, slot: usize, ty: Ty) -> Value {
        let value = self.use_slot(slot, ty);
        if matches!(ty, Ty::Dbl | Ty::Num) {
            return value;
        }
        self.integer_as_double(value)
    }

    fn integer_as_double(&mut self, value: Value) -> Value {
        let missing = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, value, NA_INTEGER as i64);
        let converted = self.builder.ins().fcvt_from_sint(F64, value);
        let na = self.builder.ins().f64const(Ieee64::with_bits(NA_REAL_BITS));
        self.builder.ins().select(missing, na, converted)
    }

    /// `value`, or when it is a compact sequence where `ty` expects a plain vector, a plain copy
    /// of it that the slot and `dst` hold from then on: a formal declared `integer[]` called with
    /// `1:n` would otherwise fail the guard on every first call. Anything else that does not fit
    /// jumps to `mismatch`. The second value says whether the slot now holds a copy.
    fn sequence_contents(
        &mut self,
        value: Value,
        ty: Ty,
        slot: usize,
        dst: Option<usize>,
        mismatch: Label,
    ) -> Result<(Value, Value), Refusal> {
        let fits = self.check(value, ty);
        let join = self.builder.create_block();
        self.builder.append_block_param(join, I64);
        self.builder.append_block_param(join, I8);
        let convert = self.builder.create_block();
        self.builder.set_cold_block(convert);
        let kept = self.builder.ins().iconst(I8, 0);
        self.builder
            .ins()
            .brif(fits, join, &[value, kept], convert, &[]);
        self.builder.switch_to_block(convert);
        let kind = if ty == Ty::IntVec {
            SexpType::Integer
        } else {
            SexpType::Real
        };
        let kind = self.builder.ins().iconst(I64, kind as i64);
        let contents = self
            .call(
                runtime::jit_sequence_contents as *const () as usize,
                &[I64, I64, I64],
                Some(I64),
                &[self.activation, value, kind],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let converted = self.builder.create_block();
        self.builder
            .ins()
            .brif(contents, converted, &[], mismatch, &[]);
        self.builder.switch_to_block(converted);
        self.store_slot_cell(slot, contents);
        if let Some(dst) = dst {
            self.store_register(dst, contents);
        }
        let copied = self.builder.ins().iconst(I8, 1);
        self.builder.ins().jump(join, &[contents, copied]);
        self.builder.switch_to_block(join);
        let params = self.builder.block_params(join);
        Ok((params[0], params[1]))
    }

    /// Whether the object `value` points to has type `ty`.
    fn check(&mut self, value: Value, ty: Ty) -> Value {
        let (kind, scalar) = match ty {
            Ty::Fun(function) => {
                return self.builder.ins().icmp_imm_s(
                    IntCC::Equal,
                    value,
                    function.as_ptr() as i64,
                );
            }
            Ty::Closure(formals, body) => {
                let formals = self.builder.ins().iconst(I64, formals.as_ptr() as i64);
                let body = self.builder.ins().iconst(I64, body.as_ptr() as i64);
                // A helper call without a result cannot happen for a helper declared with one,
                // and failing the check keeps the code correct if it ever did.
                return match self.call(
                    runtime::jit_closure_matches as *const () as usize,
                    &[I64, I64, I64, I64],
                    Some(I32),
                    &[self.activation, value, formals, body],
                ) {
                    Some(matches) => self.builder.ins().icmp_imm_s(IntCC::NotEqual, matches, 0),
                    None => self.builder.ins().iconst(I8, 0),
                };
            }
            // Tier 1 never speculates on a record type, which only native functions hold.
            Ty::Rec(_) | Ty::Obj => return self.builder.ins().iconst(I8, 0),
            Ty::Int | Ty::Num => (SexpType::Integer, true),
            Ty::Dbl => (SexpType::Real, true),
            Ty::Lgl => (SexpType::Logical, true),
            Ty::IntVec => (SexpType::Integer, false),
            Ty::DblVec => (SexpType::Real, false),
            Ty::LglVec => (SexpType::Logical, false),
            Ty::List => (SexpType::List, false),
            Ty::IntSeq | Ty::DblSeq => {
                // `a:b` gives a plain length-one vector when `a == b`, as in GNU R, so a site
                // that makes compact sequences also makes those, and both pass: the length-one
                // vector reads as a sequence of one element with increment zero (see
                // `sequence_info`). An ALTREP instance's class is its tag, which no ordinary
                // vector has, so the flag is tested before the tag is read.
                let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
                let altrep = self.builder.ins().band_imm_u(bits, ALTREP_FLAG);
                let altrep = self.builder.ins().icmp_imm_s(IntCC::NotEqual, altrep, 0);
                let decided = self.builder.create_block();
                let instance = self.builder.create_block();
                let scalar = self.builder.create_block();
                self.builder.append_block_param(decided, I8);
                self.builder.ins().brif(altrep, instance, &[], scalar, &[]);
                self.builder.switch_to_block(instance);
                let class = self.builder.ins().load(I64, flags(), value, TAG);
                let [integer, double] = interp::interp().compact_sequence_classes;
                let sequence = if ty == Ty::IntSeq { integer } else { double };
                let fits =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::Equal, class, sequence.as_ptr() as i64);
                let attributes = self.builder.ins().load(I64, flags(), value, ATTRIB);
                let plain = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, attributes, self.nil);
                let fits = self.builder.ins().band(fits, plain);
                self.builder.ins().jump(decided, &[fits]);
                self.builder.switch_to_block(scalar);
                let kind = if ty == Ty::IntSeq {
                    SexpType::Integer
                } else {
                    SexpType::Real
                };
                let actual = self.builder.ins().uload8(I32, flags(), value, KIND);
                let fits = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, actual, kind as i64);
                let length = self.builder.ins().load(I64, flags(), value, LENGTH);
                let single = self.builder.ins().icmp_imm_s(IntCC::Equal, length, 1);
                let attributes = self.builder.ins().load(I64, flags(), value, ATTRIB);
                let plain = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, attributes, self.nil);
                let fits = self.builder.ins().band(fits, single);
                let fits = self.builder.ins().band(fits, plain);
                self.builder.ins().jump(decided, &[fits]);
                self.builder.switch_to_block(decided);
                return self.builder.block_params(decided)[0];
            }
            Ty::IntArr | Ty::DblArr | Ty::LglArr => {
                // The Array bit promises no attributes, so the kind and the bits decide it.
                let kind = match ty {
                    Ty::IntArr => SexpType::Integer,
                    Ty::DblArr => SexpType::Real,
                    _ => SexpType::Logical,
                };
                let actual = self.builder.ins().uload8(I32, flags(), value, KIND);
                let fits = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, actual, kind as i64);
                let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
                let bits = self
                    .builder
                    .ins()
                    .band_imm_u(bits, ALTREP_FLAG | ARRAY_FLAG);
                let array = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::Equal, bits, ARRAY_FLAG);
                return self.builder.ins().band(fits, array);
            }
            Ty::Boxed | Ty::Bottom => return self.builder.ins().iconst(I8, 1),
        };
        let actual = self.builder.ins().uload8(I32, flags(), value, KIND);
        let mut fits = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, actual, kind as i64);
        if ty == Ty::Num {
            let real = self
                .builder
                .ins()
                .icmp_imm_s(IntCC::Equal, actual, SexpType::Real as i64);
            fits = self.builder.ins().bor(fits, real);
        }
        let attributes = self.builder.ins().load(I64, flags(), value, ATTRIB);
        let plain = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, attributes, self.nil);
        fits = self.builder.ins().band(fits, plain);
        let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
        let altrep = self
            .builder
            .ins()
            .band_imm_u(bits, ALTREP_FLAG | ARRAY_FLAG);
        let ordinary = self.builder.ins().icmp_imm_s(IntCC::Equal, altrep, 0);
        fits = self.builder.ins().band(fits, ordinary);
        if scalar {
            let length = self.builder.ins().load(I64, flags(), value, LENGTH);
            let single = self.builder.ins().icmp_imm_s(IntCC::Equal, length, 1);
            fits = self.builder.ins().band(fits, single);
        }
        fits
    }

    /// Whether `value` is a loop's compact sequence of the kind `ty` expects: tier 0 or the
    /// sequence helper gave the loop a nonzero `step` only for one, and a compact sequence has no
    /// attributes, since it is copied before it is changed.
    fn compact_sequence_fits(&mut self, value: Value, ty: Ty, step: Value) -> Value {
        let kind = match ty {
            Ty::IntVec => SexpType::Integer,
            Ty::DblVec => SexpType::Real,
            _ => return self.builder.ins().iconst(I8, 0),
        };
        let zero = self.builder.ins().f64const(0.0);
        let compact = self.builder.ins().fcmp(FloatCC::NotEqual, step, zero);
        let actual = self.builder.ins().uload8(I32, flags(), value, KIND);
        let fits = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, actual, kind as i64);
        self.builder.ins().band(fits, compact)
    }

    /// Records that `slot` now holds the compact sequence `value` (or the length-one vector its
    /// guard also passes), with its length, first element and increment, so that reads of the
    /// slot find them in variables instead of loading them from the info vector each time.
    /// Every site that gives a slot a sequence type calls this, so the variables change only
    /// where the slot does, and a loop that only reads the slot reads loop-invariant values.
    /// The length, first element and increment of the compact sequence `value`, all doubles,
    /// read from the `c(n, n1, inc)` vector of its first data slot. An ALTREP instance keeps
    /// that slot where an ordinary vector keeps its length. The length stays a double because
    /// an element read compares against it on every iteration, and a saturating conversion to
    /// an integer costs more than the read itself.
    fn sequence_info(&mut self, value: Value, ty: Ty) -> (Value, Value, Value) {
        // A compact sequence's state is the double vector `c(n, n1, inc)` in its first data
        // slot; the plain length-one vector its guard also passes is `(1, x[1], 0)`.
        let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
        let altrep = self.builder.ins().band_imm_u(bits, ALTREP_FLAG);
        let altrep = self.builder.ins().icmp_imm_s(IntCC::NotEqual, altrep, 0);
        let compact = self.builder.create_block();
        let scalar = self.builder.create_block();
        let join = self.builder.create_block();
        for _ in 0..3 {
            self.builder.append_block_param(join, F64);
        }
        self.builder.set_cold_block(scalar);
        self.builder.ins().brif(altrep, compact, &[], scalar, &[]);
        self.builder.switch_to_block(compact);
        let info = self.builder.ins().load(I64, flags(), value, CAR);
        let count = self.builder.ins().load(F64, flags(), info, DATA);
        let first = self.builder.ins().load(F64, flags(), info, DATA + 8);
        let step = self.builder.ins().load(F64, flags(), info, DATA + 16);
        self.builder.ins().jump(join, &[count, first, step]);
        self.builder.switch_to_block(scalar);
        let count = self.builder.ins().f64const(1.0);
        let first = if ty == Ty::IntSeq {
            let element = self.builder.ins().load(I32, flags(), value, DATA);
            self.builder.ins().fcvt_from_sint(F64, element)
        } else {
            self.builder.ins().load(F64, flags(), value, DATA)
        };
        let step = self.builder.ins().f64const(0.0);
        self.builder.ins().jump(join, &[count, first, step]);
        self.builder.switch_to_block(join);
        let params = self.builder.block_params(join).to_vec();
        (params[0], params[1], params[2])
    }

    /// Whether a double position at least one is within `length`. R truncates the position,
    /// so a vector accepts anything below `length + 1`; an Array rejects a position past its
    /// last item before truncating.
    fn within_length(&mut self, index: Value, length: Value, array: bool) -> Value {
        let bound = self.builder.ins().fcvt_from_sint(F64, length);
        if array {
            return self
                .builder
                .ins()
                .fcmp(FloatCC::LessThanOrEqual, index, bound);
        }
        let one = self.builder.ins().f64const(1.0);
        let bound = self.builder.ins().fadd(bound, one);
        self.builder.ins().fcmp(FloatCC::LessThan, index, bound)
    }

    fn unbox(&mut self, value: Value, ty: Ty) -> Value {
        let load_ty = if ty == Ty::Dbl { F64 } else { I32 };
        self.builder.ins().load(load_ty, flags(), value, DATA)
    }

    /// An integer or double scalar's `(value, flag)` pair, as [`Ty::Num`] holds it.
    fn unbox_num(&mut self, value: Value) -> (Value, Value) {
        let kind = self.builder.ins().uload8(I32, flags(), value, KIND);
        let integer = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, kind, SexpType::Integer as i64);
        let from_integer = self.builder.create_block();
        let from_double = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.append_block_param(done, F64);
        self.builder
            .ins()
            .brif(integer, from_integer, &[], from_double, &[]);
        self.builder.switch_to_block(from_integer);
        let unboxed = self.builder.ins().load(I32, flags(), value, DATA);
        let widened = self.integer_as_double(unboxed);
        self.builder.ins().jump(done, &[widened]);
        self.builder.switch_to_block(from_double);
        let unboxed = self.builder.ins().load(F64, flags(), value, DATA);
        self.builder.ins().jump(done, &[unboxed]);
        self.builder.switch_to_block(done);
        let double = self.builder.block_params(done)[0];
        (double, self.builder.ins().uextend(I32, integer))
    }

    /// The integer a [`Ty::Num`] whose flag says integer holds, `NA` included.
    fn num_to_integer(&mut self, value: Value) -> Value {
        let missing = self.builder.ins().fcmp(FloatCC::Unordered, value, value);
        let narrowed = self.builder.ins().fcvt_to_sint_sat(I32, value);
        let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
        self.builder.ins().select(missing, na, narrowed)
    }

    /// Boxes a register held in variables.
    fn box_register(&mut self, register: usize, ty: Ty) -> Value {
        if ty == Ty::Num {
            let value = self.builder.use_var(self.doubles[register]);
            let flag = self.builder.use_var(self.integers[register]);
            return self.box_num(value, flag);
        }
        let value = self.use_register(register, ty);
        self.box_value(ty, value)
    }

    fn box_num(&mut self, value: Value, flag: Value) -> Value {
        self.call(
            runtime::jit_box_num as *const () as usize,
            &[I64, F64, I32],
            Some(I64),
            &[self.activation, value, flag],
        )
        .unwrap_or(value)
    }

    fn box_value(&mut self, ty: Ty, value: Value) -> Value {
        let (helper, param) = match ty {
            Ty::Int => (runtime::jit_box_integer as *const () as usize, I32),
            Ty::Dbl => (runtime::jit_box_double as *const () as usize, F64),
            _ => (runtime::jit_box_logical as *const () as usize, I32),
        };
        self.call(helper, &[I64, param], Some(I64), &[self.activation, value])
            .unwrap_or(value)
    }

    fn increment(&mut self, value: Value) {
        let yes = self.builder.ins().iconst(I8, 1);
        self.increment_if(value, yes);
    }

    /// Counts a reference to `value` when `condition` holds, saturating like the heap does.
    fn increment_if(&mut self, value: Value, condition: Value) {
        let count = self.builder.ins().uload16(I32, flags(), value, REFCOUNT);
        let saturated = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, count, REFCOUNT_MAX as i64);
        let not_saturated = self.builder.ins().bxor_imm_u(saturated, 1);
        let counting = self.builder.ins().band(condition, not_saturated);
        let raised = self.builder.ins().iadd_imm_s(count, 1);
        let new_count = self.builder.ins().select(counting, raised, count);
        self.builder
            .ins()
            .istore16(flags(), new_count, value, REFCOUNT);
    }

    fn decrement_if(&mut self, value: Value, condition: Value) {
        let count = self.builder.ins().uload16(I32, flags(), value, REFCOUNT);
        let saturated = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, count, REFCOUNT_MAX as i64);
        let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, count, 0);
        let stuck = self.builder.ins().bor(saturated, zero);
        let free = self.builder.ins().bxor_imm_u(stuck, 1);
        let counting = self.builder.ins().band(condition, free);
        let lowered = self.builder.ins().iadd_imm_s(count, -1);
        let new_count = self.builder.ins().select(counting, lowered, count);
        self.builder
            .ins()
            .istore16(flags(), new_count, value, REFCOUNT);
    }

    /// Calls [`runtime::jit_fast_step`] for the element op at `pc` when `fast` says tier 0's
    /// fast handler may apply, then [`runtime::jit_quiet_step`] where it did not, and branches
    /// on what they did, to the returned blocks: where the fast handler ran the op, where the
    /// generic handler did, and where neither did. An error leaves through `failed`.
    fn fast_step(&mut self, pc: usize, fast: bool, failed: Label) -> Result<[Label; 3], Refusal> {
        let pc = iconst(&mut self.builder, pc);
        let [handled, quiet, stepped, declining, declined] =
            std::array::from_fn(|_| self.builder.create_block());
        if fast {
            let applied = self
                .call(
                    runtime::jit_fast_step as *const () as usize,
                    &[I64, I64],
                    Some(I32),
                    &[self.activation, pc],
                )
                .ok_or(Refusal("a helper without a result"))?;
            self.builder.ins().brif(applied, handled, &[], quiet, &[]);
        } else {
            self.builder.ins().jump(quiet, &[]);
        }
        self.builder.switch_to_block(quiet);
        let result = self
            .call(
                runtime::jit_quiet_step as *const () as usize,
                &[I64, I64],
                Some(I32),
                &[self.activation, pc],
            )
            .ok_or(Refusal("a helper without a result"))?;
        let ran = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, result, quiet::STEPPED as i64);
        self.builder.ins().brif(ran, stepped, &[], declining, &[]);
        self.builder.switch_to_block(declining);
        let nothing = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::Equal, result, quiet::DECLINED as i64);
        self.guard(nothing, failed);
        self.builder.ins().jump(declined, &[]);
        Ok([handled, stepped, declined])
    }

    /// Runs the tier-0 instruction at `pc` through [`runtime::jit_step`].
    fn call_step(&mut self, pc: usize, validate: Value) -> Result<Value, Refusal> {
        let pc = iconst(&mut self.builder, pc);
        self.call(
            runtime::jit_step as *const () as usize,
            &[I64, I64, I32],
            Some(I32),
            &[self.activation, pc, validate],
        )
        .ok_or(Refusal("a helper without a result"))
    }

    fn call(
        &mut self,
        address: usize,
        params: &[Type],
        result: Option<Type>,
        args: &[Value],
    ) -> Option<Value> {
        let key = (params.to_vec(), result);
        let signature = match self.signatures.get(&key) {
            Some(signature) => *signature,
            None => {
                let mut signature = Signature::new(self.call_conv);
                signature
                    .params
                    .extend(params.iter().map(|ty| AbiParam::new(*ty)));
                signature.returns.extend(result.map(AbiParam::new));
                let reference = self.builder.import_signature(signature);
                self.signatures.insert(key, reference);
                reference
            }
        };
        let callee = self.builder.ins().iconst(I64, address as i64);
        let call = self.builder.ins().call_indirect(signature, callee, args);
        self.builder.inst_results(call).first().copied()
    }
}

/// Whether a value of type `from` changes representation on an edge into a block expecting
/// `to`: an unboxed value boxed for a block that expects an object, or an integer or double
/// widened for one that expects either.
fn converts(from: Ty, to: Ty) -> bool {
    if to == Ty::Num {
        return matches!(from, Ty::Int | Ty::Dbl);
    }
    from.in_variables() && to != Ty::Bottom && !to.in_variables()
}

fn iconst(builder: &mut Builder, value: usize) -> Value {
    builder.ins().iconst(I64, value as i64)
}

unsafe fn runtime_constant(code: Sexp, index: usize) -> Sexp {
    unsafe { code.cdr().element(index) }
}
