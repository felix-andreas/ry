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

use crate::{
    analysis::{Action, Argument, Effect, Intrinsic, Numeric, Plan, Refusal, State, Ty, jump_target, status},
    runtime::{self, Activation, offset, outcome, reason, representation},
};
use cranelift_codegen::{

    ir::{
        AbiParam, Block as Label, InstBuilder, MemFlagsData, SigRef, Signature, StackSlotData, StackSlotKind, Type, Value,
        condcodes::{FloatCC, IntCC},
        immediates::Ieee64,
        types::{F64, I8, I32, I64},
    },
    isa::CallConv,
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Module, default_libcall_names};
use heap::object::{NA_INTEGER, NA_REAL_BITS, REFCOUNT_MAX, Sexp, SexpType, VECTOR_DATA_OFFSET};
use interp::{
    bytecode::{Arith, Op},
    values,
};
use rustc_hash::FxHashMap;
use std::{cell::RefCell, sync::atomic::AtomicBool};

/// The compiled entry point.
pub type Entry = unsafe extern "C" fn(*mut Activation) -> *mut heap::object::Header;

thread_local! {
    /// One module for the session. Cranelift's JIT can only free all of its code at once, so
    /// code for discarded compilations stays mapped until the process exits.
    static MODULE: RefCell<Option<JITModule>> = const { RefCell::new(None) };
}

/// Header offsets, as `heap::object` lays objects out.
const KIND: i32 = 0;
const FLAGS: i32 = 2;
const REFCOUNT: i32 = 6;
const ATTRIB: i32 = 8;
const LENGTH: i32 = 16;
const DATA: i32 = VECTOR_DATA_OFFSET as i32;
const ALTREP_FLAG: i64 = 1 << 1;
const OBJECT_FLAG: i64 = 1 << 0;

pub unsafe fn compile(plan: &Plan, code: Sexp) -> Result<Entry, Refusal> {
    MODULE.with(|cell| {
        let mut module_slot = cell.borrow_mut();
        if module_slot.is_none() {
            *module_slot = Some(new_module()?);
        }
        let module = module_slot.as_mut().ok_or(Refusal("no code module"))?;
        let call_conv = module.isa().default_call_conv();
        let frontend_config = module.isa().frontend_config();
        let mut context = module.make_context();
        context.func.signature = Signature::new(call_conv);
        context.func.signature.params.push(AbiParam::new(I64));
        context.func.signature.returns.push(AbiParam::new(I64));
        let mut function_context = FunctionBuilderContext::new();
        {
            let builder = FunctionBuilder::new(&mut context.func, &mut function_context);
            let mut translator = Translator::new(plan, code, builder, call_conv);
            translator.translate()?;
            translator.builder.seal_all_blocks();
            translator.builder.finalize(frontend_config);
        }
        if std::env::var_os("RY_JIT_DUMP").is_some() {
            eprintln!("{}", context.func.display());
        }
        let id = module
            .declare_anonymous_function(&context.func.signature)
            .map_err(|_| Refusal("Cranelift rejected the function declaration"))?;
        if let Err(failure) = module.define_function(id, &mut context) {
            if std::env::var_os("RY_JIT_DUMP").is_some() {
                eprintln!("{failure:?}");
            }
            return Err(Refusal("Cranelift rejected the function"));
        }
        module.clear_context(&mut context);
        module.finalize_definitions().map_err(|_| Refusal("Cranelift could not finalize the code"))?;
        let pointer = module.get_finalized_function(id);
        // SAFETY: the function was built with exactly this signature.
        Ok(unsafe { std::mem::transmute::<*const u8, Entry>(pointer) })
    })
}

fn new_module() -> Result<JITModule, Refusal> {
    let mut flags = settings::builder();
    let configure = |flags: &mut settings::Builder| -> Result<(), settings::SetError> {
        flags.set("opt_level", "speed")?;
        flags.set("use_colocated_libcalls", "false")?;
        flags.set("is_pic", "false")?;
        // The verifier rechecks every function's IR and costs as much as the rest of
        // compilation, so only debug builds, where the tests run, pay for it.
        flags.set("enable_verifier", if cfg!(debug_assertions) { "true" } else { "false" })?;
        Ok(())
    };
    configure(&mut flags).map_err(|_| Refusal("Cranelift rejected the settings"))?;
    let isa = cranelift_native::builder()
        .map_err(|_| Refusal("this host has no Cranelift backend"))?
        .finish(settings::Flags::new(flags))
        .map_err(|_| Refusal("Cranelift could not configure the host"))?;
    Ok(JITModule::new(JITBuilder::with_isa(isa, default_libcall_names())))
}

/// A cold block that hands the activation back to the interpreter.
struct Deopt {
    label: Label,
    pc: usize,
    state: State,
    live: Vec<bool>,
    why: i32,
}

/// An edge that converts representations before reaching its target block.
struct PendingEdge {
    label: Label,
    state: State,
    target: usize,
}

struct Translator<'p, 'b> {
    plan: &'p Plan,
    code: Sexp,
    builder: FunctionBuilder<'b>,
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
    statuses: Vec<Variable>,
    loop_indexes: Vec<Variable>,
    loop_counts: Vec<Variable>,
    labels: Vec<Option<Label>>,
    deopts: Vec<Deopt>,
    edges: Vec<PendingEdge>,
    nil: i64,
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

impl<'p, 'b> Translator<'p, 'b> {
    fn new(plan: &'p Plan, code: Sexp, mut builder: FunctionBuilder<'b>, call_conv: CallConv) -> Translator<'p, 'b> {
        let mut declare = |count: usize, ty: Type| (0..count).map(|_| builder.declare_var(ty)).collect::<Vec<_>>();
        let integers = declare(plan.registers + plan.temporaries, I32);
        let doubles = declare(plan.registers + plan.temporaries, F64);
        let slot_integers = declare(plan.slots.len(), I32);
        let slot_doubles = declare(plan.slots.len(), F64);
        let statuses = declare(plan.slots.len(), I32);
        let loop_indexes = declare(plan.loops, I64);
        let loop_counts = declare(plan.loops, I64);
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
            statuses,
            loop_indexes,
            loop_counts,
            labels: Vec::new(),
            deopts: Vec::new(),
            edges: Vec::new(),
            nil: values::nil().as_ptr() as i64,
        }
    }

    unsafe fn translate(&mut self) -> Result<(), Refusal> {
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        self.activation = self.builder.block_params(entry)[0];
        self.window = self.builder.ins().load(I64, flags(), self.activation, offset::REGISTERS);
        self.loops = self.builder.ins().load(I64, flags(), self.activation, offset::LOOPS);
        self.visible = self.builder.ins().load(I64, flags(), self.activation, offset::VISIBLE);
        let entry_pc = self.builder.ins().load(I64, flags(), self.activation, offset::ENTRY_PC);
        let zero32 = self.builder.ins().iconst(I32, 0);
        let zero64 = self.builder.ins().iconst(I64, 0);
        let zero_double = self.builder.ins().f64const(0.0);
        for variable in self.integers.iter().chain(&self.slot_integers).chain(&self.statuses) {
            self.builder.def_var(*variable, zero32);
        }
        for variable in self.doubles.iter().chain(&self.slot_doubles) {
            self.builder.def_var(*variable, zero_double);
        }
        for variable in self.loop_indexes.iter().chain(&self.loop_counts) {
            self.builder.def_var(*variable, zero64);
        }
        self.labels = self
            .plan
            .blocks
            .iter()
            .map(|block| block.entry.as_ref().map(|_| self.builder.create_block()))
            .collect();
        let start = self.labels.first().copied().flatten().ok_or(Refusal("the body has no reachable code"))?;
        // Dispatch on where execution starts: the top, or a loop header.
        let mut osr = Vec::new();
        for &header in &self.plan.loop_headers {
            let Some(label) = self.labels[header] else { continue };
            let pc = self.plan.insts[self.plan.blocks[header].start].pc;
            let enter = self.builder.create_block();
            let next = self.builder.create_block();
            let here = self.builder.ins().icmp_imm_s(IntCC::Equal, entry_pc, pc as i64);
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
            let Some(label) = self.labels[index] else { continue };
            self.builder.switch_to_block(label);
            let (start, end) = (self.plan.blocks[index].start, self.plan.blocks[index].end);
            for position in start..end {
                let terminated = self.instruction(position)?;
                if position + 1 == end && !terminated {
                    let next = self.plan.insts.get(end).ok_or(Refusal("the body falls off its end"))?.pc;
                    let target = self.edge(position, next)?;
                    self.builder.ins().jump(target, &[]);
                    self.fill_edges()?;
                }
            }
        }
        let deopts = std::mem::take(&mut self.deopts);
        for deopt in deopts {
            self.emit_deopt(deopt);
        }
        Ok(())
    }

    /// Enters at a loop header with tier 0's registers in the window and every variable in the
    /// environment, declining when a register's value does not have the type the loop assumes.
    unsafe fn osr_entry(&mut self, header: usize, label: Label, declined: Label) -> Result<(), Refusal> {
        for index in 0..self.plan.loops {
            let loop_index = self.builder.ins().load(I64, flags(), self.loops, (16 * index) as i32);
            let count = self.builder.ins().load(I64, flags(), self.loops, (16 * index + 8) as i32);
            self.builder.def_var(self.loop_indexes[index], loop_index);
            self.builder.def_var(self.loop_counts[index], count);
        }
        let entry = self.plan.blocks[header].entry.clone().ok_or(Refusal("an unreachable loop header"))?;
        for (register, ty) in entry.registers.iter().enumerate() {
            if matches!(ty, Ty::Bottom | Ty::Boxed) {
                continue;
            }
            let value = self.load_register(register);
            let fits = self.check(value, *ty);
            let next = self.builder.create_block();
            self.builder.ins().brif(fits, next, &[], declined, &[]);
            self.builder.switch_to_block(next);
            if ty.is_unboxed() {
                let unboxed = self.unbox(value, *ty);
                self.def_register(register, *ty, unboxed);
            }
        }
        self.builder.ins().jump(label, &[]);
        Ok(())
    }

    /// Translates one instruction; returns whether it emitted the block's terminator.
    unsafe fn instruction(&mut self, position: usize) -> Result<bool, Refusal> {
        let plan = self.plan;
        let Some(before) = plan.before[position].clone() else {
            return Ok(false);
        };
        self.action(position, before, plan.actions[position].clone())
    }

    /// Translates one action with the state before it; returns whether it emitted the block's
    /// terminator. The steps of an inlined call come here too, with the call's position, so
    /// every exit they take leaves at the call.
    unsafe fn action(&mut self, position: usize, before: State, action: Action) -> Result<bool, Refusal> {
        let plan = self.plan;
        let inst = &plan.insts[position];
        let pc = inst.pc;
        match action {
            Action::Skip | Action::FallThrough => {}
            Action::Constant { dst, value, ty } => {
                match ty {
                    Ty::Int | Ty::Lgl => {
                        let constant = self.builder.ins().iconst(I32, values::ints(value)[0] as i64);
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
            Action::GetVar { dst, slot, load, forcing } => {
                if let Some(ty) = load {
                    self.load_slot(position, &before, slot, ty, forcing)?;
                }
                let ty = load.unwrap_or(before.slots[slot].ty);
                if ty.is_unboxed() {
                    let value = self.use_slot(slot, ty);
                    self.def_register(dst, ty, value);
                } else {
                    let value = self.load_slot_cell(slot);
                    self.store_register(dst, value);
                }
                self.set_visible(&before, true);
            }
            Action::SetVar { slot, src } => {
                let ty = match before.registers[src] {
                    Ty::Bottom => Ty::Boxed,
                    ty => ty,
                };
                let value = if ty.is_unboxed() { self.use_register(src, ty) } else { self.load_register(src) };
                self.assign_slot(&before, slot, ty, value);
                self.set_visible(&before, false);
            }
            Action::GetFun { dst, function } => {
                let pointer = self.builder.ins().iconst(I64, function.as_ptr() as i64);
                self.store_register(dst, pointer);
            }
            Action::GetFunSlot { dst, slot, load } => {
                if let Some(ty) = load {
                    self.load_slot(position, &before, slot, ty, false)?;
                }
                let value = self.load_slot_cell(slot);
                self.store_register(dst, value);
            }
            Action::Inline { steps } => {
                for (step, state) in steps {
                    self.action(position, state, step)?;
                }
                self.set_visible(&before, true);
            }
            Action::Call { steps, function, call, args, dst } => {
                self.direct_call(position, before, steps, function, call, &args, dst)?;
            }
            Action::Peek { slot, ty } => self.peek(position, &before, slot, ty)?,
            Action::Move { dst, src, ty } => {
                let value = self.use_register(src, ty);
                self.def_register(dst, ty, value);
            }
            Action::Step { effect, objects, dst } => self.step(position, &before, effect, &objects, dst)?,
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
                let builtin = self.builder.ins().icmp_imm_s(IntCC::Equal, kind, SexpType::Builtin as i64);
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(builtin, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::Jump => {
                let target = jump_target(inst).ok_or(Refusal("a jump without a target"))?;
                if target <= pc {
                    // A pending interrupt hands the activation to tier 0 at this jump, which
                    // raises it.
                    let flag = &interp::unwind::INTERRUPT_REQUESTED as *const AtomicBool as i64;
                    let address = self.builder.ins().iconst(I64, flag);
                    let requested = self.builder.ins().uload8(I32, flags(), address, 0);
                    let interrupted = self.deopt(pc, before.clone(), plan.live[position].clone(), reason::INTERRUPT);
                    let calm = self.builder.ins().icmp_imm_s(IntCC::Equal, requested, 0);
                    self.guard(calm, interrupted);
                }
                let label = self.edge(position, target)?;
                self.builder.ins().jump(label, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::JumpUnless { src } => {
                let call = runtime_constant(self.code, inst.operands[1]);
                let truth = self.condition(position, &before, src, call)?;
                let target = jump_target(inst).ok_or(Refusal("a branch without a target"))?;
                let taken = self.edge(position, target)?;
                let next = self.next_pc(position)?;
                let fall = self.edge(position, next)?;
                self.builder.ins().brif(truth, fall, &[], taken, &[]);
                self.fill_edges()?;
                return Ok(true);
            }
            Action::Arithmetic { arith, numeric, dst, left, right } => {
                self.arithmetic(position, &before, arith, numeric, dst, left, right)?;
                self.set_visible(&before, true);
            }
            Action::Comparison { arith, numeric, dst, left, right } => {
                let result = self.comparison(&before, arith, numeric, left, right);
                self.builder.def_var(self.integers[dst], result);
                self.set_visible(&before, true);
            }
            Action::Intrinsic { intrinsic, dst, args, result } => {
                self.intrinsic(position, &before, intrinsic, dst, &args, result)?;
                self.set_visible(&before, true);
            }
            Action::Return { src } => {
                let failed = self.deopt(pc, before.clone(), plan.live[position].clone(), reason::PENDING);
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
                let sequence = if ty.is_unboxed() {
                    let unboxed = self.use_register(src, ty);
                    let boxed = self.box_value(ty, unboxed);
                    self.store_register(src, boxed);
                    boxed
                } else {
                    let value = self.load_register(src);
                    if let Some(expected) = refine {
                        let fits = self.check(value, expected);
                        let failed = self.deopt(pc, before.clone(), plan.live[position].clone(), reason::SPECULATION);
                        self.guard(fits, failed);
                    }
                    value
                };
                let count = self.builder.ins().load(I64, flags(), sequence, LENGTH);
                let zero = self.builder.ins().iconst(I64, 0);
                self.builder.def_var(self.loop_counts[index], count);
                self.builder.def_var(self.loop_indexes[index], zero);
            }
            Action::ForStep { index, src, slot } => {
                let current = self.builder.use_var(self.loop_indexes[index]);
                let count = self.builder.use_var(self.loop_counts[index]);
                let done = self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, current, count);
                let target = jump_target(inst).ok_or(Refusal("a loop without an exit"))?;
                let exit = self.edge(position, target)?;
                let body = self.builder.create_block();
                self.builder.ins().brif(done, exit, &[], body, &[]);
                self.fill_edges()?;
                self.builder.switch_to_block(body);
                let sequence = self.load_register(src);
                let element_ty = before.registers[src].element();
                let width = if matches!(element_ty, Ty::Int | Ty::Lgl) { 4 } else { 8 };
                let scaled = self.builder.ins().imul_imm_s(current, width);
                let address = self.builder.ins().iadd(sequence, scaled);
                let next_index = self.builder.ins().iadd_imm_s(current, 1);
                self.builder.def_var(self.loop_indexes[index], next_index);
                match before.registers[src] {
                    Ty::IntVec | Ty::LglVec | Ty::DblVec => {
                        let load_ty = if width == 4 { I32 } else { F64 };
                        let element = self.builder.ins().load(load_ty, flags(), address, DATA);
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
            Action::Visible(flag) => self.set_visible(&before, flag),
            Action::LogicLeft { dst, src, is_and } => {
                let call = runtime_constant(self.code, inst.operands[2]);
                let value = self.logical(position, &before, src, call, is_and, false)?;
                let decided = self.builder.ins().icmp_imm_s(IntCC::Equal, value, if is_and { 0 } else { 1 });
                let decided_block = self.builder.create_block();
                let undecided_block = self.builder.create_block();
                self.builder.ins().brif(decided, decided_block, &[], undecided_block, &[]);
                self.builder.switch_to_block(decided_block);
                self.builder.def_var(self.integers[dst], value);
                self.set_visible(&before, true);
                let target = jump_target(inst).ok_or(Refusal("a logic operator without a target"))?;
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
            Action::LogicRight { dst, left, right, is_and } => {
                if before.registers[left] != Ty::Lgl {
                    return Err(Refusal("a logic operator whose left operand is not a logical scalar"));
                }
                let call = runtime_constant(self.code, inst.operands[3]);
                let left_value = self.builder.use_var(self.integers[left]);
                let right_value = self.logical(position, &before, right, call, is_and, true)?;
                let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
                let left_na = self.builder.ins().icmp(IntCC::Equal, left_value, na);
                let right_na = self.builder.ins().icmp(IntCC::Equal, right_value, na);
                let any_na = self.builder.ins().bor(left_na, right_na);
                let (decisive, decided_value, otherwise) = if is_and { (0, 0, 1) } else { (1, 1, 0) };
                let decides = self.builder.ins().icmp_imm_s(IntCC::Equal, right_value, decisive);
                let decided_value = self.builder.ins().iconst(I32, decided_value);
                let otherwise = self.builder.ins().iconst(I32, otherwise);
                let undecided = self.builder.ins().select(any_na, na, otherwise);
                let result = self.builder.ins().select(decides, decided_value, undecided);
                self.builder.def_var(self.integers[dst], result);
                self.set_visible(&before, true);
            }
            Action::SubAssign { dst, slot, index, value } => {
                self.sub_assign(position, &before, dst, slot, index, value)?;
                self.set_visible(&before, false);
            }
        }
        Ok(false)
    }

    // Steps: instructions tier 0 runs.

    unsafe fn step(&mut self, position: usize, before: &State, effect: Effect, objects: &[usize], dst: Option<(usize, Ty)>) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        self.box_reads(position, before);
        let failed = self.deopt(pc, before.clone(), self.plan.live[position].clone(), reason::PENDING);
        let validate = match effect {
            Effect::Pure => self.builder.ins().iconst(I32, 0),
            Effect::Opaque => {
                self.flush_all(before, failed);
                self.builder.ins().iconst(I32, 1)
            }
            Effect::PureUnlessObserved => {
                let mut any = self
                    .call(runtime::jit_handlers_present as *const () as usize, &[], Some(I32), &[])
                    .ok_or(Refusal("a helper without a result"))?;
                for &register in objects {
                    let value = self.load_register(register);
                    let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
                    let object = self.builder.ins().band_imm_u(bits, OBJECT_FLAG);
                    any = self.builder.ins().bor(any, object);
                }
                let dispatching = self.builder.create_block();
                let join = self.builder.create_block();
                self.builder.append_block_param(join, I32);
                let zero = self.builder.ins().iconst(I32, 0);
                self.builder.ins().brif(any, dispatching, &[], join, &[zero.into()]);
                self.builder.switch_to_block(dispatching);
                self.flush_all(before, failed);
                let one = self.builder.ins().iconst(I32, 1);
                self.builder.ins().jump(join, &[one.into()]);
                self.builder.switch_to_block(join);
                self.builder.block_params(join)[0]
            }
        };
        let result = self.call_step(pc, validate)?;
        let after = self.after_with_boxed(position, dst.map(|(register, _)| register))?;
        let next = self.next_pc(position)?;
        let next_live = self.plan.live.get(position + 1).cloned().unwrap_or_default();
        let invalidated = self.deopt(next, after.clone(), next_live.clone(), reason::SPECULATION);
        let ok = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::OK as i64);
        let continued = self.builder.create_block();
        let problem = self.builder.create_block();
        self.builder.ins().brif(ok, continued, &[], problem, &[]);
        self.builder.switch_to_block(problem);
        self.builder.set_cold_block(problem);
        let errored = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::FAILED as i64);
        self.builder.ins().brif(errored, failed, &[], invalidated, &[]);
        self.builder.switch_to_block(continued);
        match effect {
            Effect::Pure => {}
            Effect::Opaque => self.forget_slots(),
            Effect::PureUnlessObserved => {
                let zero = self.builder.ins().iconst(I32, 0);
                for slot in 0..self.plan.slots.len() {
                    let status = self.builder.use_var(self.statuses[slot]);
                    let status = self.builder.ins().select(validate, zero, status);
                    self.builder.def_var(self.statuses[slot], status);
                }
            }
        }
        if let Some((register, ty)) = dst {
            self.refine(register, ty, next, after, next_live);
        }
        Ok(())
    }

    /// Calls a closure with arguments evaluated in compiled code. The callee may reach this
    /// frame, so the variables are written back before the call and forgotten after it.
    #[allow(clippy::too_many_arguments)]
    unsafe fn direct_call(&mut self, position: usize, before: State, steps: Vec<(Action, State)>, function: Sexp, call: Sexp, args: &[Argument], dst: (usize, Ty)) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        for (step, state) in steps {
            self.action(position, state, step)?;
        }
        let failed = self.deopt(pc, before.clone(), self.plan.live[position].clone(), reason::PENDING);
        self.flush_all(&before, failed);
        let buffer = self.builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, (16 * args.len().max(1)) as u32, 3));
        for (index, argument) in args.iter().enumerate() {
            let (kind, bits) = match *argument {
                Argument::Slot(slot) => (representation::BOXED, self.load_slot_cell(slot)),
                Argument::Unboxed { register, ty } => {
                    let value = self.use_register(register, ty);
                    let (kind, bits) = match ty {
                        Ty::Dbl => (representation::DOUBLE, self.builder.ins().bitcast(I64, MemFlagsData::new(), value)),
                        Ty::Int => (representation::INTEGER, self.builder.ins().sextend(I64, value)),
                        _ => (representation::LOGICAL, self.builder.ins().sextend(I64, value)),
                    };
                    (kind, bits)
                }
            };
            let kind = self.builder.ins().iconst(I64, kind as i64);
            self.builder.ins().stack_store(I64, kind, buffer, (16 * index) as i32);
            self.builder.ins().stack_store(I64, bits, buffer, (16 * index + 8) as i32);
        }
        let pointer = self.builder.ins().stack_addr(I64, buffer, 0);
        let function = self.builder.ins().iconst(I64, function.as_ptr() as i64);
        let call = self.builder.ins().iconst(I64, call.as_ptr() as i64);
        let count = iconst(&mut self.builder, args.len());
        let register = iconst(&mut self.builder, dst.0);
        let result = self
            .call(runtime::jit_call as *const () as usize, &[I64, I64, I64, I64, I64, I64], Some(I32), &[self.activation, function, call, pointer, count, register])
            .ok_or(Refusal("a helper without a result"))?;
        let after = self.after_with_boxed(position, Some(dst.0))?;
        let next = self.next_pc(position)?;
        let next_live = self.plan.live.get(position + 1).cloned().unwrap_or_default();
        let invalidated = self.deopt(next, after.clone(), next_live.clone(), reason::SPECULATION);
        let ok = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::OK as i64);
        let continued = self.builder.create_block();
        let problem = self.builder.create_block();
        self.builder.ins().brif(ok, continued, &[], problem, &[]);
        self.builder.switch_to_block(problem);
        self.builder.set_cold_block(problem);
        let errored = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::FAILED as i64);
        self.builder.ins().brif(errored, failed, &[], invalidated, &[]);
        self.builder.switch_to_block(continued);
        self.forget_slots();
        self.refine(dst.0, dst.1, next, after, next_live);
        Ok(())
    }

    /// Checks that the value an instruction left in `register` has type `ty` and unboxes it,
    /// deoptimizing to `next` otherwise.
    unsafe fn refine(&mut self, register: usize, ty: Ty, next: usize, after: State, live: Vec<bool>) {
        // A closure an instruction just made has its type by construction.
        if matches!(ty, Ty::Boxed | Ty::Bottom | Ty::Closure(..)) {
            return;
        }
        let value = self.load_register(register);
        let fits = self.check(value, ty);
        let failed = self.deopt(next, after, live, reason::SPECULATION);
        self.guard(fits, failed);
        if ty.is_unboxed() {
            let unboxed = self.unbox(value, ty);
            self.def_register(register, ty, unboxed);
        }
    }

    /// Boxes the unboxed registers the instruction at `position` reads, so tier 0 finds them.
    unsafe fn box_reads(&mut self, position: usize, before: &State) {
        for register in reads(&self.plan.insts[position]) {
            let ty = before.registers.get(register).copied().unwrap_or(Ty::Bottom);
            if ty.is_unboxed() {
                let value = self.use_register(register, ty);
                let boxed = self.box_value(ty, value);
                self.store_register(register, boxed);
            }
        }
    }

    /// Leaves for tier 0 at the instruction, the slow path of an inline fast path. Tier 0 then
    /// runs it with the frame written back, so a warning handler sees every variable, and
    /// re-enters compiled code at the next loop back edge.
    fn hand_off(&mut self, position: usize, before: &State) {
        let pc = self.plan.insts[position].pc;
        let handoff = self.deopt(pc, before.clone(), self.plan.live[position].clone(), reason::HANDOFF);
        self.builder.ins().jump(handoff, &[]);
    }

    // Arithmetic.

    #[allow(clippy::too_many_arguments)]
    unsafe fn arithmetic(&mut self, position: usize, before: &State, arith: Arith, numeric: Numeric, dst: usize, left: usize, right: usize) -> Result<(), Refusal> {
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
                        .call(runtime::jit_power as *const () as usize, &[F64, F64], Some(F64), &[a, b])
                        .ok_or(Refusal("a helper without a result"))?,
                    _ => return Err(Refusal("double arithmetic the JIT does not inline")),
                };
                self.builder.def_var(self.doubles[dst], value);
            }
        }
        Ok(())
    }

    unsafe fn comparison(&mut self, before: &State, arith: Arith, numeric: Numeric, left: usize, right: usize) -> Value {
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

    unsafe fn intrinsic(&mut self, position: usize, before: &State, intrinsic: Intrinsic, dst: usize, args: &[usize], result: Ty) -> Result<(), Refusal> {
        let types: Vec<Ty> = args.iter().map(|&register| before.registers[register]).collect();
        let na = self.builder.ins().iconst(I32, NA_INTEGER as i64);
        match intrinsic {
            Intrinsic::Element => {
                let vector = self.load_register(args[0]);
                let length = self.builder.ins().load(I64, flags(), vector, LENGTH);
                let (index, in_range) = match types[1] {
                    Ty::Int => {
                        let index = self.use_register(args[1], Ty::Int);
                        let index = self.builder.ins().sextend(I64, index);
                        let positive = self.builder.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, index, 1);
                        let within = self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, index, length);
                        (index, self.builder.ins().band(positive, within))
                    }
                    _ => {
                        let index = self.use_register(args[1], Ty::Dbl);
                        let one = self.builder.ins().f64const(1.0);
                        let positive = self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, index, one);
                        let bound = self.builder.ins().fcvt_from_sint(F64, length);
                        let bound = self.builder.ins().fadd(bound, one);
                        let within = self.builder.ins().fcmp(FloatCC::LessThan, index, bound);
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
                let width = if matches!(result, Ty::Int | Ty::Lgl) { 4 } else { 8 };
                let offset = self.builder.ins().iadd_imm_s(index, -1);
                let scaled = self.builder.ins().imul_imm_s(offset, width);
                let address = self.builder.ins().iadd(vector, scaled);
                match result {
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
            Intrinsic::Length => {
                let value = if types[0].is_unboxed() {
                    self.builder.ins().iconst(I32, 1)
                } else {
                    let vector = self.load_register(args[0]);
                    let length = self.builder.ins().load(I64, flags(), vector, LENGTH);
                    // Longer vectors have a double length; leave those to tier 0.
                    let fits = self.builder.ins().icmp_imm_s(IntCC::SignedLessThanOrEqual, length, i32::MAX as i64);
                    let failed = self.deopt(self.plan.insts[position].pc, before.clone(), self.plan.live[position].clone(), reason::SPECULATION);
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
                    let zero = self.builder.ins().fcmp(FloatCC::Equal, value, zero_constant);
                    (zero, self.builder.ins().fcmp(FloatCC::Unordered, value, value))
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
                    self.call(runtime::jit_log as *const () as usize, &[F64], Some(F64), &[value]).ok_or(Refusal("a helper without a result"))?
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
                    Intrinsic::Exp => self.call(runtime::jit_exp as *const () as usize, &[F64], Some(F64), &[value]).ok_or(Refusal("a helper without a result"))?,
                    Intrinsic::Floor => self.builder.ins().floor(value),
                    _ => self.builder.ins().ceil(value),
                };
                self.builder.def_var(self.doubles[dst], result);
            }
            Intrinsic::Modulo(Numeric::Int) | Intrinsic::IntegerDivide(Numeric::Int) => {
                let a = self.use_register(args[0], types[0]);
                let b = self.use_register(args[1], types[1]);
                let helper = if matches!(intrinsic, Intrinsic::Modulo(_)) { runtime::jit_integer_modulo as *const () as usize } else { runtime::jit_integer_divide as *const () as usize };
                let result = self.call(helper, &[I32, I32], Some(I32), &[a, b]).ok_or(Refusal("a helper without a result"))?;
                self.builder.def_var(self.integers[dst], result);
            }
            Intrinsic::Modulo(Numeric::Dbl) => {
                let a = self.as_double(args[0], types[0]);
                let b = self.as_double(args[1], types[1]);
                let result = self.call(runtime::jit_real_modulo as *const () as usize, &[F64, F64], Some(F64), &[a, b]).ok_or(Refusal("a helper without a result"))?;
                self.builder.def_var(self.doubles[dst], result);
            }
            Intrinsic::IntegerDivide(Numeric::Dbl) => {
                let a = self.as_double(args[0], types[0]);
                let b = self.as_double(args[1], types[1]);
                let quotient = self.builder.ins().fdiv(a, b);
                let result = self.builder.ins().floor(quotient);
                self.builder.def_var(self.doubles[dst], result);
            }
        }
        Ok(())
    }

    // Conditions.

    /// The truth of `if`'s condition as a Cranelift boolean, deoptimizing with the error for
    /// `NA` and anything else tier 0 rejects.
    unsafe fn condition(&mut self, position: usize, before: &State, src: usize, call: Sexp) -> Result<Value, Refusal> {
        let ty = before.registers[src];
        let pc = self.plan.insts[position].pc;
        let failed = self.deopt(pc, before.clone(), self.plan.live[position].clone(), reason::PENDING);
        let (truth, valid) = match ty {
            Ty::Int | Ty::Lgl => {
                let value = self.use_register(src, ty);
                let truth = self.builder.ins().icmp_imm_s(IntCC::NotEqual, value, 0);
                (truth, self.builder.ins().icmp_imm_s(IntCC::NotEqual, value, NA_INTEGER as i64))
            }
            Ty::Dbl => {
                let value = self.use_register(src, ty);
                let zero = self.builder.ins().f64const(0.0);
                let truth = self.builder.ins().fcmp(FloatCC::OrderedNotEqual, value, zero);
                (truth, self.builder.ins().fcmp(FloatCC::Ordered, value, value))
            }
            _ => {
                let value = self.load_register(src);
                let call = self.builder.ins().iconst(I64, call.as_ptr() as i64);
                let result = self
                    .call(runtime::jit_condition as *const () as usize, &[I64, I64, I64], Some(I32), &[self.activation, value, call])
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
        self.call(runtime::jit_condition as *const () as usize, &[I64, I64, I64], Some(I32), &[self.activation, boxed, call]);
        self.builder.ins().jump(failed, &[]);
        self.builder.switch_to_block(fine);
        Ok(truth)
    }

    /// An operand of `&&` or `||` as a logical `i32`.
    unsafe fn logical(&mut self, position: usize, before: &State, src: usize, call: Sexp, is_and: bool, right: bool) -> Result<Value, Refusal> {
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
                let truth = self.builder.ins().fcmp(FloatCC::OrderedNotEqual, value, zero);
                let truth = self.builder.ins().uextend(I32, truth);
                self.builder.ins().select(missing, na, truth)
            }
            _ => {
                let pc = self.plan.insts[position].pc;
                let failed = self.deopt(pc, before.clone(), self.plan.live[position].clone(), reason::PENDING);
                let value = self.load_register(src);
                let call = self.builder.ins().iconst(I64, call.as_ptr() as i64);
                let is_and = self.builder.ins().iconst(I32, is_and as i64);
                let right = self.builder.ins().iconst(I32, right as i64);
                let result = self
                    .call(runtime::jit_logic as *const () as usize, &[I64, I64, I64, I32, I32], Some(I32), &[self.activation, value, call, is_and, right])
                    .ok_or(Refusal("a helper without a result"))?;
                let valid = self.builder.ins().icmp_imm_s(IntCC::NotEqual, result, 2);
                self.guard(valid, failed);
                result
            }
        })
    }

    // Element assignment.

    unsafe fn sub_assign(&mut self, position: usize, before: &State, dst: usize, slot: usize, index: usize, value: usize) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let target = before.slots[slot].ty;
        let (index_ty, value_ty) = (before.registers[index], before.registers[value]);
        let live = self.plan.live[position].clone();
        let failed = self.deopt(pc, before.clone(), live.clone(), reason::PENDING);
        let mismatch = self.deopt(pc, before.clone(), live, reason::SPECULATION);
        // An unloaded variable is loaded first: a loop that calls a function every iteration
        // forgets its variables every time.
        if before.slots[slot].may_be(status::NOT_LOADED) {
            let status = self.builder.use_var(self.statuses[slot]);
            let unloaded = self.builder.ins().icmp_imm_s(IntCC::Equal, status, 0);
            let load = self.builder.create_block();
            let loaded = self.builder.create_block();
            self.builder.ins().brif(unloaded, load, &[], loaded, &[]);
            self.builder.switch_to_block(load);
            let slot_constant = iconst(&mut self.builder, slot);
            let result = self
                .call(runtime::jit_load_slot as *const () as usize, &[I64, I64], Some(I32), &[self.activation, slot_constant])
                .ok_or(Refusal("a helper without a result"))?;
            let error = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::LOAD_FAILED as i64);
            let not_error = self.builder.create_block();
            self.builder.ins().brif(error, failed, &[], not_error, &[]);
            self.builder.switch_to_block(not_error);
            let invalid = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::LOAD_INVALIDATED as i64);
            let valid = self.builder.ins().bxor_imm_u(invalid, 1);
            self.guard(valid, mismatch);
            let vector = self.load_slot_cell(slot);
            let fits = self.check(vector, target);
            self.guard(fits, mismatch);
            self.builder.def_var(self.statuses[slot], result);
            self.builder.ins().jump(loaded, &[]);
            self.builder.switch_to_block(loaded);
        }
        let vector = self.load_slot_cell(slot);
        let length = self.builder.ins().load(I64, flags(), vector, LENGTH);
        let (position_value, in_range) = match index_ty {
            Ty::Int => {
                let index_value = self.use_register(index, Ty::Int);
                let index_value = self.builder.ins().sextend(I64, index_value);
                let positive = self.builder.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, index_value, 1);
                let within = self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, index_value, length);
                (index_value, self.builder.ins().band(positive, within))
            }
            _ => {
                let index_value = self.use_register(index, Ty::Dbl);
                let one = self.builder.ins().f64const(1.0);
                let positive = self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, index_value, one);
                let bound = self.builder.ins().fcvt_from_sint(F64, length);
                let bound = self.builder.ins().fadd(bound, one);
                let within = self.builder.ins().fcmp(FloatCC::LessThan, index_value, bound);
                let in_range = self.builder.ins().band(positive, within);
                let safe = self.builder.ins().select(in_range, index_value, one);
                (self.builder.ins().fcvt_to_sint(I64, safe), in_range)
            }
        };
        let fast = self.builder.create_block();
        let slow = self.builder.create_block();
        let join = self.builder.create_block();
        self.builder.ins().brif(in_range, fast, &[], slow, &[]);

        self.builder.switch_to_block(fast);
        let status = self.builder.use_var(self.statuses[slot]);
        let enclosing = self.builder.ins().icmp_imm_s(IntCC::Equal, status, status::RUNTIME_ENCLOSING);
        let count = self.builder.ins().uload16(I32, flags(), vector, REFCOUNT);
        let shared_count = self.builder.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, count, 1);
        let shared = self.builder.ins().bor(enclosing, shared_count);
        let copy = self.builder.create_block();
        let store = self.builder.create_block();
        self.builder.append_block_param(store, I64);
        self.builder.ins().brif(shared, copy, &[], store, &[vector.into()]);
        self.builder.switch_to_block(copy);
        let dirty = self.builder.ins().icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
        let dirty = self.builder.ins().uextend(I32, dirty);
        let slot_constant = iconst(&mut self.builder, slot);
        let fresh = self
            .call(runtime::jit_duplicate as *const () as usize, &[I64, I64, I32], Some(I64), &[self.activation, slot_constant, dirty])
            .ok_or(Refusal("a helper without a result"))?;
        let copied = self.builder.ins().icmp_imm_s(IntCC::NotEqual, fresh, 0);
        self.guard(copied, failed);
        let dirty_status = self.builder.ins().iconst(I32, status::RUNTIME_DIRTY);
        self.builder.def_var(self.statuses[slot], dirty_status);
        self.builder.ins().jump(store, &[fresh.into()]);
        self.builder.switch_to_block(store);
        let vector = self.builder.block_params(store)[0];
        let element_offset = self.builder.ins().iadd_imm_s(position_value, -1);
        match target {
            Ty::DblVec => {
                let element = self.as_double(value, value_ty);
                let scaled = self.builder.ins().imul_imm_s(element_offset, 8);
                let address = self.builder.ins().iadd(vector, scaled);
                self.builder.ins().store(flags(), element, address, DATA);
            }
            Ty::IntVec | Ty::LglVec => {
                let element = self.use_register(value, value_ty);
                let scaled = self.builder.ins().imul_imm_s(element_offset, 4);
                let address = self.builder.ins().iadd(vector, scaled);
                self.builder.ins().store(flags(), element, address, DATA);
            }
            _ => return Err(Refusal("an element assignment into a vector of unknown type")),
        }
        self.builder.ins().jump(join, &[]);

        // Out of range: write the frame back, since tier 0 may read any variable while it grows
        // or rejects the vector, and forget the variable it rebinds.
        self.builder.switch_to_block(slow);
        self.builder.set_cold_block(slow);
        self.flush_all(before, failed);
        self.box_reads(position, before);
        let zero = self.builder.ins().iconst(I32, 0);
        let result = self.call_step(pc, zero)?;
        let ok = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::OK as i64);
        self.guard(ok, failed);
        let unloaded = self.builder.ins().iconst(I32, 0);
        self.builder.def_var(self.statuses[slot], unloaded);
        self.builder.ins().jump(join, &[]);

        self.builder.switch_to_block(join);
        if value_ty.is_unboxed() {
            let copy = self.use_register(value, value_ty);
            self.def_register(dst, value_ty, copy);
        }
        Ok(())
    }

    // Variables.

    /// Loads a slot whose runtime status may be "not loaded", checking the value has type `ty`.
    /// A `forcing` load may evaluate a default argument in this frame, so the variables are
    /// written back before it and forgotten after it.
    unsafe fn load_slot(&mut self, position: usize, before: &State, slot: usize, ty: Ty, forcing: bool) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let live = self.plan.live[position].clone();
        let failed = self.deopt(pc, before.clone(), live.clone(), reason::PENDING);
        let mismatch = self.deopt(pc, before.clone(), live, reason::SPECULATION);
        let done = self.builder.create_block();
        let load = self.builder.create_block();
        if before.slots[slot].may_be(status::LOADED) {
            let status = self.builder.use_var(self.statuses[slot]);
            let unloaded = self.builder.ins().icmp_imm_s(IntCC::Equal, status, 0);
            self.builder.ins().brif(unloaded, load, &[], done, &[]);
        } else {
            self.builder.ins().jump(load, &[]);
        }
        self.builder.switch_to_block(load);
        if forcing {
            self.flush_all(before, failed);
        }
        let pc_constant = iconst(&mut self.builder, pc);
        let slot_constant = iconst(&mut self.builder, slot);
        let result = self
            .call(runtime::jit_load as *const () as usize, &[I64, I64, I64], Some(I32), &[self.activation, pc_constant, slot_constant])
            .ok_or(Refusal("a helper without a result"))?;
        let error = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::LOAD_FAILED as i64);
        let not_error = self.builder.create_block();
        self.builder.ins().brif(error, failed, &[], not_error, &[]);
        self.builder.switch_to_block(not_error);
        let invalid = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::LOAD_INVALIDATED as i64);
        let valid = self.builder.ins().bxor_imm_u(invalid, 1);
        self.guard(valid, mismatch);
        if forcing {
            self.forget_slots();
        }
        let value = self.load_slot_cell(slot);
        if !matches!(ty, Ty::Boxed | Ty::Bottom) {
            let fits = self.check(value, ty);
            self.guard(fits, mismatch);
        }
        self.builder.def_var(self.statuses[slot], result);
        match ty {
            Ty::Int | Ty::Lgl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_integers[slot], unboxed);
            }
            Ty::Dbl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_doubles[slot], unboxed);
            }
            _ => {}
        }
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
        Ok(())
    }

    /// Loads a slot for an inlined call without forcing anything, handing the call back to
    /// tier 0 when the variable is not a plain value already.
    unsafe fn peek(&mut self, position: usize, before: &State, slot: usize, ty: Ty) -> Result<(), Refusal> {
        let pc = self.plan.insts[position].pc;
        let mismatch = self.deopt(pc, before.clone(), self.plan.live[position].clone(), reason::SPECULATION);
        let done = self.builder.create_block();
        let load = self.builder.create_block();
        if before.slots[slot].may_be(status::LOADED) {
            let status = self.builder.use_var(self.statuses[slot]);
            let unloaded = self.builder.ins().icmp_imm_s(IntCC::Equal, status, 0);
            self.builder.ins().brif(unloaded, load, &[], done, &[]);
        } else {
            self.builder.ins().jump(load, &[]);
        }
        self.builder.switch_to_block(load);
        let slot_constant = iconst(&mut self.builder, slot);
        let result = self
            .call(runtime::jit_peek_slot as *const () as usize, &[I64, I64], Some(I32), &[self.activation, slot_constant])
            .ok_or(Refusal("a helper without a result"))?;
        let declined = self.builder.ins().icmp_imm_s(IntCC::Equal, result, outcome::LOAD_FAILED as i64);
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
        self.builder.def_var(self.statuses[slot], result);
        match ty {
            Ty::Int | Ty::Lgl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_integers[slot], unboxed);
            }
            Ty::Dbl => {
                let unboxed = self.unbox(value, ty);
                self.builder.def_var(self.slot_doubles[slot], unboxed);
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
    unsafe fn assign_slot(&mut self, before: &State, slot: usize, ty: Ty, value: Value) {
        let old = before.slots[slot];
        let release = (!old.ty.is_unboxed() && old.ty != Ty::Bottom && old.may_be(status::DIRTY)).then(|| {
            let status = self.builder.use_var(self.statuses[slot]);
            let dirty = self.builder.ins().icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
            (self.load_slot_cell(slot), dirty)
        });
        match ty {
            Ty::Int | Ty::Lgl => self.builder.def_var(self.slot_integers[slot], value),
            Ty::Dbl => self.builder.def_var(self.slot_doubles[slot], value),
            _ => {
                self.store_slot_cell(slot, value);
                self.increment(value);
            }
        }
        if let Some((previous, dirty)) = release {
            self.decrement_if(previous, dirty);
        }
        let dirty = self.builder.ins().iconst(I32, status::RUNTIME_DIRTY);
        self.builder.def_var(self.statuses[slot], dirty);
    }

    /// Writes every dirty slot back to the environment.
    unsafe fn flush_all(&mut self, state: &State, failed: Label) {
        for slot in 0..state.slots.len() {
            self.flush_slot(state, slot, failed);
        }
    }

    unsafe fn flush_slot(&mut self, state: &State, slot: usize, failed: Label) {
        let current = state.slots[slot];
        if !current.may_be(status::DIRTY) {
            return;
        }
        let status = self.builder.use_var(self.statuses[slot]);
        let dirty = self.builder.ins().icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
        let flush = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.ins().brif(dirty, flush, &[], done, &[]);
        self.builder.switch_to_block(flush);
        let result = self.call_flush(slot, current.ty);
        let ok = self.builder.ins().icmp_imm_s(IntCC::NotEqual, result, 0);
        self.guard(ok, failed);
        let clean = self.builder.ins().iconst(I32, status::RUNTIME_LOCAL);
        self.builder.def_var(self.statuses[slot], clean);
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
    }

    unsafe fn call_flush(&mut self, slot: usize, ty: Ty) -> Value {
        let (kind, bits) = match ty {
            Ty::Int | Ty::Lgl => {
                let value = self.builder.use_var(self.slot_integers[slot]);
                let kind = if ty == Ty::Int { representation::INTEGER } else { representation::LOGICAL };
                (kind, self.builder.ins().sextend(I64, value))
            }
            Ty::Dbl => {
                let value = self.builder.use_var(self.slot_doubles[slot]);
                (representation::DOUBLE, self.builder.ins().bitcast(I64, MemFlagsData::new(), value))
            }
            _ => (representation::BOXED, self.builder.ins().iconst(I64, 0)),
        };
        let slot_constant = iconst(&mut self.builder, slot);
        let kind = self.builder.ins().iconst(I32, kind as i64);
        self.call(runtime::jit_flush as *const () as usize, &[I64, I64, I32, I64], Some(I32), &[self.activation, slot_constant, kind, bits])
            .unwrap_or(kind)
    }

    unsafe fn forget_slots(&mut self) {
        let zero = self.builder.ins().iconst(I32, 0);
        for slot in 0..self.plan.slots.len() {
            self.builder.def_var(self.statuses[slot], zero);
        }
    }

    // Edges and deoptimization.

    /// The block to branch to for the edge from `position` to `target`: the target block
    /// itself, or a new block that first converts the values whose representation differs.
    fn edge(&mut self, position: usize, target: usize) -> Result<Label, Refusal> {
        let state = self.plan.edges[position]
            .iter()
            .find(|(pc, _)| *pc == target)
            .map(|(_, state)| state.clone())
            .ok_or(Refusal("an edge the analysis did not see"))?;
        let block = *self.plan.block_at.get(&target).ok_or(Refusal("a jump into an instruction"))?;
        let label = self.labels[block].ok_or(Refusal("a jump to unreachable code"))?;
        let entry = self.plan.blocks[block].entry.as_ref().ok_or(Refusal("a jump to unreachable code"))?;
        let registers = state.registers.iter().zip(&entry.registers).any(|(from, to)| from.is_unboxed() && *to != Ty::Bottom && !to.is_unboxed());
        let slots = state.slots.iter().zip(&entry.slots).any(|(from, to)| {
            from.ty.is_unboxed() && from.may_be(status::LOADED) && to.ty != Ty::Bottom && !to.ty.is_unboxed()
        });
        if !registers && !slots {
            return Ok(label);
        }
        let converting = self.builder.create_block();
        self.edges.push(PendingEdge { label: converting, state, target: block });
        Ok(converting)
    }

    unsafe fn fill_edges(&mut self) -> Result<(), Refusal> {
        let edges = std::mem::take(&mut self.edges);
        for edge in edges {
            self.builder.switch_to_block(edge.label);
            let entry = self.plan.blocks[edge.target].entry.clone().ok_or(Refusal("a jump to unreachable code"))?;
            for (register, (from, to)) in edge.state.registers.iter().zip(&entry.registers).enumerate() {
                if from.is_unboxed() && *to != Ty::Bottom && !to.is_unboxed() {
                    let value = self.use_register(register, *from);
                    let boxed = self.box_value(*from, value);
                    self.store_register(register, boxed);
                }
            }
            for (slot, (from, to)) in edge.state.slots.iter().zip(&entry.slots).enumerate() {
                if from.ty.is_unboxed() && from.may_be(status::LOADED) && to.ty != Ty::Bottom && !to.ty.is_unboxed() {
                    self.box_slot(slot, from.ty);
                }
            }
            let label = self.labels[edge.target].ok_or(Refusal("a jump to unreachable code"))?;
            self.builder.ins().jump(label, &[]);
        }
        Ok(())
    }

    /// Moves a loaded slot's unboxed value into its root cell. A dirty slot owns a reference.
    unsafe fn box_slot(&mut self, slot: usize, ty: Ty) {
        let status = self.builder.use_var(self.statuses[slot]);
        let loaded = self.builder.ins().icmp_imm_s(IntCC::NotEqual, status, 0);
        let boxing = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.ins().brif(loaded, boxing, &[], done, &[]);
        self.builder.switch_to_block(boxing);
        let value = self.use_slot(slot, ty);
        let boxed = self.box_value(ty, value);
        self.store_slot_cell(slot, boxed);
        let dirty = self.builder.ins().icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
        self.increment_if(boxed, dirty);
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
    }

    fn deopt(&mut self, pc: usize, state: State, live: Vec<bool>, why: i32) -> Label {
        let label = self.builder.create_block();
        self.deopts.push(Deopt { label, pc, state, live, why });
        label
    }

    unsafe fn emit_deopt(&mut self, deopt: Deopt) {
        self.builder.switch_to_block(deopt.label);
        self.builder.set_cold_block(deopt.label);
        for (register, ty) in deopt.state.registers.iter().enumerate() {
            if ty.is_unboxed() && deopt.live.get(register).copied().unwrap_or(false) {
                let value = self.use_register(register, *ty);
                let boxed = self.box_value(*ty, value);
                self.store_register(register, boxed);
            }
        }
        for slot in 0..deopt.state.slots.len() {
            let current = deopt.state.slots[slot];
            if !current.may_be(status::DIRTY) {
                continue;
            }
            let status = self.builder.use_var(self.statuses[slot]);
            let dirty = self.builder.ins().icmp_imm_s(IntCC::Equal, status, status::RUNTIME_DIRTY);
            let flush = self.builder.create_block();
            let done = self.builder.create_block();
            self.builder.ins().brif(dirty, flush, &[], done, &[]);
            self.builder.switch_to_block(flush);
            self.builder.set_cold_block(flush);
            // A failure parks its error, which the deoptimization below then raises.
            self.call_flush(slot, current.ty);
            self.builder.ins().jump(done, &[]);
            self.builder.switch_to_block(done);
            self.builder.set_cold_block(done);
        }
        for index in 0..self.plan.loops {
            let value = self.builder.use_var(self.loop_indexes[index]);
            let count = self.builder.use_var(self.loop_counts[index]);
            self.builder.ins().store(flags(), value, self.loops, (16 * index) as i32);
            self.builder.ins().store(flags(), count, self.loops, (16 * index + 8) as i32);
        }
        let pc = iconst(&mut self.builder, deopt.pc);
        let why = self.builder.ins().iconst(I32, deopt.why as i64);
        let result = self.call(runtime::jit_deopt as *const () as usize, &[I64, I64, I32], Some(I64), &[self.activation, pc, why]);
        let result = result.unwrap_or(pc);
        self.builder.ins().return_(&[result]);
    }

    fn guard(&mut self, condition: Value, failed: Label) {
        let next = self.builder.create_block();
        self.builder.ins().brif(condition, next, &[], failed, &[]);
        self.builder.switch_to_block(next);
    }

    fn next_pc(&self, position: usize) -> Result<usize, Refusal> {
        self.plan.insts.get(position + 1).map(|inst| inst.pc).ok_or(Refusal("the body falls off its end"))
    }

    fn after(&self, position: usize) -> Result<State, Refusal> {
        let next = self.next_pc(position)?;
        self.plan.edges[position]
            .iter()
            .find(|(pc, _)| *pc == next)
            .map(|(_, state)| state.clone())
            .ok_or(Refusal("an instruction without a fallthrough state"))
    }

    /// The state after an instruction that ran in tier 0, whose destination is still boxed.
    fn after_with_boxed(&self, position: usize, dst: Option<usize>) -> Result<State, Refusal> {
        let mut state = self.after(position)?;
        if let Some(register) = dst {
            state.registers[register] = Ty::Boxed;
        }
        Ok(state)
    }

    fn set_visible(&mut self, before: &State, flag: bool) {
        if before.visible != Some(flag) {
            let value = self.builder.ins().iconst(I8, flag as i64);
            self.builder.ins().store(flags(), value, self.visible, 0);
        }
    }

    // Values.

    fn load_register(&mut self, register: usize) -> Value {
        self.builder.ins().load(I64, flags(), self.window, (8 * register) as i32)
    }

    fn store_register(&mut self, register: usize, value: Value) {
        self.builder.ins().store(flags(), value, self.window, (8 * register) as i32);
    }

    fn load_slot_cell(&mut self, slot: usize) -> Value {
        self.builder.ins().load(I64, flags(), self.window, (8 * (self.plan.registers + slot)) as i32)
    }

    fn store_slot_cell(&mut self, slot: usize, value: Value) {
        self.builder.ins().store(flags(), value, self.window, (8 * (self.plan.registers + slot)) as i32);
    }

    fn use_register(&mut self, register: usize, ty: Ty) -> Value {
        match ty {
            Ty::Dbl => self.builder.use_var(self.doubles[register]),
            _ => self.builder.use_var(self.integers[register]),
        }
    }

    fn def_register(&mut self, register: usize, ty: Ty, value: Value) {
        match ty {
            Ty::Dbl => self.builder.def_var(self.doubles[register], value),
            _ => self.builder.def_var(self.integers[register], value),
        }
    }

    fn use_slot(&mut self, slot: usize, ty: Ty) -> Value {
        match ty {
            Ty::Dbl => self.builder.use_var(self.slot_doubles[slot]),
            _ => self.builder.use_var(self.slot_integers[slot]),
        }
    }

    /// A register's value as a double, converting integers and logicals with `NA` kept.
    fn as_double(&mut self, register: usize, ty: Ty) -> Value {
        let value = self.use_register(register, ty);
        if ty == Ty::Dbl {
            return value;
        }
        let missing = self.builder.ins().icmp_imm_s(IntCC::Equal, value, NA_INTEGER as i64);
        let converted = self.builder.ins().fcvt_from_sint(F64, value);
        let na = self.builder.ins().f64const(Ieee64::with_bits(NA_REAL_BITS));
        self.builder.ins().select(missing, na, converted)
    }

    /// Whether the object `value` points to has type `ty`.
    fn check(&mut self, value: Value, ty: Ty) -> Value {
        let (kind, scalar) = match ty {
            Ty::Fun(function) => return self.builder.ins().icmp_imm_s(IntCC::Equal, value, function.as_ptr() as i64),
            Ty::Closure(formals, body) => {
                let formals = self.builder.ins().iconst(I64, formals.as_ptr() as i64);
                let body = self.builder.ins().iconst(I64, body.as_ptr() as i64);
                let matches = self
                    .call(runtime::jit_closure_matches as *const () as usize, &[I64, I64, I64, I64], Some(I32), &[self.activation, value, formals, body])
                    .unwrap_or(formals);
                return self.builder.ins().icmp_imm_s(IntCC::NotEqual, matches, 0);
            }
            Ty::Int => (SexpType::Integer, true),
            Ty::Dbl => (SexpType::Real, true),
            Ty::Lgl => (SexpType::Logical, true),
            Ty::IntVec => (SexpType::Integer, false),
            Ty::DblVec => (SexpType::Real, false),
            Ty::LglVec => (SexpType::Logical, false),
            Ty::List => (SexpType::List, false),
            Ty::Boxed | Ty::Bottom => return self.builder.ins().iconst(I8, 1),
        };
        let actual = self.builder.ins().uload8(I32, flags(), value, KIND);
        let mut fits = self.builder.ins().icmp_imm_s(IntCC::Equal, actual, kind as i64);
        let attributes = self.builder.ins().load(I64, flags(), value, ATTRIB);
        let plain = self.builder.ins().icmp_imm_s(IntCC::Equal, attributes, self.nil);
        fits = self.builder.ins().band(fits, plain);
        let bits = self.builder.ins().uload8(I32, flags(), value, FLAGS);
        let altrep = self.builder.ins().band_imm_u(bits, ALTREP_FLAG);
        let ordinary = self.builder.ins().icmp_imm_s(IntCC::Equal, altrep, 0);
        fits = self.builder.ins().band(fits, ordinary);
        if scalar {
            let length = self.builder.ins().load(I64, flags(), value, LENGTH);
            let single = self.builder.ins().icmp_imm_s(IntCC::Equal, length, 1);
            fits = self.builder.ins().band(fits, single);
        }
        fits
    }

    fn unbox(&mut self, value: Value, ty: Ty) -> Value {
        let load_ty = if ty == Ty::Dbl { F64 } else { I32 };
        self.builder.ins().load(load_ty, flags(), value, DATA)
    }

    fn box_value(&mut self, ty: Ty, value: Value) -> Value {
        let (helper, param) = match ty {
            Ty::Int => (runtime::jit_box_integer as *const () as usize, I32),
            Ty::Dbl => (runtime::jit_box_double as *const () as usize, F64),
            _ => (runtime::jit_box_logical as *const () as usize, I32),
        };
        self.call(helper, &[I64, param], Some(I64), &[self.activation, value]).unwrap_or(value)
    }

    fn increment(&mut self, value: Value) {
        let yes = self.builder.ins().iconst(I8, 1);
        self.increment_if(value, yes);
    }

    /// Counts a reference to `value` when `condition` holds, saturating like the heap does.
    fn increment_if(&mut self, value: Value, condition: Value) {
        let count = self.builder.ins().uload16(I32, flags(), value, REFCOUNT);
        let saturated = self.builder.ins().icmp_imm_s(IntCC::Equal, count, REFCOUNT_MAX as i64);
        let not_saturated = self.builder.ins().bxor_imm_u(saturated, 1);
        let counting = self.builder.ins().band(condition, not_saturated);
        let raised = self.builder.ins().iadd_imm_s(count, 1);
        let new_count = self.builder.ins().select(counting, raised, count);
        self.builder.ins().istore16(flags(), new_count, value, REFCOUNT);
    }

    fn decrement_if(&mut self, value: Value, condition: Value) {
        let count = self.builder.ins().uload16(I32, flags(), value, REFCOUNT);
        let saturated = self.builder.ins().icmp_imm_s(IntCC::Equal, count, REFCOUNT_MAX as i64);
        let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, count, 0);
        let stuck = self.builder.ins().bor(saturated, zero);
        let free = self.builder.ins().bxor_imm_u(stuck, 1);
        let counting = self.builder.ins().band(condition, free);
        let lowered = self.builder.ins().iadd_imm_s(count, -1);
        let new_count = self.builder.ins().select(counting, lowered, count);
        self.builder.ins().istore16(flags(), new_count, value, REFCOUNT);
    }

    /// Runs the tier-0 instruction at `pc` through [`runtime::jit_step`].
    fn call_step(&mut self, pc: usize, validate: Value) -> Result<Value, Refusal> {
        let pc = iconst(&mut self.builder, pc);
        self.call(runtime::jit_step as *const () as usize, &[I64, I64, I32], Some(I32), &[self.activation, pc, validate])
            .ok_or(Refusal("a helper without a result"))
    }

    fn call(&mut self, address: usize, params: &[Type], result: Option<Type>, args: &[Value]) -> Option<Value> {
        let key = (params.to_vec(), result);
        let signature = match self.signatures.get(&key) {
            Some(signature) => *signature,
            None => {
                let mut signature = Signature::new(self.call_conv);
                signature.params.extend(params.iter().map(|ty| AbiParam::new(*ty)));
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

fn iconst(builder: &mut FunctionBuilder<'_>, value: usize) -> Value {
    builder.ins().iconst(I64, value as i64)
}

unsafe fn runtime_constant(code: Sexp, index: usize) -> Sexp {
    unsafe { code.cdr().element(index) }
}

/// The registers an instruction reads.
fn reads(inst: &crate::analysis::Inst) -> Vec<usize> {
    let operand = |index: usize| inst.operands[index];
    match inst.op {
        Op::CallBuiltin => std::iter::once(operand(1)).chain(operand(3)..operand(3) + operand(4)).collect(),
        Op::CallGeneric => vec![operand(1)],
        Op::BranchUnlessBuiltin => vec![operand(0)],
        Op::Arithmetic => vec![operand(2), operand(3)],
        Op::SubAssign => vec![operand(2), operand(3)],
        Op::JumpUnless | Op::Return => vec![operand(0)],
        Op::ForInit | Op::ForStep | Op::LogicLeft => vec![operand(1)],
        Op::LogicRight => vec![operand(1), operand(2)],
        Op::SetVar => vec![operand(1)],
        _ => Vec::new(),
    }
}
