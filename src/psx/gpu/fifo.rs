use super::COMMAND_FIFO_DEPTH;

/// GP0 command FIFO
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CommandFifo {
    buffer: [u32; COMMAND_FIFO_DEPTH],
    /// Read index in buffer. One bit wider that COMMAND_FIFO_DEPTH to differentiate FIFO full and
    /// FIFO empty.
    read_index: u8,
    /// Write index in buffer. One bit wider that COMMAND_FIFO_DEPTH to differentiate FIFO full and
    /// FIFO empty.
    write_index: u8,
}

impl CommandFifo {
    pub fn new() -> CommandFifo {
        CommandFifo {
            buffer: [0; COMMAND_FIFO_DEPTH],
            read_index: 0,
            write_index: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.read_index == self.write_index
    }

    pub fn is_full(&self) -> bool {
        self.len() == COMMAND_FIFO_DEPTH
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

        let i = self.write_index % COMMAND_FIFO_DEPTH as u8;

        self.write_index = self.write_index.wrapping_add(1);

        self.buffer[i as usize] = val;
    }

    /// Pop an entry from the FIFO. Should *not* be called when the FIFO is empty!
    pub fn pop(&mut self) -> u32 {
        debug_assert!(!self.is_empty());

        let i = self.read_index % COMMAND_FIFO_DEPTH as u8;

        self.read_index = self.read_index.wrapping_add(1);

        self.buffer[i as usize]
    }

    /// Returns the element at the top of the FIFO but doesn't pop it. Should *not* be called when
    /// the FIFO is empty!
    pub fn peek(&self) -> u32 {
        debug_assert!(!self.is_empty());

        let i = self.read_index % COMMAND_FIFO_DEPTH as u8;

        self.buffer[i as usize]
    }
}

#[test]
fn test_command_fifo() {
    let mut fifo = CommandFifo::new();

    // Empty FIFO
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Push element
    fifo.push(1);
    assert!(!fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 1);

    // Peek
    assert_eq!(fifo.peek(), 1);
    assert!(!fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 1);

    // Pop
    assert_eq!(fifo.pop(), 1);
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Fill
    for i in 0..COMMAND_FIFO_DEPTH {
        assert!(!fifo.is_full());
        assert!(fifo.len() == i);
        fifo.push(i as u32);
    }

    assert!(fifo.is_full());
    assert!(!fifo.is_empty());
    assert_eq!(fifo.len(), COMMAND_FIFO_DEPTH);

    // Empty
    for i in 0..COMMAND_FIFO_DEPTH {
        assert!(!fifo.is_empty());
        assert_eq!(fifo.pop(), i as u32);
    }
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Fill
    for i in 0..COMMAND_FIFO_DEPTH {
        assert!(!fifo.is_full());
        assert!(fifo.len() == i);
        fifo.push(i as u32);
    }

    assert!(fifo.is_full());
    assert!(!fifo.is_empty());
    assert_eq!(fifo.len(), COMMAND_FIFO_DEPTH);

    // Clear
    fifo.clear();
    assert!(fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), 0);

    // Fill interleaved
    for i in 0..(COMMAND_FIFO_DEPTH - 1) {
        assert!(!fifo.is_full());

        let v = i as u32;

        fifo.push(v);
        fifo.push(v);
        fifo.pop();
    }

    assert!(!fifo.is_empty());
    assert!(!fifo.is_full());
    assert_eq!(fifo.len(), COMMAND_FIFO_DEPTH - 1);

    fifo.push(0xc0_ffee);
    assert!(fifo.is_full());
    assert!(!fifo.is_empty());
    assert_eq!(fifo.len(), COMMAND_FIFO_DEPTH);

    // Empty interleaved
    for i in 0..COMMAND_FIFO_DEPTH {
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
    let mut fifo = CommandFifo::new();

    for v in 0..256 {
        for i in 0..COMMAND_FIFO_DEPTH {
            assert_eq!(fifo.len(), i);
            fifo.push(v);
        }

        for i in 0..COMMAND_FIFO_DEPTH {
            assert_eq!(fifo.len(), COMMAND_FIFO_DEPTH - i);
            assert_eq!(fifo.pop(), v);
        }

        assert_eq!(fifo.len(), 0);
    }
}
