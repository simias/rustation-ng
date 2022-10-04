use super::Cdc;

/// Branch if bit n set
fn brset(cdc: &mut Cdc, bit: u8) {
    let test_addr = Dir::get_addr(cdc);
    let target = Rel::get_target(cdc);

    let v = super::load(cdc, test_addr);

    cdc.uc.carry = v & (1 << bit) != 0;

    if cdc.uc.carry {
        cdc.uc.jump(target);
    }
}

/// Branch if bit 0 set
fn op_brset0(cdc: &mut Cdc) {
    brset(cdc, 0);
}

/// Branch if bit 1 set
fn op_brset1(cdc: &mut Cdc) {
    brset(cdc, 1);
}

/// Branch if bit 2 set
fn op_brset2(cdc: &mut Cdc) {
    brset(cdc, 2);
}

/// Branch if bit 3 set
fn op_brset3(cdc: &mut Cdc) {
    brset(cdc, 3);
}

/// Branch if bit 4 set
fn op_brset4(cdc: &mut Cdc) {
    brset(cdc, 4);
}

/// Branch if bit 5 set
fn op_brset5(cdc: &mut Cdc) {
    brset(cdc, 5);
}

/// Branch if bit 6 set
fn op_brset6(cdc: &mut Cdc) {
    brset(cdc, 6);
}

/// Branch if bit 7 set
fn op_brset7(cdc: &mut Cdc) {
    brset(cdc, 7);
}

/// Branch if bit n set
fn brclr(cdc: &mut Cdc, bit: u8) {
    let test_addr = Dir::get_addr(cdc);
    let target = Rel::get_target(cdc);

    let v = super::load(cdc, test_addr);

    cdc.uc.carry = v & (1 << bit) != 0;

    if !cdc.uc.carry {
        cdc.uc.jump(target);
    }
}

/// Branch if bit 0 clear
fn op_brclr0(cdc: &mut Cdc) {
    brclr(cdc, 0);
}

/// Branch if bit 1 clear
fn op_brclr1(cdc: &mut Cdc) {
    brclr(cdc, 1);
}

/// Branch if bit 2 clear
fn op_brclr2(cdc: &mut Cdc) {
    brclr(cdc, 2);
}

/// Branch if bit 3 clear
fn op_brclr3(cdc: &mut Cdc) {
    brclr(cdc, 3);
}

/// Branch if bit 4 clear
fn op_brclr4(cdc: &mut Cdc) {
    brclr(cdc, 4);
}

/// Branch if bit 5 clear
fn op_brclr5(cdc: &mut Cdc) {
    brclr(cdc, 5);
}

/// Branch if bit 6 clear
fn op_brclr6(cdc: &mut Cdc) {
    brclr(cdc, 6);
}

/// Branch if bit 7 clear
fn op_brclr7(cdc: &mut Cdc) {
    brclr(cdc, 7);
}

/// Set bit n
fn bset(cdc: &mut Cdc, bit: u8) {
    let addr = Dir::get_addr(cdc);

    let v = super::load(cdc, addr);

    super::store(cdc, addr, v | (1 << bit))
}

/// Set bit 0
fn op_bset0(cdc: &mut Cdc) {
    bset(cdc, 0);
}

/// Set bit 1
fn op_bset1(cdc: &mut Cdc) {
    bset(cdc, 1);
}

/// Set bit 2
fn op_bset2(cdc: &mut Cdc) {
    bset(cdc, 2);
}

/// Set bit 3
fn op_bset3(cdc: &mut Cdc) {
    bset(cdc, 3);
}

/// Set bit 4
fn op_bset4(cdc: &mut Cdc) {
    bset(cdc, 4);
}

/// Set bit 5
fn op_bset5(cdc: &mut Cdc) {
    bset(cdc, 5);
}

/// Set bit 6
fn op_bset6(cdc: &mut Cdc) {
    bset(cdc, 6);
}

/// Set bit 7
fn op_bset7(cdc: &mut Cdc) {
    bset(cdc, 7);
}

/// Clear bit n
fn bclr(cdc: &mut Cdc, bit: u8) {
    let addr = Dir::get_addr(cdc);

    let v = super::load(cdc, addr);

    super::store(cdc, addr, v & !(1 << bit))
}

/// Clear bit 0
fn op_bclr0(cdc: &mut Cdc) {
    bclr(cdc, 0);
}

/// Clear bit 1
fn op_bclr1(cdc: &mut Cdc) {
    bclr(cdc, 1);
}

/// Clear bit 2
fn op_bclr2(cdc: &mut Cdc) {
    bclr(cdc, 2);
}

/// Clear bit 3
fn op_bclr3(cdc: &mut Cdc) {
    bclr(cdc, 3);
}

/// Clear bit 4
fn op_bclr4(cdc: &mut Cdc) {
    bclr(cdc, 4);
}

/// Clear bit 5
fn op_bclr5(cdc: &mut Cdc) {
    bclr(cdc, 5);
}

/// Clear bit 6
fn op_bclr6(cdc: &mut Cdc) {
    bclr(cdc, 6);
}

/// Clear bit 7
fn op_bclr7(cdc: &mut Cdc) {
    bclr(cdc, 7);
}

/// Transfer accumulator to index register
fn op_tax(cdc: &mut Cdc) {
    cdc.uc.x = cdc.uc.a;
}

/// Clear carry bit
fn op_clc(cdc: &mut Cdc) {
    cdc.uc.carry = false;
}

/// Set carry bit
fn op_sec(cdc: &mut Cdc) {
    cdc.uc.carry = true;
}

/// Clear interrupt mask
fn op_cli(cdc: &mut Cdc) {
    cdc.uc.irq_disable = false;
    // See if we had a pending IRQ
    super::maybe_interrupt(cdc);
}

/// Set interrupt mask
fn op_sei(cdc: &mut Cdc) {
    cdc.uc.irq_disable = true;
}

/// Reset stack pointer
fn op_rsp(cdc: &mut Cdc) {
    cdc.uc.sp = 0x00ff;
}

/// No operation
fn op_nop(cdc: &mut Cdc) {
    let _ = cdc;
}

/// Transfer index register to accumulator
fn op_txa(cdc: &mut Cdc) {
    cdc.uc.a = cdc.uc.x;
}

/// Branch always
fn op_bra(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    cdc.uc.jump(target);
}

/// Branch if higher
fn op_bhi(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if !cdc.uc.carry && !cdc.uc.zero {
        cdc.uc.jump(target);
    }
}

/// Branch if lower or same
fn op_bls(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if cdc.uc.carry || cdc.uc.zero {
        cdc.uc.jump(target);
    }
}

/// Branch if carry bit clear
fn op_bcc(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if !cdc.uc.carry {
        cdc.uc.jump(target);
    }
}

/// Branch if carry bit set
fn op_bcs(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if cdc.uc.carry {
        cdc.uc.jump(target);
    }
}

/// Branch if not equal
fn op_bne(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if !cdc.uc.zero {
        cdc.uc.jump(target);
    }
}

/// Branch if equal
fn op_beq(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if cdc.uc.zero {
        cdc.uc.jump(target);
    }
}

/// Branch if half-carry clear
fn op_bhcc(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if !cdc.uc.half_carry {
        cdc.uc.jump(target);
    }
}

/// Branch if half-carry set
fn op_bhcs(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if cdc.uc.half_carry {
        cdc.uc.jump(target);
    }
}

/// Branch if plus
fn op_bpl(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if !cdc.uc.negative {
        cdc.uc.jump(target);
    }
}

/// Branch if minus
fn op_bmi(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    if cdc.uc.negative {
        cdc.uc.jump(target);
    }
}

/// Decrement byte
fn op_dec<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    let v = super::load(cdc, addr).wrapping_sub(1);
    super::store(cdc, addr, v);

    cdc.uc.negative = (v & 0x80) != 0;
    cdc.uc.zero = v == 0;
}

/// Increment byte
fn op_inc<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    let v = super::load(cdc, addr).wrapping_add(1);
    super::store(cdc, addr, v);

    cdc.uc.negative = (v & 0x80) != 0;
    cdc.uc.zero = v == 0;
}

/// Rotate accumulator right through carry
fn op_rora(cdc: &mut Cdc) {
    let v = cdc.uc.a;
    let c = cdc.uc.carry as u8;

    let a = v >> 1 | (c << 7);

    cdc.uc.a = a;

    cdc.uc.carry = (v & 1) != 0;
    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Rotate index right through carry
fn op_rorx(cdc: &mut Cdc) {
    let v = cdc.uc.x;
    let c = cdc.uc.carry as u8;

    let x = v >> 1 | (c << 7);

    cdc.uc.x = x;

    cdc.uc.carry = (v & 1) != 0;
    cdc.uc.negative = (x & 0x80) != 0;
    cdc.uc.zero = x == 0;
}

/// Logical shift left accumulator
fn op_lsla(cdc: &mut Cdc) {
    let v = cdc.uc.a;

    let a = v << 1;
    cdc.uc.a = a;

    cdc.uc.carry = (v & 0x80) != 0;
    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Rotate left through carry
fn op_rol<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    let v = super::load(cdc, addr);
    let c = cdc.uc.carry as u8;

    let r = v << 1 | c;

    super::store(cdc, addr, r);

    cdc.uc.carry = (v & 0x80) != 0;
    cdc.uc.negative = (r & 0x80) != 0;
    cdc.uc.zero = r == 0;
}

/// Rotate accumulator left through carry
fn op_rola(cdc: &mut Cdc) {
    let v = cdc.uc.a;
    let c = cdc.uc.carry as u8;

    let a = v << 1 | c;

    cdc.uc.a = a;

    cdc.uc.carry = (v & 0x80) != 0;
    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Negate A (A = -A, 2's complement)
fn op_nega(cdc: &mut Cdc) {
    let v = cdc.uc.a;

    // 2's complement negation
    let a = (!v).wrapping_add(1);

    cdc.uc.a = a;

    // I think? Not sure what value the carry takes exactly, for now I'm assuming that it's set if
    // the `wrapping_add` of the 2's complement overflows.
    cdc.uc.carry = a == 0;
    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Unsigned multiply accumulator with index
///
/// The 16 bit result is stored in X:A
fn op_mul(cdc: &mut Cdc) {
    let a = cdc.uc.a as u16;
    let x = cdc.uc.x as u16;

    let v = a * x;

    cdc.uc.a = v as u8;
    cdc.uc.x = (v >> 8) as u8;

    cdc.uc.carry = false;
    cdc.uc.half_carry = false;
}

/// Logical shift right
fn op_lsr<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    let v = super::load(cdc, addr);

    let r = v >> 1;

    super::store(cdc, addr, r);

    cdc.uc.carry = (v & 1) != 0;
    cdc.uc.negative = false;
    cdc.uc.zero = r == 0;
}

/// Logical shift right accumulator
fn op_lsra(cdc: &mut Cdc) {
    let v = cdc.uc.a;

    cdc.uc.a = v >> 1;

    cdc.uc.carry = (v & 1) != 0;
    cdc.uc.negative = false;
    cdc.uc.zero = cdc.uc.a == 0;
}

/// Clear byte
fn op_clr<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    super::store(cdc, addr, 0);

    cdc.uc.negative = false;
    cdc.uc.zero = true;
}

/// Clear accumulator
fn op_clra(cdc: &mut Cdc) {
    cdc.uc.a = 0;
    cdc.uc.negative = false;
    cdc.uc.zero = true;
}

/// Clear index
fn op_clrx(cdc: &mut Cdc) {
    cdc.uc.x = 0;
    cdc.uc.negative = false;
    cdc.uc.zero = true;
}

/// Decrement accumulator
fn op_deca(cdc: &mut Cdc) {
    let a = cdc.uc.a.wrapping_sub(1);

    cdc.uc.a = a;
    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Increment accumulator
fn op_inca(cdc: &mut Cdc) {
    let a = cdc.uc.a.wrapping_add(1);

    cdc.uc.a = a;
    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Decrement index
fn op_decx(cdc: &mut Cdc) {
    let x = cdc.uc.x.wrapping_sub(1);

    cdc.uc.x = x;
    cdc.uc.negative = (x & 0x80) != 0;
    cdc.uc.zero = x == 0;
}

/// Increment index
fn op_incx(cdc: &mut Cdc) {
    let x = cdc.uc.x.wrapping_add(1);

    cdc.uc.x = x;
    cdc.uc.negative = (x & 0x80) != 0;
    cdc.uc.zero = x == 0;
}

/// Return from interrupt
fn op_rti(cdc: &mut Cdc) {
    cdc.uc.pop_ccr();
    cdc.uc.a = cdc.uc.pop();
    cdc.uc.x = cdc.uc.pop();
    cdc.uc.pop_pc()
}

/// Return from subroutine
fn op_rts(cdc: &mut Cdc) {
    cdc.uc.pop_pc()
}

/// Load into accumulator
fn op_lda<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc);

    cdc.uc.a = v;

    cdc.uc.negative = (v & 0x80) != 0;
    cdc.uc.zero = v == 0;
}

/// Subtract memory byte and carry bit from accumulator
fn op_sbc<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc) as u16;
    let a = cdc.uc.a as u16;
    let c = cdc.uc.carry as u16;

    let r = a.wrapping_sub(v).wrapping_sub(c);

    cdc.uc.negative = (r & 0x80) != 0;
    cdc.uc.zero = (r & 0xff) == 0;
    cdc.uc.carry = r & 0x100 != 0;
    // According to the datasheet the halfcarry is not modified by this instruction (unlike adc)

    cdc.uc.a = r as u8;
}

/// Subtract memory byte from accumulator
fn op_sub<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc) as u16;
    let a = cdc.uc.a as u16;

    let r = a.wrapping_sub(v);

    cdc.uc.negative = (r & 0x80) != 0;
    cdc.uc.zero = (r & 0xff) == 0;
    cdc.uc.carry = r & 0x100 != 0;
    // According to the datasheet the halfcarry is not modified by this instruction (unlike add)

    cdc.uc.a = r as u8;
}

/// Compare accumulator with memory byte
fn op_cmp<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    // Same implementation as SUB, only we don't store the result of the subtraction back in A
    let v = M::get_value(cdc) as u16;

    let a = cdc.uc.a as u16;
    let r = a.wrapping_sub(v);

    cdc.uc.negative = (r & 0x80) != 0;
    cdc.uc.zero = (r & 0xff) == 0;
    cdc.uc.carry = r & 0x100 != 0;
    // According to the datasheet the halfcarry is not modified by this instruction (unlike add)
}

/// Compare accumulator with memory byte
fn op_cpx<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    // Same implementation as SUB, only we don't store the result of the subtraction back in A
    let v = M::get_value(cdc) as u16;

    let x = cdc.uc.x as u16;
    let r = x.wrapping_sub(v);

    cdc.uc.negative = (r & 0x80) != 0;
    cdc.uc.zero = (r & 0xff) == 0;
    cdc.uc.carry = r & 0x100 != 0;
    // According to the datasheet the halfcarry is not modified by this instruction (unlike add)
}

/// Logical AND accumulator with memory
fn op_and<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc);

    let a = cdc.uc.a & v;

    cdc.uc.a = a;

    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Exclusive OR accumulator with memory
fn op_eor<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc);

    let a = cdc.uc.a ^ v;

    cdc.uc.a = a;

    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Logical OR accumulator with memory
fn op_ora<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc);

    let a = cdc.uc.a | v;

    cdc.uc.a = a;

    cdc.uc.negative = (a & 0x80) != 0;
    cdc.uc.zero = a == 0;
}

/// Add accumulator with memory byte and carry
fn op_adc<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc) as u16;
    let a = cdc.uc.a as u16;
    let c = cdc.uc.carry as u16;

    let r = a + v + c;

    cdc.uc.negative = (r & 0x80) != 0;
    cdc.uc.zero = (r & 0xff) == 0;
    cdc.uc.carry = r & 0x100 != 0;
    cdc.uc.half_carry = (a ^ v ^ r) & 0x10 != 0;

    cdc.uc.a = r as u8;
}

/// Add accumulator with memory byte
fn op_add<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc) as u16;
    let a = cdc.uc.a as u16;

    let r = a + v;

    cdc.uc.negative = (r & 0x80) != 0;
    cdc.uc.zero = (r & 0xff) == 0;
    cdc.uc.carry = r & 0x100 != 0;
    cdc.uc.half_carry = (a ^ v ^ r) & 0x10 != 0;

    cdc.uc.a = r as u8;
}

/// Branch to subroutine
fn op_bsr(cdc: &mut Cdc) {
    let target = Rel::get_target(cdc);

    cdc.uc.push_pc();

    cdc.uc.jump(target);
}

/// Load into index
fn op_ldx<M>(cdc: &mut Cdc)
where
    M: ValueMode,
{
    let v = M::get_value(cdc);

    cdc.uc.x = v;

    cdc.uc.negative = (v & 0x80) != 0;
    cdc.uc.zero = v == 0;
}

fn sta(cdc: &mut Cdc, addr: u16) {
    super::store(cdc, addr, cdc.uc.a);
}

/// Store accumulator
fn op_sta<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    sta(cdc, addr);
}

fn stx(cdc: &mut Cdc, addr: u16) {
    super::store(cdc, addr, cdc.uc.x);
}

/// Store index
fn op_stx<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    stx(cdc, addr);
}

/// Unconditional jump
fn op_jmp<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    cdc.uc.jump(addr);
}

/// Jump to subroutine
fn op_jsr<M>(cdc: &mut Cdc)
where
    M: AddressingMode,
{
    let addr = M::get_addr(cdc);

    cdc.uc.push_pc();

    cdc.uc.jump(addr);
}

/// Placeholder for unimplemented instructions
fn unimplemented(cdc: &mut Cdc) {
    let prev_pc = cdc.uc.pc.wrapping_sub(1);

    let op = cdc.uc.memory[prev_pc as usize];

    unimplemented!("Implement opcode 0x{:x} {:?}", op, cdc.uc);
}

/// Addressing mode
trait AddressingMode {
    fn get_addr(cdc: &mut Cdc) -> u16;
}

/// Superset of AddressingMode for commands like LDX that take an immediate value (for which
/// `AddressingMode::get_addr` makes no sense).
trait ValueMode {
    fn get_value(cdc: &mut Cdc) -> u8;
}

/// All addressing modes can be used as parameter values by dereferencing the address
impl<T> ValueMode for T
where
    T: AddressingMode,
{
    fn get_value(cdc: &mut Cdc) -> u8 {
        let addr = Self::get_addr(cdc);

        super::load(cdc, addr)
    }
}

/// Immediate 8bit value
struct Imm;

impl ValueMode for Imm {
    fn get_value(cdc: &mut Cdc) -> u8 {
        cdc.uc.pc_next()
    }
}

/// Immediate 8bit address (high 8bits are set to 0)
struct Dir;

impl AddressingMode for Dir {
    fn get_addr(cdc: &mut Cdc) -> u16 {
        cdc.uc.pc_next() as u16
    }
}

/// Indexed, no offset: X
struct Ix;

impl AddressingMode for Ix {
    fn get_addr(cdc: &mut Cdc) -> u16 {
        u16::from(cdc.uc.x)
    }
}

/// Indexed, 8bit offset: X + imm8
struct Ix1;

impl AddressingMode for Ix1 {
    fn get_addr(cdc: &mut Cdc) -> u16 {
        let off = u16::from(cdc.uc.pc_next());
        let x = u16::from(cdc.uc.x);

        off + x
    }
}

/// Indexed, 16 offset: X + imm16
struct Ix2;

impl AddressingMode for Ix2 {
    fn get_addr(cdc: &mut Cdc) -> u16 {
        let off_hi = cdc.uc.pc_next() as u16;
        let off_lo = cdc.uc.pc_next() as u16;
        let off = off_lo | (off_hi << 8);

        let x = cdc.uc.x as u16;

        off.wrapping_add(x)
    }
}

/// Immediate 16bit address
struct Ext;

impl AddressingMode for Ext {
    fn get_addr(cdc: &mut Cdc) -> u16 {
        let hi = cdc.uc.pc_next() as u16;
        let lo = cdc.uc.pc_next() as u16;

        lo | (hi << 8)
    }
}

/// PC-relative addressing mode (used for branches)
struct Rel;

impl Rel {
    fn get_target(cdc: &mut Cdc) -> u16 {
        let off = (cdc.uc.pc_next() as i8) as i16;

        cdc.uc.pc.wrapping_add(off as u16)
    }
}

pub struct Instruction {
    /// Handler to execute the instruction
    pub handler: fn(&mut Cdc),
    /// Number of cycles taken by the instruction to execute
    pub cycles: u8,
}

pub static OPCODE_MAP: [Instruction; 0x100] = [
    // 0x00
    Instruction {
        handler: op_brset0,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr0,
        cycles: 5,
    },
    Instruction {
        handler: op_brset1,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr1,
        cycles: 5,
    },
    Instruction {
        handler: op_brset2,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr2,
        cycles: 5,
    },
    Instruction {
        handler: op_brset3,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr3,
        cycles: 5,
    },
    // 0x08
    Instruction {
        handler: op_brset4,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr4,
        cycles: 5,
    },
    Instruction {
        handler: op_brset5,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr5,
        cycles: 5,
    },
    Instruction {
        handler: op_brset6,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr6,
        cycles: 5,
    },
    Instruction {
        handler: op_brset7,
        cycles: 5,
    },
    Instruction {
        handler: op_brclr7,
        cycles: 5,
    },
    // 0x10
    Instruction {
        handler: op_bset0,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr0,
        cycles: 5,
    },
    Instruction {
        handler: op_bset1,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr1,
        cycles: 5,
    },
    Instruction {
        handler: op_bset2,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr2,
        cycles: 5,
    },
    Instruction {
        handler: op_bset3,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr3,
        cycles: 5,
    },
    // 0x18
    Instruction {
        handler: op_bset4,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr4,
        cycles: 5,
    },
    Instruction {
        handler: op_bset5,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr5,
        cycles: 5,
    },
    Instruction {
        handler: op_bset6,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr6,
        cycles: 5,
    },
    Instruction {
        handler: op_bset7,
        cycles: 5,
    },
    Instruction {
        handler: op_bclr7,
        cycles: 5,
    },
    // 0x20
    Instruction {
        handler: op_bra,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_bhi,
        cycles: 3,
    },
    Instruction {
        handler: op_bls,
        cycles: 3,
    },
    Instruction {
        handler: op_bcc,
        cycles: 3,
    },
    Instruction {
        handler: op_bcs,
        cycles: 3,
    },
    Instruction {
        handler: op_bne,
        cycles: 3,
    },
    Instruction {
        handler: op_beq,
        cycles: 3,
    },
    // 0x28
    Instruction {
        handler: op_bhcc,
        cycles: 3,
    },
    Instruction {
        handler: op_bhcs,
        cycles: 3,
    },
    Instruction {
        handler: op_bpl,
        cycles: 3,
    },
    Instruction {
        handler: op_bmi,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x30
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lsr::<Dir>,
        cycles: 5,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x38
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_rol::<Dir>,
        cycles: 5,
    },
    Instruction {
        handler: op_dec::<Dir>,
        cycles: 5,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_inc::<Dir>,
        cycles: 5,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_clr::<Dir>,
        cycles: 5,
    },
    // 0x40
    Instruction {
        handler: op_nega,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_mul,
        cycles: 11,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lsra,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_rora,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x48
    Instruction {
        handler: op_lsla,
        cycles: 3,
    },
    Instruction {
        handler: op_rola,
        cycles: 3,
    },
    Instruction {
        handler: op_deca,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_inca,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_clra,
        cycles: 3,
    },
    // 0x50
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_rorx,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x58
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_decx,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_incx,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_clrx,
        cycles: 3,
    },
    // 0x60
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x68
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_clr::<Ix1>,
        cycles: 6,
    },
    // 0x70
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x78
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x80
    Instruction {
        handler: op_rti,
        cycles: 9,
    },
    Instruction {
        handler: op_rts,
        cycles: 6,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x88
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0x90
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_tax,
        cycles: 2,
    },
    // 0x98
    Instruction {
        handler: op_clc,
        cycles: 2,
    },
    Instruction {
        handler: op_sec,
        cycles: 2,
    },
    Instruction {
        handler: op_cli,
        cycles: 2,
    },
    Instruction {
        handler: op_sei,
        cycles: 2,
    },
    Instruction {
        handler: op_rsp,
        cycles: 2,
    },
    Instruction {
        handler: op_nop,
        cycles: 2,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_txa,
        cycles: 2,
    },
    // 0xa0
    Instruction {
        handler: op_sub::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: op_cmp::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: op_sbc::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: op_cpx::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: op_and::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lda::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0xa8
    Instruction {
        handler: op_eor::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: op_adc::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: op_ora::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: op_add::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_bsr,
        cycles: 6,
    },
    Instruction {
        handler: op_ldx::<Imm>,
        cycles: 2,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    // 0xb0
    Instruction {
        handler: op_sub::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_cmp::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_sbc::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_cpx::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_and::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lda::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_sta::<Dir>,
        cycles: 4,
    },
    // 0xb8
    Instruction {
        handler: op_eor::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_adc::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_ora::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_add::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_jmp::<Dir>,
        cycles: 2,
    },
    Instruction {
        handler: op_jsr::<Dir>,
        cycles: 5,
    },
    Instruction {
        handler: op_ldx::<Dir>,
        cycles: 3,
    },
    Instruction {
        handler: op_stx::<Dir>,
        cycles: 4,
    },
    // 0xc0
    Instruction {
        handler: op_sub::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_cmp::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_sbc::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_cpx::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_and::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lda::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_sta::<Ext>,
        cycles: 5,
    },
    // 0xc8
    Instruction {
        handler: op_eor::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_adc::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_ora::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_add::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_jmp::<Ext>,
        cycles: 3,
    },
    Instruction {
        handler: op_jsr::<Ext>,
        cycles: 6,
    },
    Instruction {
        handler: op_ldx::<Ext>,
        cycles: 4,
    },
    Instruction {
        handler: op_stx::<Ext>,
        cycles: 5,
    },
    // 0xd0
    Instruction {
        handler: op_sub::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_cmp::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_sbc::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_cpx::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_and::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lda::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_sta::<Ix2>,
        cycles: 6,
    },
    // 0xd8
    Instruction {
        handler: op_eor::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_adc::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_ora::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_add::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_jmp::<Ix2>,
        cycles: 4,
    },
    Instruction {
        handler: op_jsr::<Ix2>,
        cycles: 7,
    },
    Instruction {
        handler: op_ldx::<Ix2>,
        cycles: 5,
    },
    Instruction {
        handler: op_stx::<Ix2>,
        cycles: 6,
    },
    // 0xe0
    Instruction {
        handler: op_sub::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_cmp::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_sbc::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_cpx::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_and::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lda::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_sta::<Ix1>,
        cycles: 5,
    },
    // 0xe8
    Instruction {
        handler: op_eor::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_adc::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_ora::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_add::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_jmp::<Ix1>,
        cycles: 3,
    },
    Instruction {
        handler: op_jsr::<Ix1>,
        cycles: 6,
    },
    Instruction {
        handler: op_ldx::<Ix1>,
        cycles: 4,
    },
    Instruction {
        handler: op_stx::<Ix1>,
        cycles: 5,
    },
    // 0xf0
    Instruction {
        handler: op_sub::<Ix>,
        cycles: 4,
    },
    Instruction {
        handler: op_cmp::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_sbc::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_cpx::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_and::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: unimplemented,
        cycles: 0,
    },
    Instruction {
        handler: op_lda::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_sta::<Ix>,
        cycles: 4,
    },
    // 0xf8
    Instruction {
        handler: op_eor::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_adc::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_ora::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_add::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_jmp::<Ix>,
        cycles: 2,
    },
    Instruction {
        handler: op_jsr::<Ix>,
        cycles: 5,
    },
    Instruction {
        handler: op_ldx::<Ix>,
        cycles: 3,
    },
    Instruction {
        handler: op_stx::<Ix>,
        cycles: 4,
    },
];
