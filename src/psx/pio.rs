//! Emulation of the parallel interface (the Parallel I/O connector on the back of older PlayStation
//! models)

use super::{Addressable, Psx};

pub struct Pio {
    module: Box<dyn PioModule>,
}

impl Pio {
    pub fn disconnected() -> Pio {
        Pio {
            module: Box::new(Disconnected),
        }
    }
}

impl Default for Pio {
    fn default() -> Self {
        Pio::disconnected()
    }
}

pub fn store<T: Addressable>(psx: &mut Psx, off: u32, val: T) {
    let v = val.as_u32();

    for i in 0..(T::width() as u32) {
        let b = (v >> (8 * i)) as u8;

        psx.pio.module.store(off + i, b);
    }
}

pub fn load<T: Addressable>(psx: &mut Psx, off: u32) -> T {
    let mut r = 0;

    for i in 0..(T::width() as u32) {
        let b = psx.pio.module.load(off + i);

        r |= (b as u32) << (8 * i);
    }

    T::from_u32(r)
}

/// Since there can be all sorts of hardware connected to the Parallel
/// I/O port I abstract it behind a trait interface
pub trait PioModule {
    /// Parallel I/O load 8bits at offset `off` (within the
    /// expansion 1 memory region)
    fn load(&mut self, off: u32) -> u8;

    /// Parallel I/O byte store at offset `off` (within the expansion 1
    /// memory region)
    fn store(&mut self, off: u32, val: u8);
}

/// A dummy implementation of PioModule when nothing is connected
pub struct Disconnected;

impl PioModule for Disconnected {
    fn load(&mut self, _off: u32) -> u8 {
        // When no expansion is present the CPU reads full ones
        !0
    }

    fn store(&mut self, _off: u32, _val: u8) {
        // NOP
    }
}
