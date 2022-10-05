use std::ops::Index;

/// 16 sample FIFO used to store decoded audio samples
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DecoderFifo {
    /// Sample buffer
    buffer: [i16; 16],
    /// Write pointer (4bits + carry)
    write_idx: u8,
    /// Read pointer (4bits + carry)
    read_idx: u8,
}

impl DecoderFifo {
    pub fn new() -> DecoderFifo {
        DecoderFifo {
            buffer: [0; 16],
            write_idx: 0,
            read_idx: 0,
        }
    }

    pub fn is_full(&self) -> bool {
        // The FIFO is full if both indexes point to the same cell while having a different carry.
        self.write_idx == self.read_idx ^ 0x10
    }

    pub fn clear(&mut self) {
        self.write_idx = 0;
        self.read_idx = 0;
    }

    pub fn len(&self) -> u8 {
        (self.write_idx.wrapping_sub(self.read_idx)) & 0x1f
    }

    pub fn push(&mut self, val: i16) {
        debug_assert!(!self.is_full());

        let idx = (self.write_idx & 0xf) as usize;

        self.buffer[idx] = val;

        self.write_idx = self.write_idx.wrapping_add(1) & 0x1f;
    }

    pub fn discard(&mut self, how_many: usize) {
        debug_assert!(self.len() as usize >= how_many);

        self.read_idx = self.read_idx.wrapping_add(how_many as u8);
    }
}

impl Index<usize> for DecoderFifo {
    type Output = i16;

    fn index(&self, index: usize) -> &Self::Output {
        debug_assert!(self.len() as usize > index);

        let mut idx = self.read_idx as usize;
        idx += index;
        idx &= 0xf;

        &self.buffer[idx]
    }
}
