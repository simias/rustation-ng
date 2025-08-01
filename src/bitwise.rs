/// Simple trait to simplify bit manipulation
pub trait Bitwise {
    /// Returns true if the given bit is set in `self`
    fn bit(self, bitpos: u8) -> bool;

    /// Sets the given bit in self to 1 if `v` is true, 0 if `v` is false
    fn set_bit(&mut self, bitpos: u8, v: bool);

    /// Sets the given bit in self to 1
    #[cfg(test)]
    fn set_bit_h(&mut self, bitpos: u8) {
        self.set_bit(bitpos, true)
    }

    /// Sets the given bit in self to 0
    #[cfg(test)]
    fn set_bit_l(&mut self, bitpos: u8) {
        self.set_bit(bitpos, false)
    }
}

impl Bitwise for u8 {
    fn bit(self, bitpos: u8) -> bool {
        self & (1u8 << bitpos) != 0
    }

    fn set_bit(&mut self, bitpos: u8, v: bool) {
        *self &= !(1u8 << bitpos);
        *self |= (v as u8) << bitpos;
    }
}

impl Bitwise for u16 {
    fn bit(self, bitpos: u8) -> bool {
        self & (1u16 << bitpos) != 0
    }

    fn set_bit(&mut self, bitpos: u8, v: bool) {
        *self &= !(1u16 << bitpos);
        *self |= (v as u16) << bitpos;
    }
}

impl Bitwise for u32 {
    fn bit(self, bitpos: u8) -> bool {
        self & (1u32 << bitpos) != 0
    }

    fn set_bit(&mut self, bitpos: u8, v: bool) {
        *self &= !(1u32 << bitpos);
        *self |= (v as u32) << bitpos;
    }
}

#[test]
fn bitwise() {
    let mut v = 0xaau8;

    assert!(v.bit(7));
    assert!(!v.bit(6));

    v.set_bit(6, true);

    assert!(v.bit(6));
    assert_eq!(v, 0xea);

    v.set_bit(7, false);

    assert!(!v.bit(7));
    assert_eq!(v, 0x6a);

    v.set_bit_h(0);
    v.set_bit_l(1);
    assert!(v.bit(0));
    assert!(!v.bit(1));
    assert_eq!(v, 0x69);
}
