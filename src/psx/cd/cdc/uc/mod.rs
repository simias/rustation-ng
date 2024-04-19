//! NXP MC68HC05 microcontroller emulation

mod instructions;

use super::Cdc;

/// MC68HC05 microcontroller
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Uc {
    /// The full 64KiB address map, used for fetching instructions
    #[serde(with = "serialize_memory")]
    memory: [u8; 0x1_0000],
    pc: u16,
    /// Stack pointer
    sp: u16,
    /// Accumulator
    a: u8,
    /// Index
    x: u8,
    /// Half carry flag
    half_carry: bool,
    /// Interrupt flag
    irq_disable: bool,
    /// Negative flag
    negative: bool,
    /// Zero flag
    zero: bool,
    /// Carry/Borrow flag
    carry: bool,
    /// I/O ports A, B, C, D and E
    io_ports: [IoPort; 5],
    /// Interrupt control register
    intcr: IntCr,
    /// Interrupt status register
    intsr: IntSr,
    timebase_ctrl1: TimebaseCtrl1,
    timebase_ctrl2: TimebaseCtrl2,
    timer1_ctrl: Timer1Ctrl,
    timer2_ctrl: Timer2Ctrl,
    timer2_status: Timer2Status,
    timer2_counter: u32,
    /// Serial peripheral control register (SPCR)
    spi_spcr: SspiCtrl,
    /// Serial peripheral status register (SPSR)
    spi_spsr: SspiStat,
    /// Serial Peripheral data register (SPDR)
    spi_spdr: u8,
    /// SSPI transfer state machine: None if no transfer is taking place, otherwise contains the
    /// position of the next bit to be received (from 0 to 7).
    spi_transfer: Option<u8>,
    /// Counter to the next SPI event. When the SPI is idle this is set to a very large value.
    spi_next_event: i32,
    /// Output compare Register 2. The register is shadowed and transferred to the "real" OC2
    /// register only in certain situations (see 9.3.3 p.87 in the datasheet)
    oc2: u8,
    timer2_target: u8,
    /// Pre-computed target in number of 2MHz clock cycles
    timer2_target_cycles: u32,
    /// Value of the misc register (0x003e)
    misc: Misc,
    /// When true we debug the full controller state between every instruction
    debug: bool,
    /// We keep running until the cycle budget reaches 0
    cycle_budget: i32,
}

impl Uc {
    pub fn new(rom: &[u8; ROM_DUMP_SIZE]) -> Uc {
        let mut port_b_input = 0;

        // Pins 0 and 6 are left floating but since the pull-up should be active on port B they
        // read 1.
        port_b_input |= 1 | (1 << 6);

        // If pin 5 is low we boot in test mode, let's not do that
        //
        // XXX on the 5502 this seems to be something else, but it's also not connected
        port_b_input |= 1 << 5;

        let mut uc = Uc {
            // 0x90 is an invalid instruction, this way we'll easily catch executions from a wrong
            // location in memory
            memory: [0x90; 0x1_0000],
            pc: 0,
            // The top 10 bits of the stack pointer are fixed
            sp: 0x00ff,
            a: 0,
            x: 0,
            half_carry: false,
            irq_disable: true,
            negative: false,
            zero: false,
            carry: false,
            io_ports: [
                IoPort::new(0),
                IoPort::new(port_b_input),
                IoPort::new(0xff),
                IoPort::new(0),
                IoPort::new(0),
            ],
            intcr: IntCr::new(),
            intsr: IntSr::new(),
            timebase_ctrl1: TimebaseCtrl1::new(),
            timebase_ctrl2: TimebaseCtrl2::new(),
            timer1_ctrl: Timer1Ctrl::new(),
            timer2_ctrl: Timer2Ctrl::new(),
            timer2_status: Timer2Status::new(),
            timer2_counter: 0x01 << 5,
            spi_spcr: SspiCtrl::new(),
            spi_spsr: SspiStat::new(),
            spi_spdr: 0,
            spi_transfer: None,
            spi_next_event: i32::MAX,
            oc2: 0,
            timer2_target: 0,
            timer2_target_cycles: 0xffff_ffff,
            misc: Misc::new(),
            debug: false,
            cycle_budget: 0,
        };

        // Port D and E are output-only
        uc.io_ports[3].set_dir(0xff);
        uc.io_ports[4].set_dir(0xff);
        // Port D and E output full ones on reset
        uc.io_ports[3].set_output(0xff);
        uc.io_ports[4].set_output(0xff);

        // On startup RAM appears to be mostly 0xff (judging by the bottom of the stack) although
        // sometimes I get a clear bit here and there. The firmware seems to clear the
        // RAM (except for the stack region) on startup anyway.
        for addr in map::RAM.iter() {
            uc.memory[addr] = 0xff;
        }

        uc.load_rom(rom);

        // Load reset user vector address into the PC
        uc.pc = uc.vector_address(IrqVector::Reset);

        uc
    }

    /// Return the value of the STAT byte from RAM. Of course this is firmware-dependant.
    fn get_stat(&self) -> u8 {
        self.memory[0xbc]
    }

    pub fn is_playing_audio(&self) -> bool {
        self.get_stat() & (1 << 7) != 0
    }

    pub fn is_seeking(&self) -> bool {
        self.get_stat() & (1 << 6) != 0
    }

    pub fn is_reading_data(&self) -> bool {
        self.get_stat() & (1 << 5) != 0
    }

    pub fn set_shell_open(&mut self, opened: bool) {
        // Pin 3 is "DOOR", set to 1 when the door is opened
        if opened {
            self.io_ports[1].input |= 1 << 3;
        } else {
            self.io_ports[1].input &= !(1 << 3);
        }
    }

    pub fn is_shell_open(&self) -> bool {
        (self.io_ports[1].input & (1 << 3)) != 0
    }

    /// Set the state of the lmsw input that's connected to a switch at the inner limit of the CD
    /// sled. The input is low when the sled is all the way to the start of the disc, otherwise
    /// it's high.
    ///
    /// It's used by the firmware to reset the sled before attempting the focus sequence.
    pub fn set_lmsw(&mut self, set: bool) {
        if set {
            self.io_ports[1].input |= 1 << 2;
        } else {
            self.io_ports[1].input &= !(1 << 2);
        }
    }

    pub fn set_debug(&mut self, debug: bool) {
        self.debug = debug;
    }

    pub fn is_laser_on(&self) -> bool {
        // LED off is port D bit 7
        let port_d = self.io_ports[3];

        port_d.load() & 0x80 == 0
    }

    /// Load the MASK ROM, SELF-CHECK ROM, TEST VECTORS and USER VECTORS from `rom`. This is the
    /// format used by No$'s dumps
    fn load_rom(&mut self, rom: &[u8; ROM_DUMP_SIZE]) {
        let mut i = 0usize;

        for addr in map::MASK_ROM.iter() {
            self.memory[addr] = rom[i];
            i += 1;
        }

        for addr in map::SELF_CHECK_ROM.iter() {
            self.memory[addr] = rom[i];
            i += 1;
        }

        for addr in map::TEST_VECTORS.iter() {
            self.memory[addr] = rom[i];
            i += 1;
        }

        for addr in map::USER_VECTORS.iter() {
            self.memory[addr] = rom[i];
            i += 1;
        }

        debug_assert!(i == ROM_DUMP_SIZE);
    }

    /// Copy the ROM from another Uc instance
    pub fn copy_rom(&mut self, source: &Uc) {
        for addr in map::MASK_ROM.iter() {
            self.memory[addr] = source.memory[addr];
        }

        for addr in map::SELF_CHECK_ROM.iter() {
            self.memory[addr] = source.memory[addr];
        }

        for addr in map::TEST_VECTORS.iter() {
            self.memory[addr] = source.memory[addr];
        }

        for addr in map::USER_VECTORS.iter() {
            self.memory[addr] = source.memory[addr];
        }
    }

    fn vector_address(&self, vector: IrqVector) -> u16 {
        let addr = map::USER_VECTORS.start() + vector as usize;
        let hi = self.memory[addr] as u16;
        let lo = self.memory[addr + 1] as u16;

        lo | (hi << 8)
    }

    fn read_mem(&self, idx: u16) -> u8 {
        self.memory[idx as usize]
    }

    /// Return the value at *PC and increment PC
    fn pc_next(&mut self) -> u8 {
        let v = self.read_mem(self.pc);

        self.pc = self.pc.wrapping_add(1);

        // Make sure we don't leave the ROM
        debug_assert!(self.pc >= 0x1000 && self.pc < 0x5000);

        v
    }

    fn push(&mut self, v: u8) {
        self.memory[self.sp as usize] = v;
        self.sp = self.sp.wrapping_sub(1);

        // Only the 6 LSBs are writeable
        self.sp &= 0x00ff;
        self.sp |= 0x00c0;
    }

    fn pop(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);

        // Only the 6 LSBs are writeable
        self.sp &= 0x00ff;
        self.sp |= 0x00c0;

        self.memory[self.sp as usize]
    }

    fn push16(&mut self, v: u16) {
        self.push(v as u8);
        self.push((v >> 8) as u8);
    }

    fn pop16(&mut self) -> u16 {
        let hi = self.pop() as u16;
        let lo = self.pop() as u16;

        lo | (hi << 8)
    }

    fn push_pc(&mut self) {
        self.push16(self.pc);
    }

    fn pop_pc(&mut self) {
        self.pc = self.pop16();
    }

    fn push_ccr(&mut self) {
        let mut ccr = 0xe0;

        ccr |= self.carry as u8;
        ccr |= (self.zero as u8) << 1;
        ccr |= (self.negative as u8) << 2;
        ccr |= (self.irq_disable as u8) << 3;
        ccr |= (self.half_carry as u8) << 4;

        self.push(ccr);
    }

    fn pop_ccr(&mut self) {
        let ccr = self.pop();

        self.carry = ccr & 1 != 0;
        self.zero = ccr & (1 << 1) != 0;
        self.negative = ccr & (1 << 2) != 0;
        self.irq_disable = ccr & (1 << 3) != 0;
        self.half_carry = ccr & (1 << 4) != 0;
    }

    fn maybe_interrupt(&mut self) -> bool {
        if self.irq_disable {
            // Master IRQ disabled
            return false;
        }

        // The PSX software only uses Timer2's IRQ, no need to worry about the rest
        let timer_irq_pending = self.timer2_status.oc2f() && self.timer2_ctrl.oc2ie();

        if timer_irq_pending {
            self.interrupt(IrqVector::Timer2);
            true
        } else {
            false
        }
    }

    fn interrupt(&mut self, vector: IrqVector) {
        // Push current context
        self.push_pc();
        self.push(self.x);
        self.push(self.a);
        self.push_ccr();

        // Disable interrupts
        self.irq_disable = true;

        // Jump to vector
        self.pc = self.vector_address(vector);
    }

    fn jump(&mut self, target: u16) {
        self.pc = target;
    }

    /// Called by the decoder to set the level of the XINT signal. In practice the SCPH-1002's
    /// firmware doesn't appear to use this signal (it polls the decoder's interrupt register
    /// instead) but it doesn't hurt to emulate it correctly.
    pub fn set_decoder_xint(&mut self, pin_level: bool) {
        // Connected to port C pin 6
        let old_lvl = self.io_ports[2].load() & (1 << 6) != 0;

        self.io_ports[2].input &= !(1 << 6);
        self.io_ports[2].input |= (pin_level as u8) << 6;

        if old_lvl && !pin_level {
            // Falling edge, set IRQ2
            self.intsr.trigger_irq2();
        }
    }

    /// Called by the DSP to set the value of the SENS input
    pub fn set_dsp_sens(&mut self, sens: bool) {
        // Connected to port B pin 7
        self.io_ports[1].input &= !(1 << 7);
        self.io_ports[1].input |= (sens as u8) << 7;
    }

    /// Called by the DSP to set the value of the SCOR input
    pub fn set_dsp_scor(&mut self, scor: bool) {
        let port_c = &mut self.io_ports[2];

        // Connected to port C pin 7
        let prev_scor = port_c.load() & (1 << 7) != 0;
        port_c.input |= (scor as u8) << 7;

        // This pin is also IRQ1, triggered on falling edge.
        if prev_scor && !scor {
            self.intsr.trigger_irq1();
        }
    }

    /// Called by the DSP to set the value of the SCEx input
    pub fn set_dsp_scex(&mut self, scex: bool) {
        // Connected to port B pin 1
        self.io_ports[1].input &= !(1 << 1);
        self.io_ports[1].input |= (scex as u8) << 1;
    }

    /// Called by the DSP to set the value of the SQSO (SUBQ/SOCT) input
    pub fn set_dsp_subq(&mut self, subq: bool) {
        // XXX The datasheet says that when the SPI hardware interface (SSPI) is enabled, the GPIOs
        // in port C are disabled, however the firmware checks port C bit 0 *even* when the SPI is
        // enabled and skips the read if it's not 1. Does it mean that reading the port still
        // returns the extern input value? Or does it mean that in this mode we always read 1? But
        // then why test it, is it a bug?
        //
        // For now I'm going to assume that it means that the input is actually still active even
        // when the pad is controlled by the SSPI
        self.io_ports[2].input &= !1;
        self.io_ports[2].input |= subq as u8;
    }

    /// True if the SPI (SSPI) IP is enabled. If true port C bits 0, 1 and 2 aren't usable as GPIO
    pub fn is_spi_enabled(&self) -> bool {
        self.spi_spcr.is_enabled()
    }
}

impl ::std::fmt::Debug for Uc {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        writeln!(f, "Registers:")?;
        writeln!(
            f,
            "  PC: 0x{:04x} [0x{:02x} 0x{:02x} 0x{:02x}]",
            self.pc,
            self.read_mem(self.pc),
            self.read_mem(self.pc + 1),
            self.read_mem(self.pc + 2)
        )?;
        writeln!(
            f,
            "  SP: 0x{:04x} [0x{:02x} 0x{:02x} 0x{:02x}]",
            self.sp,
            self.read_mem(self.sp),
            self.read_mem(self.sp + 1),
            self.read_mem(self.sp + 2)
        )?;
        writeln!(f, "  A: 0x{:02x}  X: 0x{:02x}", self.a, self.x)?;
        writeln!(
            f,
            "  Flags: {} {} {} {} {}",
            if self.half_carry { 'H' } else { '-' },
            if self.irq_disable { 'I' } else { '-' },
            if self.negative { 'N' } else { '-' },
            if self.zero { 'Z' } else { '-' },
            if self.carry { 'C' } else { '-' },
        )?;

        // Dump RAM
        writeln!(f, "RAM dump:")?;

        write!(f, "       ")?;
        for j in 0..32 {
            if j % 8 == 0 {
                write!(f, " ")?;
            }
            if j % 16 == 0 {
                write!(f, " ")?;
            }
            write!(f, " {:2x}", j)?;
        }
        writeln!(f)?;

        for i in 0..16 {
            write!(f, "  {:04x}:", 0x40 + i * 32)?;
            for j in 0..32 {
                if j % 8 == 0 {
                    write!(f, " ")?;
                }
                if j % 16 == 0 {
                    write!(f, " ")?;
                }
                let v = self.read_mem(0x40 + i * 32 + j);
                if v > 0 {
                    write!(f, " {:02x}", v)?;
                } else {
                    write!(f, " ..")?;
                }
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

/// Called at 44.1kHz
pub fn run_audio_cycle(cdc: &mut Cdc) {
    let uc = &mut cdc.uc;

    uc.cycle_budget += UC_CYCLES_PER_AUDIO_CYCLE as i32;

    run_uc(cdc);
}

/// Run UC until `cycle_budget` reaches 0
fn run_uc(cdc: &mut Cdc) {
    while cdc.uc.cycle_budget > 0 {
        let instruction_cycles = run_next_instruction(cdc);

        cdc.uc.timer2_counter += instruction_cycles as u32;
        cdc.uc.cycle_budget -= instruction_cycles as i32;

        // Advance SPI state machine
        cdc.uc.spi_next_event -= instruction_cycles as i32;

        // Loop in case we have missed a full bit period and we have to handle several bits in a
        // single call (16 cycles).
        while cdc.uc.spi_next_event <= 0 {
            sspi_event(cdc);
        }

        if cdc.uc.timer2_counter >= cdc.uc.timer2_target_cycles {
            timer2_match(cdc);
        }
    }
}

/// Returns the number of cycles elapsed to run this instruction
fn run_next_instruction(cdc: &mut Cdc) -> u8 {
    if cfg!(feature = "cdc_verbose") {
        let breakpoint: &[u16] = &[
            0x4d09, 0x4d12, 0x2606, 0x1cc1, 0x21ce, 0x1cfb, 0x1d23, 0x23a5, 0x21fa, 0x1dc7, 0x1e19,
            0x2753, 0x4526, 0x2937, 0x1248, 0x4a92, 0x38ef, 0x2bda, 0x3b3f, 0x2fea, 0x14fb, 0x191f,
            0x2adb, 0x3055, 0x3079, 0x3079, 0x30fa, 0x31b4, 0x3202, 0x3294, 0x32bf, 0x2900, 0x33df,
            0x33df, 0x3876, 0x387c, 0x1e8c, 0x1e8f, 0x1e92, 0x1e95, 0x1e9b, 0x1ea7, 0x1eb6, 0x1eb9,
            0x1ecc, 0x1ef0, 0x20ad, 0x1f7d, 0x2951, 0x24be, 0x472d, 0x4c86, 0x4c57, 0x26ab, 0x1f1f,
            0x1f16, 0x1f22, 0x1711, 0x1548, 0x15a3, 0x160f, 0x1e09, 0x16e5, 0x1707, 0x28de, 0x181d,
            0x1846, 0x142b, 0x1f6e, 0x1f7d, 0x1fa6, 0x12ff, 0x42a5, 0x1feb, 0x1944, 0x2690, 0x3bca,
            0x13b0, 0x140d, 0x2cab, 0x3425, 0x3bbd, 0x340d, 0x3413, 0x3416, 0x3419, 0x341c, 0x341f,
            0x3422, 0x3525, 0x3528, 0x352b, 0x352e, 0x38f9, 0x3c2a, 0x3c0c, 0x3ec8,
        ];

        for &a in breakpoint {
            if cdc.uc.pc == a {
                cdc.set_debug(true);
            }
        }

        if cdc.uc.debug {
            println!("{:?}", cdc.uc);
        }
    }

    let op = cdc.uc.pc_next();

    let instruction = &instructions::OPCODE_MAP[op as usize];

    (instruction.handler)(cdc);

    debug_assert!(instruction.cycles != 0);

    instruction.cycles
}

fn maybe_interrupt(cdc: &mut Cdc) {
    if cdc.uc.maybe_interrupt() {
        // We have been interrupted.
        // XXX I don't know how long this should normally take, can't find the info in the docs.
        // RTI takes 10 cycles, so assuming at least 2 cycles for the normal instruction fetch +
        // decode we can estimate the delay for the switch at about 8 cycles. No idea if it's even
        // remotely accurate though
        cdc.uc.cycle_budget -= 8;
    }
}

fn timer2_match(cdc: &mut Cdc) {
    debug_assert!(cdc.uc.timer2_counter >= cdc.uc.timer2_target_cycles);

    if cdc.uc.timer2_target != 0 {
        // Set match IRQ
        cdc.uc.timer2_status.set_oc2f();
        maybe_interrupt(cdc);
    }

    // Counter is reset to 1, not 0. We also preserve the fractional part of the counter to remain
    // cycle-accurate.
    cdc.uc.timer2_counter = (1 << 5) | (cdc.uc.timer2_counter & 0x1f);
}

/// Called when the Timer2 target changed
fn update_timer2_target(cdc: &mut Cdc) {
    // For now we only support SysClk / 32 clock source since it's the only one that seems to be
    // used in practice.
    debug_assert!(cdc.uc.timer2_target == 0 || cdc.uc.timebase_ctrl1.0 & 3 == 2);

    let target = cdc.uc.timer2_target.wrapping_sub(1) as u32;

    cdc.uc.timer2_target_cycles = target << 5;
}

fn load(cdc: &mut Cdc, addr: u16) -> u8 {
    if map::RAM.contains(addr).is_some() {
        return cdc.uc.memory[addr as usize];
    }

    if addr <= 0x000f {
        if cdc.uc.misc.optm() {
            return load_io_port_option(cdc, addr);
        } else {
            return load_io_port_value(cdc, addr);
        }
    }

    if map::MASK_ROM.contains(addr).is_some() {
        return cdc.uc.memory[addr as usize];
    }

    match addr {
        0x10 => cdc.uc.timebase_ctrl1.0,
        0x11 => cdc.uc.timebase_ctrl2.get(),
        0x12 => cdc.uc.timer1_ctrl.0,
        0x1c => cdc.uc.timer2_ctrl.0,
        0x1d => cdc.uc.timer2_status.0,
        0x1e => cdc.uc.oc2,
        0x1f => (cdc.uc.timer2_counter >> 5) as u8,
        0x3e => cdc.uc.misc.0,
        _ => unimplemented!("Load from 0x{:04x}", addr),
    }
}

fn store(cdc: &mut Cdc, addr: u16, val: u8) {
    if map::RAM.contains(addr).is_some() {
        cdc.uc.memory[addr as usize] = val;
        return;
    }

    if addr <= 0x000f {
        if cdc.uc.misc.optm() {
            store_io_port_option(cdc, addr, val);
        } else {
            store_io_port_value(cdc, addr, val);
        }
        return;
    }

    match addr {
        0x10 => cdc.uc.timebase_ctrl1.set(val),
        0x11 => cdc.uc.timebase_ctrl2.set(val),
        0x12 => cdc.uc.timer1_ctrl.set(val),
        0x1c => {
            cdc.uc.timer2_ctrl.set(val);
            // See if the IRQ has been enabled
            maybe_interrupt(cdc);
        }
        0x1d => {
            cdc.uc.timer2_status.set(val);
        }
        0x1e => cdc.uc.oc2 = val,
        0x1f => {
            cdc.uc.timer2_counter = (val as u32) << 5;
            // Writing to this register moves OC2 to the active timer target
            cdc.uc.timer2_target = cdc.uc.oc2;
            update_timer2_target(cdc);
        }
        // LCD control register: LCD disabled, Ports D and E used as normal GPIO
        0x20 => assert!(val & 0xee == 0x2e, "Unexpected LCDCR value"),
        0x3e => cdc.uc.misc.set(val),
        _ => unimplemented!("Store 0x{:02x} to 0x{:04x}", val, addr),
    }
}

fn store_io_port_option(cdc: &mut Cdc, addr: u16, val: u8) {
    match addr {
        // For ports A and C we configure the direction
        0x00 | 0x02 => cdc.uc.io_ports[addr as usize].set_dir(val),
        // Port B is always input, this register is read only
        0x01 => (),
        0x03 => {
            // Port D is always output. The high nibble configures the pin mux for LCD driver
            // output (when the bits are 0) which shouldn't be used on the PSX
            if val & 0xf0 != 0xf0 {
                unimplemented!("Port D configured with LCD pins!");
            }
        }
        0x04 => {
            // Like Port D, output only but this register can be used to configure the LCD driver
            if val != 0xff {
                unimplemented!("Port E configured with LCD pins!");
            }
        }
        // Resistor control register 1, normally pull-ups are enabled on ports A and B
        0x08 => {
            assert!(val & 0xf == 0xf, "Unexpected RCR1 value");
        }
        // Resistor control register 2, normally pull-up is enabled on Port C bit 6
        0x09 => {
            assert!(val == 0x40, "Unexpected RCR2 value");
        }
        // Open-Drain Output Control Register 1: normally no port is configured in open drain
        0x0a => assert!(val == 0, "Unexpected WOM1 value"),
        // Open-Drain Output Control Register 2: normally no port is configured in open drain
        0x0b => assert!(val == 0, "Unexpected WOM2 value"),
        // Key Wakeup Input Enable: disabled
        0x0e => assert!(val == 0, "Unexpected KWIEN value"),
        _ => unimplemented!("IO Option store {:x} @ {:04x}", val, addr),
    }
}

fn load_io_port_option(cdc: &mut Cdc, addr: u16) -> u8 {
    let _ = cdc;
    let _ = addr;
    unimplemented!();
}

fn store_io_port_value(cdc: &mut Cdc, addr: u16, val: u8) {
    match addr {
        0x00 | 0x01 | 0x04 => cdc.uc.io_ports[addr as usize].set_output(val),
        0x02 => {
            // Port C store
            let prev_c = cdc.uc.io_ports[2].load();

            cdc.uc.io_ports[addr as usize].set_output(val);

            let new_c = cdc.uc.io_ports[2].load();

            let speed = new_c & (1 << 3) != 0;
            if (prev_c & (1 << 3) != 0) != speed {
                cdc_debug!("SPEED {}", speed);
            }

            // XXX I don't remember why I named this bit "mirror", I can't find it documented
            // anywhere. Maybe I just tracked it on the PCB?
            let mirror = new_c & (1 << 4) != 0;
            if (prev_c & (1 << 4) != 0) != mirror {
                cdc_debug!("mirror {}", mirror);
            }

            // Check for SCLK pulse (SENS value changes after falling edge)
            if (prev_c & (1 << 5) != 0) && (new_c & (1 << 5) == 0) {
                cdc.dsp_sclk_tick();
            }

            if !cdc.uc.is_spi_enabled() {
                // Hardware SPI is disabled, check for bitbanging on SQCK
                if (prev_c & (1 << 2) != 0) && (new_c & (1 << 2) == 0) {
                    // This will update the state of the SQSO in Port C bit 0 (see `set_dsp_subq`)
                    cdc.dsp_sqck_tick();
                }
            }
        }
        0x03 => {
            // Port D store
            let prev_d = cdc.uc.io_ports[3].load();

            // Port D LSB is always 1
            cdc.uc.io_ports[3].set_output(val | 1);

            let new_d = cdc.uc.io_ports[3].load();

            // Check if the chip select of the decoder is asserted (active low)
            if new_d & (1 << 4) == 0 {
                // Yes, check for a rising edge of the read or write strobe signals.

                // We latch writes on the *rising* edge of the WR signal
                let is_write = (prev_d & (1 << 5) == 0) && (new_d & (1 << 5) != 0);
                // We send read data on the *falling* edge of the RD signal
                let is_read = (prev_d & (1 << 6) != 0) && (new_d & (1 << 6) == 0);

                if is_write && is_read {
                    unimplemented!("CXD1815Q simultaneous read and write");
                }

                let port_a = cdc.uc.io_ports[0];
                let port_e = cdc.uc.io_ports[4];

                // Index in port E
                let addr = port_e.load() & 0x1f;
                // Value in port A
                let val = port_a.load();

                if is_write {
                    cdc.decoder_write(addr, val);
                } else if is_read {
                    let v = cdc.decoder_read(addr);
                    cdc.uc.io_ports[0].input = v;
                }
            }

            // Check if we have a rising edge for the clock of the DSP serial link
            if (prev_d & (1 << 3) == 0) && (new_d & (1 << 3) != 0) {
                let b = (new_d & (1 << 1)) != 0;

                cdc.dsp_serial_tick(b)
            }

            // Check if the DSP data is latched (falling edge of XLAT)
            if (prev_d & (1 << 2) != 0) && (new_d & (1 << 2) == 0) {
                cdc.dsp_serial_latch();
            }

            let laser_off = new_d & (1 << 7) != 0;
            if (prev_d & (1 << 7) != 0) != laser_off {
                cdc_debug!("laser_off: {}", laser_off);
            }
        }
        0x08 => cdc.uc.intcr.set(val),
        0x09 => cdc.uc.intsr.set(val),
        // Serial peripheral control register: normally 0x71:
        // * Internal clock / 16
        // * Master mode
        // * LSB first
        // * Enable
        // * No interrupt
        0x0a => cdc.uc.spi_spcr.set(val),
        // SSPI data register (SPDR)
        //
        // Writing to this register triggers an SPI transfer on the SQCK/SQSO interface to the DSP
        // (to read SUBQ and SOCT data). The transfer is only used to *read* data, the MOSI pin is
        // not connected to anything, so the actual value written to this register doesn't matter
        // since it's sent nowhere.
        0x0c => sspi_transfer_start(cdc),
        _ => unimplemented!("IO value store {:x} @ {:04x}", val, addr),
    }
}

fn load_io_port_value(cdc: &mut Cdc, addr: u16) -> u8 {
    match addr {
        0x00..=0x04 => cdc.uc.io_ports[addr as usize].load(),
        0x08 => cdc.uc.intcr.0,
        0x09 => cdc.uc.intsr.0,
        0x0b => cdc.uc.spi_spsr.0,
        // SSPI data register
        0x0c => {
            if cdc.uc.spi_transfer.is_some() {
                // This should set DCOL according to the datasheet
                unimplemented!("SSPI transfer read collision");
            }

            // According to the datasheet reading this register should only clear the status if
            // the SPSR had been read before (i.e. the clear sequence is "accessing the SPSR while
            // the SPIF bit is set" followed by "accessing the SPDR").
            //
            // In practice it shouldn't really make a difference
            cdc.uc.spi_spsr.clear();

            cdc.uc.spi_spdr
        }
        _ => unimplemented!("IO value load @ {:04x}", addr),
    }
}

/// Called when a new transfer should start on the serial interface connected to the DSP SOCK/SQSO
/// interface (used to read SUBQ/SOCT)
fn sspi_transfer_start(cdc: &mut Cdc) {
    let uc = &mut cdc.uc;

    if uc.spi_transfer.is_some() {
        // This should set DCOL according to the datasheet
        unimplemented!("SSPI transfer start collision");
    }

    uc.spi_spsr.clear();

    // Validate that we are using the expected config: no IRQ, LSB-first, master mode, sysclk / 16
    debug_assert!(!uc.spi_spcr.spie(), "Unimplemented SSPI IRQ");
    debug_assert!(
        uc.spi_spcr.lsb_first(),
        "Unimplemented SSPI MSB-first transfer"
    );
    debug_assert!(uc.spi_spcr.master_mode(), "Unimplemented SSPI slave mode");
    debug_assert!(
        uc.spi_spcr.clock_rate_sysctk_16(),
        "Unimplemented SSPI clock ratio"
    );

    // Start transfer
    uc.spi_transfer = Some(0);
    // We'll receive the first bit in 16 cycles
    uc.spi_next_event = 16;
    // Clear result
    uc.spi_spdr = 0;
}

/// Called when `spi_next_event` reached 0
fn sspi_event(cdc: &mut Cdc) {
    let bit_pos = match cdc.uc.spi_transfer {
        Some(p) => p,
        None => {
            // No transfer is taking place
            cdc.uc.spi_next_event = i32::MAX;
            return;
        }
    };

    // This will update the state of the SQSO in Port C bit 0 (see `set_dsp_subq`)
    cdc.dsp_sqck_tick();

    // Retrieve value of the external MISO line and update the SSPI data register
    //
    // I don't actually know if on the real hardware the value is available for the software to
    // read during transfer or if it's only committed once the transfer is done. The datasheet says
    // that the access to the register is "inhibited" when a transmission is taking place, but I
    // don't know what that means exactly. In practice that shouldn't even happen with the existing
    // firmware anyway.
    let miso = cdc.uc.io_ports[2].input & 1;
    cdc.uc.spi_spdr |= miso << bit_pos;

    if bit_pos < 7 {
        // We have more bits to receive
        cdc.uc.spi_transfer = Some(bit_pos + 1);

        // Next bit comes 16 cycles later. We add instead of merely setting the value because we
        // only check for SPI events in-between instructions, so we might have overshot a bit and
        // ended up with a negative `spi_next_event`.
        cdc.uc.spi_next_event += 16;
    } else {
        // Transfer done
        cdc.uc.spi_transfer = None;
        cdc.uc.spi_next_event = i32::MAX;

        // Flag end of transfer
        cdc.uc.spi_spsr.set_spif();
    }
}

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Eq)]
enum IrqVector {
    TimeBase = 0,
    /// Serial peripheral interface
    Sspi = 2,
    Timer2 = 4,
    Timer1 = 6,
    /// Key Wakeup
    Kwi = 8,
    /// IRQ1 and 2 external interrupts
    Irq = 10,
    /// Software interrupt, can't be masked
    Swi = 12,
    /// Reset, can't be masked
    Reset = 14,
}

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct IoPort {
    /// Current input state
    input: u8,
    /// Latched output value
    output: u8,
    /// Direction of each signal: 1 for out, 0 for in
    dir: u8,
}

impl IoPort {
    /// Create a new port with all values to 0 and ports configured as full RX
    fn new(input: u8) -> IoPort {
        IoPort {
            input,
            output: 0,
            dir: 0,
        }
    }

    fn load(self) -> u8 {
        (self.output & self.dir) | (self.input & !self.dir)
    }

    fn set_output(&mut self, output: u8) {
        self.output = output;
    }

    fn set_dir(&mut self, dir: u8) {
        self.dir = dir;
    }
}

/// Interrupt control register
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct IntCr(u8);

impl IntCr {
    fn new() -> IntCr {
        IntCr(0)
    }

    fn set(&mut self, v: u8) {
        self.0 = v & 0xdc;

        if self.0 & 0xd0 != 0 {
            // We don't implement IRQ1, IRQ2 or KWI
            unimplemented!("Unsupported IRQ control: {:x}", v);
        }
    }
}

/// Interrupt status register
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct IntSr(u8);

impl IntSr {
    fn new() -> IntSr {
        IntSr(0)
    }

    fn set(&mut self, v: u8) {
        // Low nibble acks the high nibble
        self.0 &= !(v << 4)
    }

    fn trigger_irq1(&mut self) {
        self.0 |= 1 << 7;
    }

    fn trigger_irq2(&mut self) {
        self.0 |= 1 << 6;
    }
}

/// Timebase Control Register 1
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct TimebaseCtrl1(u8);

impl TimebaseCtrl1 {
    fn new() -> TimebaseCtrl1 {
        TimebaseCtrl1(0)
    }

    fn set(&mut self, v: u8) {
        // Normally Timer 2 should run SysClk / 32
        assert!(v & 3 == 2, "Unsupported Timer2 divider");

        // XXX Bit 8 (TBCLK) can be written *only once* post-reset
        self.0 = v & 0xa3;
    }
}

/// Timebase Control Register 2
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct TimebaseCtrl2(u8);

impl TimebaseCtrl2 {
    fn new() -> TimebaseCtrl2 {
        TimebaseCtrl2(0)
    }

    fn set(&mut self, v: u8) {
        // The interrupt flag is reset by writing to bit
        let irq_flag = self.irq_flag();

        self.0 = v & 0x7f;

        if irq_flag && !self.rtbif() {
            // IRQ flag is still active
            self.0 |= 0x80;
        }

        if self.tbie() {
            unimplemented!("Timebase IRQ activated");
        }
    }

    fn get(self) -> u8 {
        // Low nibble is write-only
        self.0 & 0xf0
    }

    /// Reset TBS Interrupt flag
    fn rtbif(self) -> bool {
        self.0 & (1 << 3) != 0
    }

    /// Timebase IRQ enable
    fn tbie(self) -> bool {
        self.0 & (1 << 6) != 0
    }

    fn irq_flag(self) -> bool {
        self.0 & 0x80 != 0
    }
}

/// Timer1 control
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct Timer1Ctrl(u8);

impl Timer1Ctrl {
    fn new() -> Timer1Ctrl {
        // Bit 1 keeps its value across resets
        Timer1Ctrl(0)
    }

    fn set(&mut self, v: u8) {
        self.0 = v & 0xe2;
        if self.0 & 0xe0 != 0 {
            unimplemented!("Enable timer IRQ");
        }
    }
}

/// Timer2 control
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct Timer2Ctrl(u8);

impl Timer2Ctrl {
    fn new() -> Timer2Ctrl {
        Timer2Ctrl(0)
    }

    fn set(&mut self, v: u8) {
        // Normally the firmware only enables the interrupt and doesn't change anything else
        assert!(v & 0xdf == 0x40, "Unsupported TR2");
        self.0 = v & 0xdf;
    }

    /// True if the OC2 match IRQ is enabled
    fn oc2ie(self) -> bool {
        self.0 & (1 << 6) != 0
    }
}

/// Timer2 status
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct Timer2Status(u8);

impl Timer2Status {
    fn new() -> Timer2Status {
        Timer2Status(0)
    }

    fn set(&mut self, v: u8) {
        // Low nibble acks the high nibble
        self.0 &= !(v << 4)
    }

    fn set_oc2f(&mut self) {
        self.0 |= 1 << 6;
    }

    /// True if the OC2 matched the counter and an IRQ is flagged
    fn oc2f(self) -> bool {
        self.0 & (1 << 6) != 0
    }
}

/// Serial peripheral control register (SPCR)
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct SspiCtrl(u8);

impl SspiCtrl {
    fn new() -> SspiCtrl {
        SspiCtrl(0)
    }

    fn set(&mut self, v: u8) {
        self.0 = v
    }

    /// Interrupt enable
    fn spie(self) -> bool {
        self.0 & (1 << 7) != 0
    }

    fn is_enabled(self) -> bool {
        self.0 & (1 << 6) != 0
    }

    /// True if the transfer occurs LSB-first, false if it's MSB-first
    fn lsb_first(self) -> bool {
        self.0 & (1 << 5) != 0
    }

    /// True if we're the SPI master, false otherwise
    fn master_mode(self) -> bool {
        self.0 & (1 << 4) != 0
    }

    /// True if we clock the SPI using SysClock / 16, false if we use SysClock /  2
    fn clock_rate_sysctk_16(self) -> bool {
        self.0 & 1 != 0
    }
}

/// Serial peripheral status register (SPSR)
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct SspiStat(u8);

impl SspiStat {
    fn new() -> SspiStat {
        SspiStat(0)
    }

    fn clear(&mut self) {
        self.0 = 0;
    }

    /// Set the Serial Transfer Complete Flag (SPIF)
    fn set_spif(&mut self) {
        self.0 |= 1 << 7;
    }
}

/// Miscellaneous register (0x003e)
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct Misc(u8);

impl Misc {
    fn new() -> Misc {
        // Bit 7 is "OSC Time Up Flag" (FTUP).
        // XXX It should be set after timebase has counted 8072 clocks after fosce is set. Maybe
        // not at startup though? Since we start directly from it.
        //
        // Bit 6 is "XOSC Time Up Flag" (STUP), it's always 0 on the PSX since there's no external
        // oscillator.
        //
        // Bits 4 and 5 are reserved and always 0
        Misc(0x8a)
    }

    fn set(&mut self, v: u8) {
        // Only the low nibble is writeable
        self.0 &= 0xf0;
        self.0 |= v & 0xf;

        if !self.fosce() {
            // Since we have no external oscillator on the PSX that shouldn't happen
            unimplemented!("main oscilator disable!");
        }

        // Normally the PSX should only use 00 for SYS1/SYS0 since that's the highest possible
        // frequency. That should give us a CPU Bus Frequency of about 2.1MHz (assuming OSC is
        // ~4.2MHz)
        if self.divider() != OscDivider::OscDiv2 {
            unimplemented!("Unsupported divider {:?}", self.divider());
        }
    }

    fn optm(self) -> bool {
        self.0 & 1 != 0
    }

    fn fosce(self) -> bool {
        self.0 & (1 << 1) != 0
    }

    fn divider(self) -> OscDivider {
        match (self.0 >> 2) & 3 {
            0 => OscDivider::OscDiv2,
            1 => OscDivider::OscDiv4,
            2 => OscDivider::OscDiv64,
            3 => OscDivider::XOscDiv2,
            _ => unreachable!(),
        }
    }
}

#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum OscDivider {
    OscDiv2 = 0,
    OscDiv4 = 1,
    OscDiv64 = 2,
    XOscDiv2 = 3,
}

/// Total size of all ROM sections (watch out, they're not contiguous in the controller's memory
/// map, they're just normally dumped that way):
///
/// * 16KiB MASK ROM
/// * 480B SELF-CHECK ROM
/// * 16B TEST VECTORS
/// * 16B USER VECTORS
pub const ROM_DUMP_SIZE: usize = 16 * 1024 + 480 + 16 + 16;

mod map {
    //! MC68HC05 microcontroller memory map

    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct Range(u16, u16);

    impl Range {
        /// Return `Some(offset)` if addr is contained in `self`
        pub const fn contains(self, addr: u16) -> Option<u16> {
            let Range(start, length) = self;

            if addr >= start && addr < start + length {
                Some(addr - start)
            } else {
                None
            }
        }

        pub fn iter(self) -> RangeIterator {
            RangeIterator {
                cur: self.0,
                end: self.0.wrapping_add(self.1),
            }
        }

        pub const fn start(self) -> usize {
            self.0 as usize
        }

        pub const fn len(self) -> usize {
            self.1 as usize
        }
    }

    pub struct RangeIterator {
        cur: u16,
        end: u16,
    }

    impl ::std::iter::Iterator for RangeIterator {
        type Item = usize;

        fn next(&mut self) -> Option<Self::Item> {
            if self.cur != self.end {
                let i = self.cur;
                self.cur = self.cur.wrapping_add(1);

                Some(i as usize)
            } else {
                None
            }
        }
    }

    /// 512B of RAM
    pub const RAM: Range = Range(0x0040, 512);

    /// 16KiB of MASK ROM
    pub const MASK_ROM: Range = Range(0x1000, 16 * 1024);

    /// 480B of SELF-CHECK ROM
    pub const SELF_CHECK_ROM: Range = Range(0xfe00, 480);

    /// Vectors used in self-check mode
    pub const TEST_VECTORS: Range = Range(0xffe0, 16);

    /// Vectors used in user mode
    pub const USER_VECTORS: Range = Range(0xfff0, 16);
}

/// The microcontroller frequency varies a bit between models.
///
/// On the SCPH-5502 (PU-18) the oscillator it appears to be about 4.23MHz by my measurements. No$
/// says it's 4.2336MHz, I'm not really sure where that value comes from but conveniently it's
/// 44.1kHz * 96 so it does seem like a good frequency to use since it means that
/// UC_CYCLES_PER_AUDIO_CYCLE will be an integer.
///
/// The value is divided by 2 because the microcontroller's system clock is OSC / 2
pub const UC_FREQ: u32 = 4_233_600 / 2;

/// Number of microcontroller cycles per audio cycle (@44.1kHz)
const UC_CYCLES_PER_AUDIO_CYCLE: u32 = UC_FREQ / 44_100;

mod serialize_memory {
    use super::map::RAM;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct SerializedRam {
        #[serde(with = "serde_big_array::BigArray")]
        ram: [u8; RAM.len()],
    }

    pub fn serialize<S>(mem: &[u8; 0x1_0000], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut ram = [0; RAM.len()];

        for (i, &b) in mem.iter().skip(RAM.start()).take(RAM.len()).enumerate() {
            ram[i] = b;
        }

        let s = SerializedRam { ram };

        s.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 0x1_0000], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = SerializedRam::deserialize(deserializer)?;

        let mut ram = [0x90; 0x1_0000];

        for (i, &b) in s.ram.iter().enumerate() {
            ram[RAM.start() + i] = b
        }

        Ok(ram)
    }
}
