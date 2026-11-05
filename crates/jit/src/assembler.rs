//! The baseline tier's machine code emitter, for x86-64 and aarch64.
//!
//! The baseline compiler needs a few dozen instruction forms: calls through a table of step
//! functions, compares of the step's answer against constants, register-window loads and
//! stores, byte tests on object headers and a jump table. This module encodes exactly those,
//! for both architectures, as operations on fixed machine registers, so the compiler above it
//! never names a machine register and the same compiler serves both hosts. Both encoders are
//! built on every host, which lets the tests check the aarch64 bytes on an x86-64 machine.
//!
//! The pinned registers, which the System V and AAPCS64 conventions both preserve across calls:
//!
//! | role | x86-64 | aarch64 |
//! | --- | --- | --- |
//! | the interpreter's activation | `rbx` | `x19` |
//! | the register window | `r12` | `x20` |
//! | the step table | `r13` | `x21` |
//! | the dispatch table | `r14` | `x22` |
//! | the `Interp` | `r15` | `x23` |
//!
//! A step's answer (the next pc) arrives in `rax` or `x0`, the accumulator below.

/// The instruction set to emit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    X64,
    A64,
}

impl Arch {
    /// The host's instruction set, if the baseline tier supports it.
    pub fn host() -> Option<Arch> {
        if cfg!(target_arch = "x86_64") {
            Some(Arch::X64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::A64)
        } else {
            None
        }
    }
}

/// A position in the code that branches name before it is bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label(u32);

/// Comparison outcomes a branch can test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Condition {
    Equal,
    NotEqual,
}

#[derive(Clone, Copy)]
enum Fixup {
    /// x86-64: a 32-bit displacement relative to the end of the four bytes at `at`.
    Rel32 { at: u32, label: Label },
    /// aarch64: a `B` with a 26-bit word offset.
    Branch26 { at: u32, label: Label },
    /// x86-64: `lea r14, [rip + table]`; aarch64: `adr x22, table` (21-bit byte offset).
    Table { at: u32 },
}

/// Machine code under construction, with labels resolved when [`Assembler::finish`] runs.
pub struct Assembler {
    arch: Arch,
    code: Vec<u8>,
    labels: Vec<u32>,
    fixups: Vec<Fixup>,
}

const UNBOUND: u32 = u32::MAX;

/// What [`Assembler::finish`] returns: the code and the jump table's position in it.
pub struct Finished {
    pub bytes: Vec<u8>,
}

/// Why emission failed.
#[derive(Debug)]
pub struct TooFar;

impl Assembler {
    pub fn new(arch: Arch) -> Assembler {
        Assembler {
            arch,
            code: Vec::with_capacity(4096),
            labels: Vec::new(),
            fixups: Vec::new(),
        }
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(UNBOUND);
        Label(self.labels.len() as u32 - 1)
    }

    pub fn bind(&mut self, label: Label) {
        self.labels[label.0 as usize] = self.code.len() as u32;
    }

    /// Saves the pinned registers and loads them from the activation, which arrives as the
    /// first argument. The second argument, the pc to start at, is left in the accumulator.
    pub fn prologue(&mut self, steps: u64, registers_offset: u32, interp_offset: u32) {
        match self.arch {
            Arch::X64 => {
                // Five pushes after the return address leave the stack 16-byte aligned for calls.
                self.bytes(&[0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]);
                // mov rbx, rdi
                self.bytes(&[0x48, 0x89, 0xFB]);
                // mov r12, [rbx + registers]
                self.bytes(&[0x4C, 0x8B, 0xA3]);
                self.u32(registers_offset);
                // mov r15, [rbx + interp]
                self.bytes(&[0x4C, 0x8B, 0xBB]);
                self.u32(interp_offset);
                // mov r13, steps
                self.bytes(&[0x49, 0xBD]);
                self.u64(steps);
                // lea r14, [rip + table]
                self.bytes(&[0x4C, 0x8D, 0x35]);
                self.fixups.push(Fixup::Table {
                    at: self.code.len() as u32,
                });
                self.u32(0);
                // mov rax, rsi
                self.bytes(&[0x48, 0x89, 0xF0]);
            }
            Arch::A64 => {
                // stp x29, x30, [sp, #-48]!
                self.word(0xA980_0000 | (0x7A << 15) | (30 << 10) | (31 << 5) | 29);
                // mov x29, sp
                self.word(0x9100_03FD);
                // stp x19, x20, [sp, #16]; stp x21, x22, [sp, #32]; str x23, [sp, #40]
                self.word(0xA900_0000 | (2 << 15) | (20 << 10) | (31 << 5) | 19);
                self.word(0xA900_0000 | (4 << 15) | (22 << 10) | (31 << 5) | 21);
                self.word(0xF900_0000 | (5 << 10) | (31 << 5) | 23);
                self.a64_mov(19, 0);
                self.a64_load(20, 19, registers_offset);
                self.a64_load(23, 19, interp_offset);
                self.a64_constant(21, steps);
                // adr x22, table
                self.fixups.push(Fixup::Table {
                    at: self.code.len() as u32,
                });
                self.word(0x1000_0000 | 22);
                self.a64_mov(0, 1);
            }
        }
    }

    /// Restores the pinned registers and returns.
    pub fn epilogue(&mut self) {
        match self.arch {
            Arch::X64 => {
                self.bytes(&[0x41, 0x5F, 0x41, 0x5E, 0x41, 0x5D, 0x41, 0x5C, 0x5B, 0xC3]);
            }
            Arch::A64 => {
                self.word(0xF940_0000 | (5 << 10) | (31 << 5) | 23);
                self.word(0xA940_0000 | (4 << 15) | (22 << 10) | (31 << 5) | 21);
                self.word(0xA940_0000 | (2 << 15) | (20 << 10) | (31 << 5) | 19);
                // ldp x29, x30, [sp], #48
                self.word(0xA8C0_0000 | (6 << 15) | (30 << 10) | (31 << 5) | 29);
                self.word(0xD65F_03C0);
            }
        }
    }

    /// Calls step `index` of the step table with the activation and `pc`; its answer lands in
    /// the accumulator.
    pub fn call_step(&mut self, index: usize, pc: u32) {
        let offset = (index * 8) as u32;
        match self.arch {
            Arch::X64 => {
                // mov rdi, rbx; mov esi, pc; call [r13 + offset]
                self.bytes(&[0x48, 0x89, 0xDF, 0xBE]);
                self.u32(pc);
                self.bytes(&[0x41, 0xFF, 0x95]);
                self.u32(offset);
            }
            Arch::A64 => {
                self.a64_mov(0, 19);
                self.a64_constant32(1, pc);
                self.a64_load(16, 21, offset);
                // blr x16
                self.word(0xD63F_0000 | (16 << 5));
            }
        }
    }

    /// Branches to `label` when the accumulator compares to `value` as `condition` says.
    pub fn branch_accumulator(&mut self, condition: Condition, value: u32, label: Label) {
        match self.arch {
            Arch::X64 => {
                // cmp rax, imm32 (sign-extended, so a pc stays below 2^31)
                self.bytes(&[0x48, 0x3D]);
                self.u32(value);
                self.x64_jcc(condition, label);
            }
            Arch::A64 => {
                if value < 4096 {
                    // cmp x0, #value
                    self.word(0xF100_001F | (value << 10));
                } else {
                    self.a64_constant32(17, value);
                    // cmp x0, x17
                    self.word(0xEB00_001F | (17 << 16));
                }
                self.a64_branch_if(condition, label);
            }
        }
    }

    /// Branches to `label` when the accumulator is the all-ones stop answer.
    pub fn branch_if_stop(&mut self, label: Label) {
        match self.arch {
            Arch::X64 => {
                // cmp rax, -1
                self.bytes(&[0x48, 0x83, 0xF8, 0xFF]);
                self.x64_jcc(Condition::Equal, label);
            }
            Arch::A64 => {
                // cmn x0, #1
                self.word(0xB100_041F);
                self.a64_branch_if(Condition::Equal, label);
            }
        }
    }

    /// Jumps to the code of the pc in the accumulator, through the jump table.
    pub fn jump_through_table(&mut self) {
        match self.arch {
            Arch::X64 => {
                // movsxd rax, dword [r14 + rax*4]; add rax, r14; jmp rax
                self.bytes(&[0x49, 0x63, 0x04, 0x86, 0x4C, 0x01, 0xF0, 0xFF, 0xE0]);
            }
            Arch::A64 => {
                // ldrsw x16, [x22, x0, lsl #2]; add x16, x22, x16; br x16
                self.word(0xB8A0_7800 | (22 << 5) | 16);
                self.word(0x8B00_0000 | (16 << 16) | (22 << 5) | 16);
                self.word(0xD61F_0000 | (16 << 5));
            }
        }
    }

    pub fn jump(&mut self, label: Label) {
        match self.arch {
            Arch::X64 => {
                self.code.push(0xE9);
                self.rel32(label);
            }
            Arch::A64 => self.a64_branch(label),
        }
    }

    /// Copies register `from` of the window to register `to`.
    pub fn copy_register(&mut self, to: u32, from: u32) {
        self.load_register_scratch(from);
        self.store_scratch_register(to);
    }

    /// Stores the 64-bit `value` into register `to` of the window.
    pub fn store_register_constant(&mut self, to: u32, value: u64) {
        match self.arch {
            Arch::X64 => {
                // mov rax, value
                self.bytes(&[0x48, 0xB8]);
                self.u64(value);
            }
            Arch::A64 => self.a64_constant(16, value),
        }
        self.store_scratch_register(to);
    }

    /// Stores the 64-bit `value` at `offset` in the `Interp`.
    pub fn store_interp_constant(&mut self, offset: u32, value: u64) {
        match self.arch {
            Arch::X64 => {
                // mov rax, value; mov [r15 + offset], rax
                self.bytes(&[0x48, 0xB8]);
                self.u64(value);
                self.bytes(&[0x49, 0x89, 0x87]);
                self.u32(offset);
            }
            Arch::A64 => {
                self.a64_constant(16, value);
                self.a64_address(17, 23, offset);
                self.word(0xF900_0000 | (17 << 5) | 16);
            }
        }
    }

    /// Stores the byte `value` at `offset` in the `Interp`.
    pub fn store_interp_byte(&mut self, offset: u32, value: u8) {
        match self.arch {
            Arch::X64 => {
                // mov byte [r15 + offset], value
                self.bytes(&[0x41, 0xC6, 0x87]);
                self.u32(offset);
                self.code.push(value);
            }
            Arch::A64 => {
                self.a64_address(17, 23, offset);
                // movz w16, #value; strb w16, [x17]
                self.word(0x5280_0000 | (u32::from(value) << 5) | 16);
                self.word(0x3900_0000 | (17 << 5) | 16);
            }
        }
    }

    /// Loads register `from` of the window into the object scratch register.
    pub fn load_object(&mut self, from: u32) {
        self.load_register_scratch(from);
    }

    /// Branches to `label` when the byte at `offset` in the loaded object compares to `value`
    /// as `condition` says.
    pub fn branch_object_byte(
        &mut self,
        offset: u8,
        value: u8,
        condition: Condition,
        label: Label,
    ) {
        match self.arch {
            Arch::X64 => {
                // movzx ecx, byte [rax + offset]; cmp ecx, value
                self.bytes(&[0x0F, 0xB6, 0x48, offset, 0x83, 0xF9, value]);
                self.x64_jcc(condition, label);
            }
            Arch::A64 => {
                // ldrb w17, [x16, #offset]; cmp w17, #value
                self.word(0x3940_0000 | (u32::from(offset) << 10) | (16 << 5) | 17);
                self.word(0x7100_001F | (u32::from(value) << 10) | (17 << 5));
                self.a64_branch_if(condition, label);
            }
        }
    }

    /// Branches to `label` when the byte at `offset` in the loaded object has any of `bits`
    /// set (`NotEqual`) or none of them (`Equal`).
    pub fn branch_object_bits(&mut self, offset: u8, bits: u8, condition: Condition, label: Label) {
        match self.arch {
            Arch::X64 => {
                // movzx ecx, byte [rax + offset]; test cl, bits
                self.bytes(&[0x0F, 0xB6, 0x48, offset, 0xF6, 0xC1, bits]);
                self.x64_jcc(condition, label);
            }
            Arch::A64 => {
                self.word(0x3940_0000 | (u32::from(offset) << 10) | (16 << 5) | 17);
                // and w17, w17, #bits (as a register operand); cmp w17, #0
                self.a64_constant32(9, u32::from(bits));
                self.word(0x0A00_0000 | (9 << 16) | (17 << 5) | 17);
                self.word(0x7100_001F | (17 << 5));
                self.a64_branch_if(condition, label);
            }
        }
    }

    /// Branches to `label` when the 64-bit word at `offset` in the loaded object compares to
    /// the small `value` as `condition` says.
    pub fn branch_object_word(
        &mut self,
        offset: u8,
        value: u8,
        condition: Condition,
        label: Label,
    ) {
        match self.arch {
            Arch::X64 => {
                // cmp qword [rax + offset], value
                self.bytes(&[0x48, 0x83, 0x78, offset, value]);
                self.x64_jcc(condition, label);
            }
            Arch::A64 => {
                // ldr x17, [x16, #offset]; cmp x17, #value
                self.word(0xF940_0000 | ((u32::from(offset) / 8) << 10) | (16 << 5) | 17);
                self.word(0xF100_001F | (u32::from(value) << 10) | (17 << 5));
                self.a64_branch_if(condition, label);
            }
        }
    }

    /// Branches to `label` when the 32-bit word at `offset` in the loaded object compares to
    /// `value` as `condition` says.
    pub fn branch_object_int(
        &mut self,
        offset: u8,
        value: u32,
        condition: Condition,
        label: Label,
    ) {
        match self.arch {
            Arch::X64 => {
                // cmp dword [rax + offset], value
                self.bytes(&[0x81, 0x78, offset]);
                self.u32(value);
                self.x64_jcc(condition, label);
            }
            Arch::A64 => {
                // ldr w17, [x16, #offset]; cmp w17, w9
                self.word(0xB940_0000 | ((u32::from(offset) / 4) << 10) | (16 << 5) | 17);
                self.a64_constant32(9, value);
                self.word(0x6B00_001F | (9 << 16) | (17 << 5));
                self.a64_branch_if(condition, label);
            }
        }
    }

    /// Branches to `label` when the loaded object is (`Equal`) or is not (`NotEqual`) the
    /// object at `address`.
    pub fn branch_object_is(&mut self, address: u64, condition: Condition, label: Label) {
        match self.arch {
            Arch::X64 => {
                // mov rcx, address; cmp rax, rcx
                self.bytes(&[0x48, 0xB9]);
                self.u64(address);
                self.bytes(&[0x48, 0x39, 0xC8]);
                self.x64_jcc(condition, label);
            }
            Arch::A64 => {
                self.a64_constant(17, address);
                // cmp x16, x17
                self.word(0xEB00_001F | (17 << 16) | (16 << 5));
                self.a64_branch_if(condition, label);
            }
        }
    }

    /// Resolves every label and appends the jump table: one 32-bit offset per pc, relative to
    /// the table, of the label `targets` gives for it.
    pub fn finish(mut self, targets: &[Label]) -> Result<Finished, TooFar> {
        // The table is read as 32-bit words, so it starts aligned.
        while self.code.len() % 4 != 0 {
            self.code.push(0);
        }
        let table = self.code.len() as i64;
        for target in targets {
            let at = i64::from(self.labels[target.0 as usize]);
            self.code
                .extend_from_slice(&i32::try_from(at - table).map_err(|_| TooFar)?.to_le_bytes());
        }
        for fixup in std::mem::take(&mut self.fixups) {
            match fixup {
                Fixup::Rel32 { at, label } => {
                    let target = i64::from(self.labels[label.0 as usize]);
                    let relative =
                        i32::try_from(target - (i64::from(at) + 4)).map_err(|_| TooFar)?;
                    self.patch(at, relative.to_le_bytes());
                }
                Fixup::Branch26 { at, label } => {
                    let target = i64::from(self.labels[label.0 as usize]);
                    let words = (target - i64::from(at)) / 4;
                    if !(-(1 << 25)..(1 << 25)).contains(&words) {
                        return Err(TooFar);
                    }
                    let word = 0x1400_0000 | (words as u32 & 0x03FF_FFFF);
                    self.patch(at, word.to_le_bytes());
                }
                Fixup::Table { at } => match self.arch {
                    Arch::X64 => {
                        let relative =
                            i32::try_from(table - (i64::from(at) + 4)).map_err(|_| TooFar)?;
                        self.patch(at, relative.to_le_bytes());
                    }
                    Arch::A64 => {
                        let relative = table - i64::from(at);
                        if !(-(1 << 20)..(1 << 20)).contains(&relative) {
                            return Err(TooFar);
                        }
                        let relative = relative as u32;
                        let word = 0x1000_0000
                            | ((relative & 3) << 29)
                            | (((relative >> 2) & 0x7FFFF) << 5)
                            | 22;
                        self.patch(at, word.to_le_bytes());
                    }
                },
            }
        }
        Ok(Finished { bytes: self.code })
    }

    fn patch(&mut self, at: u32, bytes: [u8; 4]) {
        let at = at as usize;
        self.code[at..at + 4].copy_from_slice(&bytes);
    }

    fn load_register_scratch(&mut self, from: u32) {
        let offset = from * 8;
        match self.arch {
            Arch::X64 => {
                // mov rax, [r12 + offset]
                self.bytes(&[0x49, 0x8B, 0x84, 0x24]);
                self.u32(offset);
            }
            Arch::A64 => self.a64_load(16, 20, offset),
        }
    }

    fn store_scratch_register(&mut self, to: u32) {
        let offset = to * 8;
        match self.arch {
            Arch::X64 => {
                // mov [r12 + offset], rax
                self.bytes(&[0x49, 0x89, 0x84, 0x24]);
                self.u32(offset);
            }
            Arch::A64 => {
                if offset / 8 < 4096 {
                    self.word(0xF900_0000 | ((offset / 8) << 10) | (20 << 5) | 16);
                } else {
                    self.a64_address(17, 20, offset);
                    self.word(0xF900_0000 | (17 << 5) | 16);
                }
            }
        }
    }

    fn x64_jcc(&mut self, condition: Condition, label: Label) {
        let opcode = match condition {
            Condition::Equal => 0x84,
            Condition::NotEqual => 0x85,
        };
        self.bytes(&[0x0F, opcode]);
        self.rel32(label);
    }

    fn rel32(&mut self, label: Label) {
        self.fixups.push(Fixup::Rel32 {
            at: self.code.len() as u32,
            label,
        });
        self.u32(0);
    }

    /// A conditional branch with the range of an unconditional one: the inverted condition
    /// skips a `B` to the label.
    fn a64_branch_if(&mut self, condition: Condition, label: Label) {
        let inverted = match condition {
            Condition::Equal => 1,
            Condition::NotEqual => 0,
        };
        // b.<inverted> +8
        self.word(0x5400_0000 | (2 << 5) | inverted);
        self.a64_branch(label);
    }

    fn a64_branch(&mut self, label: Label) {
        self.fixups.push(Fixup::Branch26 {
            at: self.code.len() as u32,
            label,
        });
        self.word(0x1400_0000);
    }

    fn a64_mov(&mut self, to: u32, from: u32) {
        // orr xto, xzr, xfrom
        self.word(0xAA00_03E0 | (from << 16) | to);
    }

    /// `ldr xto, [xbase, #offset]`, through a computed address when the offset is out of range.
    fn a64_load(&mut self, to: u32, base: u32, offset: u32) {
        if offset % 8 == 0 && offset / 8 < 4096 {
            self.word(0xF940_0000 | ((offset / 8) << 10) | (base << 5) | to);
        } else {
            self.a64_address(17, base, offset);
            self.word(0xF940_0000 | (17 << 5) | to);
        }
    }

    /// `xto = xbase + offset`.
    fn a64_address(&mut self, to: u32, base: u32, offset: u32) {
        if offset < 4096 {
            self.word(0x9100_0000 | (offset << 10) | (base << 5) | to);
        } else {
            self.a64_constant(to, u64::from(offset));
            // add xto, xbase, xto
            self.word(0x8B00_0000 | (to << 16) | (base << 5) | to);
        }
    }

    fn a64_constant(&mut self, to: u32, value: u64) {
        // movz xto, #lo; movk xto, #..., lsl #16/32/48
        self.word(0xD280_0000 | (((value & 0xFFFF) as u32) << 5) | to);
        for shift in 1..4 {
            let part = ((value >> (16 * shift)) & 0xFFFF) as u32;
            if part != 0 {
                self.word(0xF280_0000 | (shift << 21) | (part << 5) | to);
            }
        }
    }

    fn a64_constant32(&mut self, to: u32, value: u32) {
        self.word(0x5280_0000 | ((value & 0xFFFF) << 5) | to);
        if value >> 16 != 0 {
            self.word(0x72A0_0000 | ((value >> 16) << 5) | to);
        }
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn u32(&mut self, value: u32) {
        self.code.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.code.extend_from_slice(&value.to_le_bytes());
    }

    fn word(&mut self, word: u32) {
        self.u32(word);
    }
}
