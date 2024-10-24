//! PSX MIPS CPU implementation, including instruction cache
//!
//! The timings code is copied from mednafen

use super::cop0::Exception;
use super::{cop0, map, AccessWidth, Addressable, CycleCount, Psx};

#[cfg(feature = "debugger")]
use super::debugger;

use std::fmt;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cpu {
    /// Address of the instruction currently being executed. Used for
    /// setting the EPC in exceptions.
    current_pc: u32,
    /// The Program Counter register: points to the next instruction
    pc: u32,
    /// Next value for the PC, used to emulate the branch delay slot
    next_pc: u32,
    /// General Purpose Registers. The first entry (R0) must always contain 0
    regs: [u32; 32],
    /// HI register for division remainder and multiplication MSBs
    hi: u32,
    /// LO register for division quotient and multiplication LSBs
    lo: u32,
    /// Load initiated by the current instruction (will take effect after the load delay slot). The
    /// values in the triplet are: target register, value, number of cycles taken by the load
    load: Option<(RegisterIndex, u32, u8)>,
    /// If a load is taking place this is the register being targetted
    free_cycles_reg: RegisterIndex,
    /// `free_cycles[free_cycles_reg]` contains the number of cycles left before the last load (if
    /// any) completes. That means that at any given moment only one cell of this array is
    /// effectively in use but laying things out that way lets us avoid a bunch of branching in
    /// `reg_dep` which is executed once *per CPU register* for *every* instruction, so it gives us
    /// a significant speedup.
    free_cycles: [u8; 32],
    /// Set by the current instruction if a branch occurred and the next instruction will be in the
    /// delay slot.
    branch: bool,
    /// Instruction cache (256 4-word cachelines, for a total of 4KiB)
    #[serde(with = "serde_big_array::BigArray")]
    icache: [ICacheLine; 0x100],
    /// Set if the current instruction executes in the delay slot
    delay_slot: bool,
    /// If true BREAK instructions trigged the debugger instead of generating an exception
    debug_on_break: bool,
    /// Date at which the last division or multiplication will be done. DIV(U) and MULT(U) can run
    /// concurrently with other "normal" MIPS instructions and only block if a mf(hi|lo) is
    /// executed before they're finished
    mult_div_end: CycleCount,
    /// Date at which the last GTE operation will be done. GTE commands can run concurrently with
    /// main CPU instructions and will only stall is
    #[serde(default)]
    gte_command_end: CycleCount,
    /// Offset added to the index in the opcode jumptable when decoding instructions
    opcode_table_offset: u8,
}

impl Cpu {
    pub fn new() -> Cpu {
        // Reset value for the PC: beginning of BIOS ROM
        let reset_pc = 0xbfc0_0000;

        Cpu {
            current_pc: reset_pc,
            pc: reset_pc,
            next_pc: reset_pc.wrapping_add(4),
            // Not sure what the reset values of the general purpose registers is but it shouldn't
            // matter since the BIOS doesn't read them. R0 is always 0 however, so that shouldn't
            // be changed.
            regs: [0; 32],
            hi: 0,
            lo: 0,
            load: None,
            free_cycles_reg: RegisterIndex(0),
            free_cycles: [0; 32],
            branch: false,
            icache: [ICacheLine::new(); 0x100],
            delay_slot: false,
            debug_on_break: false,
            mult_div_end: 0,
            gte_command_end: 0,
            opcode_table_offset: 0,
        }
    }

    /// Returns the address of the instruction currently being executed
    pub fn current_pc(&self) -> u32 {
        self.current_pc
    }

    /// Force PC address. Meant to be used from the debugger. Use at your own risk.
    #[cfg(feature = "debugger")]
    pub fn force_pc(&mut self, pc: u32) {
        self.pc = pc;
        self.next_pc = self.pc.wrapping_add(4);
        self.delay_slot = false;
    }

    /// Returns true if the instruction currently being executed is in a delay slot
    pub fn in_delay_slot(&self) -> bool {
        self.delay_slot
    }

    /// Get the value of all general purpose registers
    #[cfg(feature = "debugger")]
    pub fn regs(&self) -> &[u32] {
        &self.regs
    }

    /// Get the value of the LO register
    #[cfg(feature = "debugger")]
    pub fn lo(&self) -> u32 {
        self.lo
    }

    /// Get the value of the HI register
    #[cfg(feature = "debugger")]
    pub fn hi(&self) -> u32 {
        self.hi
    }

    /// Rebase our internal counters that are relative to the global `cycle_counter`
    pub fn rebase_counters(&mut self, cycle_counter: CycleCount) {
        if self.mult_div_end > 0 {
            self.mult_div_end -= cycle_counter;
        }

        if self.gte_command_end > 0 {
            self.gte_command_end -= cycle_counter;
        }
    }

    /// Return the current value of register `index`
    fn reg(&self, index: RegisterIndex) -> u32 {
        self.regs[index.0 as usize]
    }

    /// Put `val` into register `index`. If `index` is 0 nothing happens as R0 always contains 0.
    fn set_reg(&mut self, index: RegisterIndex, val: u32) {
        self.regs[index.0 as usize] = val;

        // R0 always contains 0
        self.regs[0] = 0;
    }

    /// Branch to immediate value `offset`.
    fn branch(&mut self, offset: u32) {
        // Offset immediates are always shifted two places to the
        // right since `PC` addresses have to be aligned on 32bits at
        // all times.
        let offset = offset << 2;

        self.next_pc = self.pc.wrapping_add(offset);
        self.branch = true;
    }

    /// Execute and clear any pending load
    fn delayed_load(&mut self) {
        if let Some((reg, val, duration)) = self.load {
            self.set_reg(reg, val);

            self.free_cycles[reg.0 as usize] = duration;
            self.free_cycles_reg = reg;

            // We clear the load now that it's been executed
            self.load = None;
        }
    }

    /// Called when any currently-executing load needs to be synced
    fn load_sync(&mut self) {
        self.free_cycles[self.free_cycles_reg.0 as usize] = 0;
    }

    /// Execute the pending delayed and setup the next one. If the new load targets the same
    /// register as the current one then the older one is cancelled (i.e. it never makes it to the
    /// register).
    ///
    /// This method should be used instead of `delayed_load` for instructions that setup a delayed
    /// load.
    fn delayed_load_chain(&mut self, reg: RegisterIndex, val: u32, duration: u8, sync: bool) {
        if let Some((pending_reg, pending_val, duration)) = self.load {
            // This takes care of the following situation:
            //
            //    lw   $t0, 0($s0)
            //    lw   $t0, 0($s1)
            //    move $t0, $s1
            //
            // In this situation the 2nd LW targets the same register an the one just before.
            // In this scenario the first load never completes and the value of T0 in the move
            // won't have been modified by either LW (the first one being interrupted by the
            // second one, and the second one not having yet finished since we're in the delay
            // slot).
            if pending_reg != reg {
                // We target a different register, we can execute the delay load.
                self.set_reg(pending_reg, pending_val);

                // If the calling function doesn't force a sync (i.e. the load doesn't come from
                // memory) we can also set the free_cycles as usual since the load can run in the
                // background while other instruction executes
                if !sync {
                    self.free_cycles[pending_reg.0 as usize] = duration;
                    self.free_cycles_reg = pending_reg;
                }
            }
        }

        self.load = Some((reg, val, duration));
    }
}

impl fmt::Debug for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f)?;
        writeln!(f, "PC: 0x{:08x}", self.pc)?;

        for i in 0..16 {
            writeln!(
                f,
                "{}: 0x{:08x}    {}: 0x{:08x}",
                REGISTER_NAMES[i],
                self.regs[i],
                REGISTER_NAMES[i + 16],
                self.regs[i + 16]
            )?;
        }

        Ok(())
    }
}

/// Called whenever the IRQ state has potentially changed
pub fn irq_changed(psx: &mut Psx) {
    psx.cpu.opcode_table_offset = if cop0::irq_pending(psx) {
        // Use the 2nd half of the jump table
        64
    } else {
        // Use the normal table
        0
    };
}

pub fn run_next_instruction(psx: &mut Psx) {
    // Explanation of the various *pc variables:
    //
    // * `psx.cpucurrent_pc`: Pointer to the instruction about to be executed.
    //
    // * `psx.cpu.pc`: Pointer to the next instruction to be executed. It's possible for this value
    //                 to change before the next instruction is reached if an exception occurs
    //                 (exceptions have no delay slot).
    //
    // * `psx.cpu.next_pc`: Value `psx.cpu.pc` will take on the *next* cycle, so effectively a
    //                      pointer to the next next instruction being executed. It's possible for
    //                      this value to change before the next instruction is reached if an
    //                      exception *or* a branch/jump occurs. We can't change `psx.cpu.pc`
    //                      directly in case of a branch because we need to emulate the branch
    //                      delay slot.
    //
    // So basically when a branch/jump is executed only `psx.cpu.next_pc` is modified, which means
    // that the value of the next instruction to be executed (pointed at by `psx.cpu.pc`) remains
    // in the pipeline. Thus the branch delay slot is emulated accurately.
    psx.cpu.current_pc = psx.cpu.pc;
    psx.cpu.pc = psx.cpu.next_pc;
    psx.cpu.next_pc = psx.cpu.pc.wrapping_add(4);

    // If the last instruction was a branch then we're in the delay slot
    psx.cpu.delay_slot = psx.cpu.branch;
    psx.cpu.branch = false;

    // Debugger entrypoint: used for code breakpoints and stepping
    #[cfg(feature = "debugger")]
    {
        debugger::pc_change(psx);
    }

    if psx.cpu.current_pc % 4 != 0 {
        // PC is not correctly aligned!
        exception(psx, Exception::LoadAddressError);
        return;
    }

    // Fetch instruction at PC
    let instruction = fetch_instruction(psx);

    instruction_tick(psx);

    let opcode_index = instruction.opcode() | psx.cpu.opcode_table_offset as usize;

    let handler = OPCODE_HANDLERS[opcode_index];

    handler(psx, instruction);
}

/// Advance the CPU cycle counter by one tick unless we're still catching up with a load
pub fn instruction_tick(psx: &mut Psx) {
    let r = psx.cpu.free_cycles_reg;
    let free_cycles = &mut psx.cpu.free_cycles[r.0 as usize];

    if *free_cycles > 0 {
        // We're still catching up with a load. Since `load` advances the cycle counter to the
        // end of the load it means that we're still catching up, so we don't do anything
        *free_cycles -= 1;
    } else {
        // We're in sync, we can move the time forward
        psx.tick(1);
    }
}

/// Fetch the instruction at `current_pc` through the instruction cache
fn fetch_instruction(psx: &mut Psx) -> Instruction {
    let pc = psx.cpu.current_pc;

    // KUSEG and KSEG0 regions are cached. KSEG1 is uncached and
    // KSEG2 doesn't contain any code
    let cached = pc < 0xa000_0000;

    if cached && psx.icache_enabled() {
        // The MSB is ignored: running from KUSEG or KSEG0 hits the same cachelines. So for
        // instance addresses 0x00000000 and 0x80000000 have the same tag and you can jump from one
        // to the other without having to reload the cache.

        // Cache tag: bits [30:12]
        let tag = pc & 0x7fff_f000;
        // Cache line "bucket": bits [11:4]
        let line_off = ((pc >> 4) & 0xff) as usize;
        // Index in the cache line: bits [3:2]
        let index = (pc >> 2) & 3;

        // Fetch the cacheline for this address
        let mut line = psx.cpu.icache[line_off];

        // Check the tag and validity
        if line.tag() != tag || line.valid_index() > index {
            // Cache miss. Fetch the cacheline starting at the current index. If the index is not 0
            // then some words are going to remain invalid in the cacheline.
            let mut cpc = pc;

            // We're about to access the memory to fetch the instructions, we need to finish any
            // active load first
            psx.cpu.load_sync();

            // Cache timing lifted straight from Mednafen
            psx.tick(7 - index as i32);

            for i in index..4 {
                let instruction = psx.xmem.load_instruction(cpc);

                line.set_instruction(i, instruction);
                cpc += 4;
            }

            // Set the tag and valid bits
            line.set_tag_valid(pc);

            // Store updated cacheline
            psx.cpu.icache[line_off] = line;
        }

        // Cache line is now guaranteed to be valid
        line.instruction(index)
    } else {
        // XXX Apparently pointing the PC to KSEG2 causes a bus error no matter what, even if you
        // point it at some valid register address (like the "cache control" register). Not like it
        // should happen anyway, there's nowhere to put code in KSEG2, only a bunch of registers.

        // We need to wait for any active load to finish before we can fetch the instruction
        psx.cpu.load_sync();

        // When running without a cache the penalty is about 4 cycles per instruction, sometimes
        // more (but on average for typical code fairly close to 4). This is therefore a bit
        // optimistic but it would be pretty tricky to emulate the pipeline more accurately and
        // running a tiny bit too fast shouldn't be too much of a problem, this isn't a Game Boy.
        psx.tick(4);

        psx.xmem.load_instruction(pc)
    }
}

/// Handle writes when the cache is isolated
pub fn cache_store<T: Addressable>(psx: &mut Psx, addr: u32, val: T) {
    // Implementing full cache emulation requires handling many corner cases. For now I'm just
    // going to add support for cache invalidation which is the only use case for cache isolation
    // as far as I know.
    let val = val.as_u32();

    if !psx.icache_enabled() {
        panic!("Cache maintenance while instruction cache is disabled");
    }

    if T::width() != AccessWidth::Word || val != 0 {
        panic!("Unsupported write while cache is isolated: {:08x}", val);
    }

    let line_off = ((addr >> 4) & 0xff) as usize;

    // Fetch the cacheline for this address
    let mut line = psx.cpu.icache[line_off];

    if psx.tag_test_mode() {
        // In tag test mode the write invalidates the entire targeted cacheline
        line.invalidate();
    } else {
        // Otherwise the write ends up directly in the cache.
        let index = (addr >> 2) & 3;

        let instruction = Instruction(val);

        line.set_instruction(index, instruction);
    }

    psx.cpu.icache[line_off] = line;
}

/// Trigger an exception
fn exception(psx: &mut Psx, cause: Exception) {
    // Update the status register
    let handler_addr = cop0::enter_exception(psx, cause);

    // Exceptions don't have a branch delay, we jump directly into
    // the handler
    psx.cpu.pc = handler_addr;
    psx.cpu.next_pc = handler_addr.wrapping_add(4);
}

/// Execute a memory write
fn store<T: Addressable>(psx: &mut Psx, addr: u32, v: T) {
    if psx.cop0.cache_isolated() {
        // When the cache is isolated the CPU writes don't reach the system bus, instead they end
        // up in the cache.
        cache_store(psx, addr, v);
        return;
    }

    #[cfg(feature = "debugger")]
    {
        debugger::memory_write(psx, addr);
    }

    psx.store(addr, v);
}

/// Execute a memory read and return the value alongside with the number of cycles necessary for
/// the load to complete;
fn load<T: Addressable>(psx: &mut Psx, addr: u32, from_lwc: bool) -> (T, u8) {
    // Any pending load must terminate before we attempt to start a new one
    psx.cpu.load_sync();

    #[cfg(feature = "debugger")]
    {
        debugger::memory_read(psx, addr);
    }

    // The Scratch Pad is the CPU data cache, it therefore has very low latency and needs to be
    // special-cased
    {
        // XXX Scratch Pad can't be accessed through uncached address space, so this is a bit too
        // aggressive.
        let abs_addr = map::mask_region(addr);

        if let Some(offset) = map::SCRATCH_PAD.contains(abs_addr) {
            return (psx.scratch_pad.load(offset), 0);
        }
    }

    if psx.cpu.load.is_none() {
        // From mednafen: apparently the CPU manages to schedule loads faster if they happen in a
        // row?
        psx.tick(2);
    }

    let prev_cc = psx.cycle_counter;

    let v = psx.load(addr);

    // From mednafen: delay to complete the load
    let d = if from_lwc { 1 } else { 2 };
    psx.tick(d);

    // Compute the duration of the load. The CPU (if possible) keeps executing instructions
    // while the load takes place, so effectively at this point `psx.cpu.cycle_counter` is too far
    // ahead by `duration` cycles, so `instruction_tick` will actually skip cycles until we catch
    // up.
    let duration = psx.cycle_counter - prev_cc;

    // The duration of an instruction should be a small number that fits easily in an u8 to save
    // some space (and some cache)
    debug_assert!(duration < 0x100);

    (v, duration as u8)
}

/// Handle pipeline timings for register dependencies. Should be called for every CPU registers
/// used as an input or output. Returns `r` to allow chaining.
fn reg_dep(psx: &mut Psx, r: RegisterIndex) -> RegisterIndex {
    // R0 is always "free" to read or write, so it doesn't force a sync when used as a register. In
    // order to emulate this we can just save and restore the value of the cycle counter for R0 to
    // make this function a NOP if `r` is R0 without having to use any branching.
    let c0 = psx.cpu.free_cycles[0];

    // If the register was executing a load we have to wait for it to complete before we can
    // continue (this is true even if `r` is used as an output register).
    psx.cpu.free_cycles[r.0 as usize] = 0;

    psx.cpu.free_cycles[0] = c0;

    r
}

/// When the main opcode is 0 we need to dispatch through a secondary table based on bits [5:0] of
/// the instruction
fn op_function(psx: &mut Psx, instruction: Instruction) {
    let handler = FUNCTION_HANDLERS[instruction.function()];

    handler(psx, instruction);
}

/// Shift Left Logical
///
/// `SLL $r0, $r0, 0` (machine code 0x0000_0000) is the idiomatic way of encoding a NOP
fn op_sll(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.shift();
    let t = reg_dep(psx, instruction.t());
    let d = reg_dep(psx, instruction.d());

    let v = psx.cpu.reg(t) << i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Shift Right Logical
fn op_srl(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.shift();
    let t = reg_dep(psx, instruction.t());
    let d = reg_dep(psx, instruction.d());

    let v = psx.cpu.reg(t) >> i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Shift Right Arithmetic
fn op_sra(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.shift();
    let t = reg_dep(psx, instruction.t());
    let d = reg_dep(psx, instruction.d());

    let v = (psx.cpu.reg(t) as i32) >> i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v as u32);
}

/// Shift Left Logical Variable
fn op_sllv(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    // Shift amount is truncated to 5 bits
    let v = psx.cpu.reg(t) << (psx.cpu.reg(s) & 0x1f);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Shift Right Logical Variable
fn op_srlv(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    // Shift amount is truncated to 5 bits
    let v = psx.cpu.reg(t) >> (psx.cpu.reg(s) & 0x1f);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Shift Right Arithmetic Variable
fn op_srav(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    // Shift amount is truncated to 5 bits
    let v = (psx.cpu.reg(t) as i32) >> (psx.cpu.reg(s) & 0x1f);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v as u32);
}

/// Jump Register
fn op_jr(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());

    psx.cpu.next_pc = psx.cpu.reg(s);
    psx.cpu.branch = true;

    psx.cpu.delayed_load();
}

/// Jump And Link Register
fn op_jalr(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let d = reg_dep(psx, instruction.d());

    let ra = psx.cpu.next_pc;

    psx.cpu.next_pc = psx.cpu.reg(s);
    psx.cpu.branch = true;

    psx.cpu.delayed_load();

    // Store return address in `d`
    psx.cpu.set_reg(d, ra);
}

/// System Call
fn op_syscall(psx: &mut Psx, _: Instruction) {
    exception(psx, Exception::SysCall);
}

/// Break
fn op_break(psx: &mut Psx, _: Instruction) {
    #[cfg(feature = "debugger")]
    {
        if psx.cpu.debug_on_break {
            info!("BREAK instruction while debug_on_break is active");
            debugger::trigger_break(psx);
            return;
        }
    }
    exception(psx, Exception::Break);
}

/// Block if the current DIV(U) or MULT(U) instruction has not yet finished
fn sync_mult_div(psx: &mut Psx) {
    let block_for = psx.cpu.mult_div_end - psx.cycle_counter;

    if block_for == 1 {
        // XXX timing hack from mednafen, if we only have one cycle left we ignore it. We should
        // really just implement proper timing from psxact
        return;
    }

    if block_for > 0 {
        psx.cycle_counter = psx.cpu.mult_div_end;

        let ri = psx.cpu.free_cycles_reg.0 as usize;

        if CycleCount::from(psx.cpu.free_cycles[ri]) <= block_for {
            psx.cpu.free_cycles[ri] = 0;
        } else {
            psx.cpu.free_cycles[ri] -= block_for as u8;
        }
    }
}

/// Move From HI
fn op_mfhi(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());

    let hi = psx.cpu.hi;

    psx.cpu.delayed_load();

    sync_mult_div(psx);

    psx.cpu.set_reg(d, hi);
}

/// Move to HI
fn op_mthi(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());

    psx.cpu.hi = psx.cpu.reg(s);

    psx.cpu.delayed_load();
}

/// Move From LO
fn op_mflo(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());

    let lo = psx.cpu.lo;

    psx.cpu.delayed_load();

    sync_mult_div(psx);

    psx.cpu.set_reg(d, lo);
}

/// Move to LO
fn op_mtlo(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());

    psx.cpu.lo = psx.cpu.reg(s);

    psx.cpu.delayed_load();
}

/// Multiply (signed)
fn op_mult(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let a = psx.cpu.reg(s) as i32;
    let b = psx.cpu.reg(t) as i32;

    let res = i64::from(a) * i64::from(b);
    let res = res as u64;

    psx.cpu.delayed_load();

    psx.cpu.hi = (res >> 32) as u32;
    psx.cpu.lo = res as u32;

    let timing_index = if a < 0 {
        (!a).leading_zeros()
    } else {
        a.leading_zeros()
    };

    let penalty = CycleCount::from(MULT_TIMINGS[timing_index as usize]);

    psx.cpu.mult_div_end = psx.cycle_counter + penalty;
}

/// Multiply Unsigned
fn op_multu(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let a = psx.cpu.reg(s);
    let b = psx.cpu.reg(t);

    let res = u64::from(a) * u64::from(b);

    psx.cpu.delayed_load();

    psx.cpu.hi = (res >> 32) as u32;
    psx.cpu.lo = res as u32;

    let penalty = CycleCount::from(MULT_TIMINGS[a.leading_zeros() as usize]);

    psx.cpu.mult_div_end = psx.cycle_counter + penalty;
}

/// Divide (signed)
fn op_div(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let n = psx.cpu.reg(s) as i32;
    let d = psx.cpu.reg(t) as i32;

    psx.cpu.delayed_load();

    if d == 0 {
        // Division by zero, results are bogus
        psx.cpu.hi = n as u32;

        if n >= 0 {
            psx.cpu.lo = 0xffff_ffff;
        } else {
            psx.cpu.lo = 1;
        }
    } else if n as u32 == 0x8000_0000 && d == -1 {
        // Result is not representable in a 32bit signed integer
        psx.cpu.hi = 0;
        psx.cpu.lo = 0x8000_0000;
    } else {
        psx.cpu.hi = (n % d) as u32;
        psx.cpu.lo = (n / d) as u32;
    }

    psx.cpu.mult_div_end = psx.cycle_counter + 37;
}

/// Divide Unsigned
fn op_divu(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let n = psx.cpu.reg(s);
    let d = psx.cpu.reg(t);

    psx.cpu.delayed_load();

    if d == 0 {
        // Division by zero, results are bogus
        psx.cpu.hi = n;
        psx.cpu.lo = 0xffff_ffff;
    } else {
        psx.cpu.hi = n % d;
        psx.cpu.lo = n / d;
    }

    psx.cpu.mult_div_end = psx.cycle_counter + 37;
}

/// Add and check for signed overflow
fn op_add(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());
    let d = reg_dep(psx, instruction.d());

    let s = psx.cpu.reg(s) as i32;
    let t = psx.cpu.reg(t) as i32;

    psx.cpu.delayed_load();

    match s.checked_add(t) {
        Some(v) => psx.cpu.set_reg(d, v as u32),
        None => exception(psx, Exception::Overflow),
    }
}

/// Add Unsigned
fn op_addu(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());
    let d = reg_dep(psx, instruction.d());

    let v = psx.cpu.reg(s).wrapping_add(psx.cpu.reg(t));

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Subtract and check for signed overflow
fn op_sub(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());
    let d = reg_dep(psx, instruction.d());

    let s = psx.cpu.reg(s) as i32;
    let t = psx.cpu.reg(t) as i32;

    psx.cpu.delayed_load();

    match s.checked_sub(t) {
        Some(v) => psx.cpu.set_reg(d, v as u32),
        None => exception(psx, Exception::Overflow),
    }
}

/// Subtract Unsigned
fn op_subu(psx: &mut Psx, instruction: Instruction) {
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());
    let d = reg_dep(psx, instruction.d());

    let v = psx.cpu.reg(s).wrapping_sub(psx.cpu.reg(t));

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Bitwise And
fn op_and(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let v = psx.cpu.reg(s) & psx.cpu.reg(t);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Bitwise Or
fn op_or(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let v = psx.cpu.reg(s) | psx.cpu.reg(t);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Bitwise Exclusive Or
fn op_xor(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let v = psx.cpu.reg(s) ^ psx.cpu.reg(t);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Bitwise Not Or
fn op_nor(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let v = !(psx.cpu.reg(s) | psx.cpu.reg(t));

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v);
}

/// Set on Less Than (signed)
fn op_slt(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let s = psx.cpu.reg(s) as i32;
    let t = psx.cpu.reg(t) as i32;

    let v = s < t;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v as u32);
}

/// Set on Less Than Unsigned
fn op_sltu(psx: &mut Psx, instruction: Instruction) {
    let d = reg_dep(psx, instruction.d());
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let v = psx.cpu.reg(s) < psx.cpu.reg(t);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(d, v as u32);
}

/// Various branch instructions: BGEZ, BLTZ, BGEZAL, BLTZAL. Bits [20:16] are used to figure out
/// which one to use
fn op_bxx(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let s = reg_dep(psx, instruction.s());

    let instruction = instruction.0;

    let is_bgez = (instruction >> 16) & 1;
    // It's not enough to test for bit 20 to see if we're supposed
    // to link, if any bit in the range [19:17] is set the link
    // doesn't take place and RA is left untouched.
    let is_link = (instruction >> 17) & 0xf == 0x8;

    let v = psx.cpu.reg(s) as i32;

    // Test "less than zero"
    let test = (v < 0) as u32;

    // If the test is "greater than or equal to zero" we need to
    // negate the comparison above ("a >= 0" <=> "!(a < 0)"). The
    // xor takes care of that.
    let test = test ^ is_bgez;

    psx.cpu.delayed_load();

    // If linking is requested it occurs unconditionally, even if
    // the branch is not taken
    if is_link {
        let ra = psx.cpu.next_pc;

        // Store return address in R31
        psx.cpu.set_reg(RegisterIndex(31), ra);
    }

    if test != 0 {
        psx.cpu.branch(i);
    }
}

/// Jump
fn op_j(psx: &mut Psx, instruction: Instruction) {
    let target = instruction.imm_jump();

    // In order to fit the immediate target in the instruction the bottom two bits are stripped
    // (see the implementation of `imm_jump`) but that still only leaves 26 bits to store 30 bits.
    // As a workaround the 4 MSBs are simply copied from the PC. That means that the effective
    // range of this instruction is limited and it can't reach any location in memory, in
    // particular it can't be used to switch from one area to an other (like, say, from KUSEG to
    // KSEG0).
    psx.cpu.next_pc = (psx.cpu.pc & 0xf000_0000) | target;
    psx.cpu.branch = true;

    psx.cpu.delayed_load();
}

/// Jump And Link
fn op_jal(psx: &mut Psx, instruction: Instruction) {
    let ra = psx.cpu.next_pc;
    let target = instruction.imm_jump();

    reg_dep(psx, RegisterIndex(31));

    psx.cpu.next_pc = (psx.cpu.pc & 0xf000_0000) | target;
    psx.cpu.branch = true;

    psx.cpu.delayed_load();

    // Store return address in R31
    psx.cpu.set_reg(RegisterIndex(31), ra);
}

/// Branch if Equal
fn op_beq(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    if psx.cpu.reg(s) == psx.cpu.reg(t) {
        psx.cpu.branch(i);
    }

    psx.cpu.delayed_load();
}

/// Branch if Not Equal
fn op_bne(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    if psx.cpu.reg(s) != psx.cpu.reg(t) {
        psx.cpu.branch(i);
    }

    psx.cpu.delayed_load();
}

/// Branch if Less than or Equal to Zero
fn op_blez(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let s = reg_dep(psx, instruction.s());

    let v = psx.cpu.reg(s) as i32;

    if v <= 0 {
        psx.cpu.branch(i);
    }

    psx.cpu.delayed_load();
}

/// Branch if Greater Than Zero
fn op_bgtz(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let s = reg_dep(psx, instruction.s());

    let v = psx.cpu.reg(s) as i32;

    if v > 0 {
        psx.cpu.branch(i);
    }

    psx.cpu.delayed_load();
}

/// Add Immediate and check for signed overflow
fn op_addi(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se() as i32;
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let s = psx.cpu.reg(s) as i32;

    psx.cpu.delayed_load();

    match s.checked_add(i) {
        Some(v) => psx.cpu.set_reg(t, v as u32),
        None => exception(psx, Exception::Overflow),
    }
}

/// Add Immediate Unsigned
fn op_addiu(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let v = psx.cpu.reg(s).wrapping_add(i);

    psx.cpu.delayed_load();

    psx.cpu.set_reg(t, v);
}

/// Set if Less Than Immediate (signed)
fn op_slti(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se() as i32;
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let v = (psx.cpu.reg(s) as i32) < i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(t, v as u32);
}

/// Set if Less Than Immediate Unsigned
fn op_sltiu(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let s = reg_dep(psx, instruction.s());
    let t = reg_dep(psx, instruction.t());

    let v = psx.cpu.reg(s) < i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(t, v as u32);
}

/// Bitwise And Immediate
fn op_andi(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let v = psx.cpu.reg(s) & i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(t, v);
}

/// Bitwise Or Immediate
fn op_ori(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let v = psx.cpu.reg(s) | i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(t, v);
}

/// Bitwise eXclusive Or Immediate
fn op_xori(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let v = psx.cpu.reg(s) ^ i;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(t, v);
}

/// Load Upper Immediate
fn op_lui(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm();
    let t = reg_dep(psx, instruction.t());

    // Low 16bits are set to 0
    let v = i << 16;

    psx.cpu.delayed_load();

    psx.cpu.set_reg(t, v);
}

/// Coprocessor 0 opcode
fn op_cop0(psx: &mut Psx, instruction: Instruction) {
    match instruction.cop_opcode() {
        0b00000 => op_mfc0(psx, instruction),
        0b00100 => op_mtc0(psx, instruction),
        0b10000 => op_rfe(psx, instruction),
        _ => panic!("Unhandled cop0 instruction {}", instruction),
    }
}

/// Move To Coprocessor 0
fn op_mtc0(psx: &mut Psx, instruction: Instruction) {
    // Mednafen doesn't force the register sync if a load was in progress here. It doesn't make a
    // lot of sense to me, maybe it's a mistake or maybe it compensates from something else. For
    // the time being just do whatever mednafen does.
    let cpu_r = instruction.t();
    let cop_r = instruction.d();

    let v = psx.cpu.reg(cpu_r);

    psx.cpu.delayed_load();

    cop0::mtc0(psx, cop_r, v);
}

/// Move From Coprocessor 0
fn op_mfc0(psx: &mut Psx, instruction: Instruction) {
    let cpu_r = reg_dep(psx, instruction.t());
    let cop_r = instruction.d();

    let v = cop0::mfc0(psx, cop_r);

    psx.cpu.delayed_load_chain(cpu_r, v, 0, false);
}

/// Return From Exception. Doesn't actually jump anywhere but tells the coprocessor to return to
/// the mode it was in when the exception occurred.
fn op_rfe(psx: &mut Psx, instruction: Instruction) {
    psx.cpu.delayed_load();

    // There are other instructions with the same encoding but all are virtual memory related and
    // the PlayStation doesn't implement them. Still, let's make sure we're not running buggy code.
    if instruction.0 & 0x3f != 0b01_0000 {
        panic!("Invalid cop0 instruction: {}", instruction);
    }

    cop0::return_from_exception(psx);
}

/// Coprocessor 1 opcode (does not exist on the PlayStation)
fn op_cop1(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered Cop1 instruction");

    exception(psx, Exception::CoprocessorError);
}

/// Coprocessor 2 opcode (GTE)
fn op_cop2(psx: &mut Psx, instruction: Instruction) {
    if psx.cpu.gte_command_end > psx.cycle_counter {
        psx.cycle_counter = psx.cpu.gte_command_end;
    }

    // XXX: we should check that the GTE is enabled in cop0's status register, otherwise the cop2
    // instructions seem to freeze the CPU (or maybe raise an exception?). Furthermore it seems
    // that one has to wait at least two cycles (tested with two nops) after raising the flag in
    // the status register before the GTE can be accessed.
    let cop_opcode = instruction.cop_opcode();

    if cop_opcode & 0x10 != 0 {
        // GTE command

        psx.cpu.delayed_load();

        psx.cpu.gte_command_end = psx.cycle_counter + psx.gte.command(instruction.0);
    } else {
        match cop_opcode {
            0b00000 => op_mfc2(psx, instruction),
            0b00010 => op_cfc2(psx, instruction),
            0b00100 => op_mtc2(psx, instruction),
            0b00110 => op_ctc2(psx, instruction),
            n => unimplemented!("GTE opcode {:x}", n),
        }
    }
}

/// Move From Coprocessor 2 Data register
fn op_mfc2(psx: &mut Psx, instruction: Instruction) {
    let block_for = psx.cpu.gte_command_end - psx.cycle_counter;

    let delay = if block_for > 0 {
        psx.cycle_counter = psx.cpu.gte_command_end;
        block_for as u8
    } else {
        0
    };

    let cpu_r = instruction.t();
    let cop_r = instruction.d().0;

    let v = psx.gte.data(cop_r);

    psx.cpu.delayed_load_chain(cpu_r, v, delay, false);
}

/// Move From Coprocessor 2 Control register
fn op_cfc2(psx: &mut Psx, instruction: Instruction) {
    let block_for = psx.cpu.gte_command_end - psx.cycle_counter;

    let delay = if block_for > 0 {
        psx.cycle_counter = psx.cpu.gte_command_end;
        block_for as u8
    } else {
        0
    };

    let cpu_r = instruction.t();
    let cop_r = instruction.d().0;

    let v = psx.gte.control(cop_r);

    psx.cpu.delayed_load_chain(cpu_r, v, delay, false);
}

/// Move To Coprocessor 2 Data register
fn op_mtc2(psx: &mut Psx, instruction: Instruction) {
    if psx.cpu.gte_command_end > psx.cycle_counter {
        psx.cycle_counter = psx.cpu.gte_command_end;
    }

    // Mednafen doesn't force the register sync if a load was in progress here. It doesn't make a
    // lot of sense to me, maybe it's a mistake or maybe it compensates from something else. For
    // the time being just do whatever mednafen does.
    let cpu_r = instruction.t();
    let cop_r = instruction.d().0;

    let v = psx.cpu.reg(cpu_r);

    psx.cpu.delayed_load();

    psx.gte.set_data(cop_r, v);
}

/// Move To Coprocessor 2 Control register
fn op_ctc2(psx: &mut Psx, instruction: Instruction) {
    if psx.cpu.gte_command_end > psx.cycle_counter {
        psx.cycle_counter = psx.cpu.gte_command_end;
    }

    // Mednafen doesn't force the register sync if a load was in progress here. It doesn't make a
    // lot of sense to me, maybe it's a mistake or maybe it compensates from something else. For
    // the time being just do whatever mednafen does.
    let cpu_r = instruction.t();
    let cop_r = instruction.d().0;

    let v = psx.cpu.reg(cpu_r);

    psx.cpu.delayed_load();

    psx.gte.set_control(cop_r, v);
}

/// Coprocessor 3 opcode (does not exist on the PlayStation)
fn op_cop3(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered Cop3 instruction");

    exception(psx, Exception::CoprocessorError);
}

/// Load Byte (signed)
fn op_lb(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    let (v, duration) = load::<u8>(psx, addr, false);

    // Cast as i8 to force sign extension
    let v = v as i8;

    // Put the load in the delay slot
    psx.cpu.delayed_load_chain(t, v as u32, duration, true);
}

/// Load Halfword (signed)
fn op_lh(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    if addr % 2 == 0 {
        let (v, duration) = load::<u16>(psx, addr, false);

        // Cast as i16 to force sign extension
        let v = v as i16;

        // Put the load in the delay slot
        psx.cpu.delayed_load_chain(t, v as u32, duration, true);
    } else {
        psx.cpu.delayed_load();
        exception(psx, Exception::LoadAddressError);
    }
}

/// Load Word Left
fn op_lwl(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    let mut cur_v = psx.cpu.reg(t);

    // This instruction bypasses the load delay restriction: this instruction will merge the new
    // contents with the value currently being loaded if need be.
    if let Some((pending_reg, pending_value, _)) = psx.cpu.load {
        if pending_reg == t {
            cur_v = pending_value;
        }
    }

    // Next we load the *aligned* word containing the first addressed byte
    let aligned_addr = addr & !3;
    let (aligned_word, duration) = load::<u32>(psx, aligned_addr, false);

    // Depending on the address alignment we fetch the 1, 2, 3 or 4 *most* significant bytes and
    // put them in the target register.
    let v = match addr & 3 {
        0 => (cur_v & 0x00ff_ffff) | (aligned_word << 24),
        1 => (cur_v & 0x0000_ffff) | (aligned_word << 16),
        2 => (cur_v & 0x0000_00ff) | (aligned_word << 8),
        3 => aligned_word,
        _ => unreachable!(),
    };

    // Put the load in the delay slot
    psx.cpu.delayed_load_chain(t, v, duration, true);
}

/// Load Word
fn op_lw(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    // Address must be 32bit aligned
    if addr % 4 == 0 {
        let (v, duration) = load(psx, addr, false);

        psx.cpu.delayed_load_chain(t, v, duration, true);
    } else {
        psx.cpu.delayed_load();
        exception(psx, Exception::LoadAddressError);
    }
}

/// Load Byte Unsigned
fn op_lbu(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    let (v, duration) = load::<u8>(psx, addr, false);

    // Put the load in the delay slot
    psx.cpu.delayed_load_chain(t, u32::from(v), duration, true);
}

/// Load Halfword Unsigned
fn op_lhu(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    // Address must be 16bit aligned
    if addr % 2 == 0 {
        let (v, duration) = load::<u16>(psx, addr, false);

        // Put the load in the delay slot
        psx.cpu.delayed_load_chain(t, u32::from(v), duration, true);
    } else {
        psx.cpu.delayed_load();
        exception(psx, Exception::LoadAddressError);
    }
}

/// Load Word Right
fn op_lwr(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    let mut cur_v = psx.cpu.reg(t);

    // This instruction bypasses the load delay restriction: this instruction will merge the new
    // contents with the value currently being loaded if need be.
    if let Some((pending_reg, pending_value, _)) = psx.cpu.load {
        if pending_reg == t {
            cur_v = pending_value;
        }
    }

    // Next we load the *aligned* word containing the first addressed byte
    let aligned_addr = addr & !3;
    let (aligned_word, duration) = load::<u32>(psx, aligned_addr, false);

    // Depending on the address alignment we fetch the 1, 2, 3 or 4 *least* significant bytes and
    // put them in the target register.
    let v = match addr & 3 {
        0 => aligned_word,
        1 => (cur_v & 0xff00_0000) | (aligned_word >> 8),
        2 => (cur_v & 0xffff_0000) | (aligned_word >> 16),
        3 => (cur_v & 0xffff_ff00) | (aligned_word >> 24),
        _ => unreachable!(),
    };

    // Put the load in the delay slot
    psx.cpu.delayed_load_chain(t, v, duration, true);
}

/// Store Byte
fn op_sb(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);
    let v = psx.cpu.reg(t);

    psx.cpu.delayed_load();

    store(psx, addr, v as u8);
}

/// Store Halfword
fn op_sh(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);
    let v = psx.cpu.reg(t);

    psx.cpu.delayed_load();

    // Address must be 16bit aligned
    if addr % 2 == 0 {
        store(psx, addr, v as u16);
    } else {
        exception(psx, Exception::StoreAddressError);
    }
}

/// Store Word Left
fn op_swl(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);
    let v = psx.cpu.reg(t);

    let aligned_addr = addr & !3;
    // Load the current value for the aligned word at the target address
    let (cur, _) = load::<u32>(psx, aligned_addr, false);

    let new = match addr & 3 {
        0 => (cur & 0xffff_ff00) | (v >> 24),
        1 => (cur & 0xffff_0000) | (v >> 16),
        2 => (cur & 0xff00_0000) | (v >> 8),
        3 => v,
        _ => unreachable!(),
    };

    psx.cpu.delayed_load();

    store(psx, aligned_addr, new);
}

/// Store Word
fn op_sw(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);
    let v = psx.cpu.reg(t);

    psx.cpu.delayed_load();

    // Address must be 32bit aligned
    if addr % 4 == 0 {
        store(psx, addr, v);
    } else {
        exception(psx, Exception::StoreAddressError);
    }
}

/// Store Word Right
fn op_swr(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let t = reg_dep(psx, instruction.t());
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);
    let v = psx.cpu.reg(t);

    let aligned_addr = addr & !3;
    // Load the current value for the aligned word at the target address
    let (cur, _) = load::<u32>(psx, aligned_addr, false);

    let new = match addr & 3 {
        0 => v,
        1 => (cur & 0x0000_00ff) | (v << 8),
        2 => (cur & 0x0000_ffff) | (v << 16),
        3 => (cur & 0x00ff_ffff) | (v << 24),
        _ => unreachable!(),
    };

    psx.cpu.delayed_load();

    store(psx, aligned_addr, new);
}

/// Load Word in Coprocessor 0
fn op_lwc0(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered LWC0 instruction");

    // Not supported by this coprocessor
    exception(psx, Exception::CoprocessorError);
}

/// Load Word in Coprocessor 1
fn op_lwc1(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered LWC1 instruction");

    // Not supported by this coprocessor
    exception(psx, Exception::CoprocessorError);
}

/// Load Word in Coprocessor 2
fn op_lwc2(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let cop_r = instruction.t().0;
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    psx.cpu.delayed_load();

    // Address must be 32bit aligned
    if addr % 4 == 0 {
        // XXX how should we handle duration here? No absorb?
        let (v, _duration) = load::<u32>(psx, addr, true);

        psx.gte.set_data(cop_r, v);
    } else {
        exception(psx, Exception::LoadAddressError);
    }
}

/// Load Word in Coprocessor 3
fn op_lwc3(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered LWC3 instruction");

    // Not supported by this coprocessor
    exception(psx, Exception::CoprocessorError);
}

/// Store Word in Coprocessor 0
fn op_swc0(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered SWC0 instruction");

    // Not supported by this coprocessor
    exception(psx, Exception::CoprocessorError);
}

/// Store Word in Coprocessor 1
fn op_swc1(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered SWC1 instruction");

    // Not supported by this coprocessor
    exception(psx, Exception::CoprocessorError);
}

/// Store Word in Coprocessor 2
fn op_swc2(psx: &mut Psx, instruction: Instruction) {
    let i = instruction.imm_se();
    let cop_r = instruction.t().0;
    let s = reg_dep(psx, instruction.s());

    let addr = psx.cpu.reg(s).wrapping_add(i);

    // XXX read from GTE
    let v = psx.gte.data(cop_r);

    psx.cpu.delayed_load();

    // Address must be 32bit aligned
    if addr % 4 == 0 {
        store(psx, addr, v);
    } else {
        exception(psx, Exception::LoadAddressError);
    }
}

/// Store Word in Coprocessor 3
fn op_swc3(psx: &mut Psx, _: Instruction) {
    psx.cpu.delayed_load();

    warn!("Encountered SWC3 instruction");

    // Not supported by this coprocessor
    exception(psx, Exception::CoprocessorError);
}

/// Illegal instruction
fn op_illegal(psx: &mut Psx, instruction: Instruction) {
    psx.cpu.delayed_load();

    warn!(
        "Illegal instruction {} at PC 0x{:08x}!",
        instruction, psx.cpu.current_pc
    );

    exception(psx, Exception::IllegalInstruction);
}

fn op_irq(psx: &mut Psx, _instruction: Instruction) {
    psx.cpu.delayed_load();

    exception(psx, Exception::Interrupt);
}

/// A single MIPS instruction wrapper to make decoding easier
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
pub struct Instruction(u32);

impl Instruction {
    pub fn new(machine_code: u32) -> Instruction {
        Instruction(machine_code)
    }

    /// Return bits [31:26] of the instruction
    fn opcode(self) -> usize {
        let Instruction(op) = self;

        (op >> 26) as usize
    }

    /// Return bits [5:0] of the instruction
    fn function(self) -> usize {
        let Instruction(op) = self;

        (op & 0x3f) as usize
    }

    /// Return coprocessor opcode in bits [25:21]
    fn cop_opcode(self) -> u32 {
        let Instruction(op) = self;

        (op >> 21) & 0x1f
    }

    /// Return immediate value in bits [16:0]
    fn imm(self) -> u32 {
        let Instruction(op) = self;

        op & 0xffff
    }

    /// Jump target stored in bits [25:0].
    fn imm_jump(self) -> u32 {
        let Instruction(op) = self;

        // The two LSBs aren't stored since (due to alignment constraints) they're assumed to be 0.
        (op & 0x3ff_ffff) << 2
    }

    /// Return immediate value in bits [16:0] as a sign-extended 32bit
    /// value
    fn imm_se(self) -> u32 {
        let Instruction(op) = self;

        let v = (op & 0xffff) as i16;

        v as u32
    }

    /// Shift Immediate values are stored in bits [10:6]
    fn shift(self) -> u32 {
        let Instruction(op) = self;

        (op >> 6) & 0x1f
    }

    /// Return register index in bits [25:21]
    fn s(self) -> RegisterIndex {
        let Instruction(op) = self;

        RegisterIndex(((op >> 21) & 0x1f) as u8)
    }

    /// Return register index in bits [20:16]
    fn t(self) -> RegisterIndex {
        let Instruction(op) = self;

        RegisterIndex(((op >> 16) & 0x1f) as u8)
    }

    /// Return register index in bits [15:11]
    fn d(self) -> RegisterIndex {
        let Instruction(op) = self;

        RegisterIndex(((op >> 11) & 0x1f) as u8)
    }
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

/// A simple wrapper around a register index to avoid coding errors where the register index could
/// be used instead of its value
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct RegisterIndex(pub u8);

/// Handler table for the main opcodes (instruction bits [31:26])
#[rustfmt::skip]
const OPCODE_HANDLERS: [fn(&mut Psx, Instruction); 128] = [
    // 0x00
    op_function, op_bxx,      op_j,        op_jal,
    op_beq,      op_bne,      op_blez,     op_bgtz,
    op_addi,     op_addiu,    op_slti,     op_sltiu,
    op_andi,     op_ori,      op_xori,     op_lui,
    // 0x10
    op_cop0,     op_cop1,     op_cop2,     op_cop3,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    // 0x20
    op_lb,       op_lh,       op_lwl,      op_lw,
    op_lbu,      op_lhu,      op_lwr,      op_illegal,
    op_sb,       op_sh,       op_swl,      op_sw,
    op_illegal,  op_illegal,  op_swr,      op_illegal,
    // 0x30
    op_lwc0,     op_lwc1,     op_lwc2,     op_lwc3,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    op_swc0,     op_swc1,     op_swc2,     op_swc3,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,

    // This second half is called when an interrupt is active and `opcode_table_offset` is set to
    // 64. You'll notice that the interrupt code is called every time *except* for COP2 opcodes.
    // That's because GTE operations behave weirdly when at interrupt occurs. No$ says that they
    // get repeated, mednafen "cheats" and just postpone the interrupt if it was to occur on a GTE
    // instruction. Here we use mednafen's approach and ignore the interrupts for GTE operations.

    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,

    op_irq,      op_irq,      op_cop2,     op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,

    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,

    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
    op_irq,      op_irq,      op_irq,      op_irq,
];

/// Handler table for the function codes (instruction bits [31:26] when opcode is 0)
#[rustfmt::skip]
const FUNCTION_HANDLERS: [fn(&mut Psx, Instruction); 64] = [
    // 0x00
    op_sll,      op_illegal,  op_srl,      op_sra,
    op_sllv,     op_illegal,  op_srlv,     op_srav,
    op_jr,       op_jalr,     op_illegal,  op_illegal,
    op_syscall,  op_break,    op_illegal,  op_illegal,
    // 0x10
    op_mfhi,     op_mthi,     op_mflo,     op_mtlo,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    op_mult,     op_multu,    op_div,      op_divu,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    // 0x20
    op_add,      op_addu,     op_sub,      op_subu,
    op_and,      op_or,       op_xor,      op_nor,
    op_illegal,  op_illegal,  op_slt,      op_sltu,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    // 0x30
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
    op_illegal,  op_illegal,  op_illegal,  op_illegal,
];

/// Conventional names given to the MIPS registers
const REGISTER_NAMES: [&str; 32] = [
    "r0", // Hardwired to be always 0
    "at", // Assembler Temporary (reserved for the assembler)
    "v0", "v1", // First and second return values
    "a0", "a1", "a2", "a3", // First four function arguments
    "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", // Temporary registers
    "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", // Saved registers
    "t8", "t9", // Temporary registers
    "k0", "k1", // Reserved for kernel use
    "gp", // Global pointer (not normally used on the PSX)
    "sp", // Stack Pointer
    "fp", // Frame Pointer
    "ra", // Return address
];

/// Instruction cache line
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
struct ICacheLine {
    /// Tag: high 22bits of the address associated with this cacheline Valid bits: 3 bit index of
    /// the first valid word in line.
    tag_valid: u32,
    /// Four words per line
    line: [Instruction; 4],
}

impl ICacheLine {
    fn new() -> ICacheLine {
        // The cache starts in a random state. In order to catch missbehaving software we fill them
        // with "trap" values
        ICacheLine {
            // Tag is 0, all line valid
            tag_valid: 0x0,
            // BREAK opcode to catch misbehaving code
            line: [Instruction(0xbadc_0de5); 4],
        }
    }

    /// Return the cacheline's tag
    fn tag(&self) -> u32 {
        self.tag_valid & 0xffff_f000
    }

    /// Return the cacheline's first valid word
    fn valid_index(&self) -> u32 {
        // We store the valid bits in bits [4:2], this way we can just mask the PC value in
        // `set_tag_valid` without having to shuffle the bits around
        (self.tag_valid >> 2) & 0x7
    }

    /// Set the cacheline's tag and valid bits. `pc` is the first valid PC in the cacheline.
    fn set_tag_valid(&mut self, pc: u32) {
        self.tag_valid = pc & 0x7fff_f00c;
    }

    /// Invalidate the entire cacheline by pushing the index out of range. Doesn't change the tag
    /// or contents of the line.
    fn invalidate(&mut self) {
        // Setting bit 4 means that the value returned by valid_index will be in the range [4, 7]
        // which is outside the valid cacheline index range [0, 3].
        self.tag_valid |= 0x10;
    }

    fn instruction(&self, index: u32) -> Instruction {
        self.line[index as usize]
    }

    fn set_instruction(&mut self, index: u32, instruction: Instruction) {
        self.line[index as usize] = instruction;
    }
}

/// Multiplication timings, based on the number of leading zeroes in the first multiplicand.
///
/// XXX I don't know how accurate these timings are. Is it really only dependent on the magnitude
/// of the first multiplicand?
const MULT_TIMINGS: [u8; 33] = [
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 10, 10, 10, 10, 10, 10, 10, 10, 10, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7,
];

#[test]
fn validate_mult_timings() {
    for i in 0..33 {
        let penalty = if i < 12 {
            7
        } else if i < 21 {
            3
        } else {
            0
        };

        assert_eq!(penalty + 7, MULT_TIMINGS[i]);
    }
}

/// The PSX CPU is supposed to run at 33.868Mhz. This frequency is exactly 0x300 times the CD
/// sample rate frequency of 44.1kHz so that the SPU can run synchronously.
pub const CPU_FREQ_HZ: CycleCount = 33_868_800;
