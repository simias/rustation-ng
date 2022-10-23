/// MDEC command and output FIFOs
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Fifo {
    buffer: [u32; 0x20],
    /// Read index in buffer
    read_index: u8,
    /// Write index in buffer
    write_index: u8,
}

impl Fifo {
    pub fn new() -> Fifo {
        Fifo {
            buffer: [0; 0x20],
            read_index: 0,
            write_index: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.read_index == self.write_index
    }

    pub fn is_full(&self) -> bool {
        self.len() == self.buffer.len()
    }

    pub fn len(&self) -> usize {
        let l = self.write_index.wrapping_sub(self.read_index);

        l as usize
    }

    /// Empty the FIFO completely
    pub fn clear(&mut self) {
        self.read_index = self.write_index;
    }

    /// Push an entry in the FIFO. Should *not* be called when the FIFO is full!
    pub fn push(&mut self, val: u32) {
        debug_assert!(!self.is_full());

        let i = self.write_index & 0x1f;

        self.write_index = self.write_index.wrapping_add(1);

        self.buffer[i as usize] = val;
    }

    /// Pop an entry from the FIFO. Should *not* be called when the FIFO is empty!
    pub fn pop(&mut self) -> u32 {
        debug_assert!(!self.is_empty());

        let i = self.read_index & 0x1f;

        self.read_index = self.read_index.wrapping_add(1);

        self.buffer[i as usize]
    }
}

#[test]
fn test_command_fifo() {
    let mut fifo = Fifo::new();

    // Empty FIFO
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Push element
    fifo.push(1);
    assert!(!fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 1);

    // Pop
    assert_eq!(fifo.pop(), 1);
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Fill
    for i in 0..0x20 {
        assert!(!fifo.is_full());
        assert!(fifo.len() == i);
        fifo.push(i as u32);
    }

    assert!(fifo.is_full());
    assert!(!fifo.is_empty());
    assert_eq!(fifo.len(), 0x20);

    // Empty
    for i in 0..0x20 {
        assert!(!fifo.is_empty());
        assert_eq!(fifo.pop(), i as u32);
    }
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Fill
    for i in 0..0x20 {
        assert!(!fifo.is_full());
        assert!(fifo.len() == i);
        fifo.push(i as u32);
    }

    assert!(fifo.is_full());
    assert!(!fifo.is_empty());
    assert_eq!(fifo.len(), 0x20);

    // Clear
    fifo.clear();
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Fill interleaved
    for i in 0..(0x20 - 1) {
        assert!(!fifo.is_full());

        let v = i as u32;

        fifo.push(v);
        fifo.push(v);
        fifo.pop();
    }

    assert!(!fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0x20 - 1);

    fifo.push(0xc0_ffee);
    assert!(fifo.is_full());
    assert!(!fifo.is_empty());
    assert_eq!(fifo.len(), 0x20);

    // Empty interleaved
    for i in 0..0x20 {
        assert!(!fifo.is_empty());
        fifo.pop();
        fifo.push(i as u32);
        fifo.pop();
    }

    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);
}

#[test]
fn test_fifo_pointer_overflow() {
    let mut fifo = Fifo::new();

    for v in 0..256 {
        for i in 0..0x20 {
            assert_eq!(fifo.len(), i);
            fifo.push(v);
        }

        for i in 0..0x20 {
            assert_eq!(fifo.len(), 0x20 - i);
            assert_eq!(fifo.pop(), v);
        }

        assert_eq!(fifo.len(), 0);
    }
}
