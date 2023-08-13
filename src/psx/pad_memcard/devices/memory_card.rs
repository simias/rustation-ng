use super::{DeviceInterface, DsrState};
use crate::box_array::BoxArray;
use crate::psx::CycleCount;

/// The standard SCPH-1020 memory card
pub struct MemoryCard {
    /// The non-volatile Flash memory itself
    memory: BoxArray<u8, FLASH_SIZE>,
    /// Write counter, incremented every time the memory is written to *if* the contents of the
    /// flash have been changed.
    write_counter: u32,
    /// Set to true after the first successful write to flash
    has_been_written: bool,
    /// Type of memory card access for the current command
    access_type: AccessType,
    /// Sector index for the current command (if applicable)
    sector_index: u16,
    /// Last command byte we received (used to simplify some commands that echo the previously
    /// received byte in subsequent accesses)
    last_command: u8,
    /// Buffer holding the memory card write data while it's being received, plus the checksum
    /// byte.
    write_buffer: [u8; 129],
    /// Most games (and the BIOS) don't seem to notice if we swap memory cards without going
    /// through a disconnected state, so we use this counter to disable the memory card for a few
    /// frames upon disconnection
    disabled_frames: u16,
}

impl MemoryCard {
    /// Creates an empty memory card image
    pub fn new_formatted() -> MemoryCard {
        let mut mc = MemoryCard::new_with_memory(BoxArray::from_vec(vec![0; FLASH_SIZE]));

        mc.format();

        mc
    }

    /// Create a memory card image with the provided memory contents
    pub fn new_with_memory(memory: BoxArray<u8, FLASH_SIZE>) -> MemoryCard {
        MemoryCard {
            memory,
            write_counter: 0,
            has_been_written: false,
            access_type: AccessType::Id,
            sector_index: 0,
            last_command: 0,
            write_buffer: [0; 129],
            disabled_frames: 0,
        }
    }

    /// Perform some basic format tests to see if the current memory contents appear to be a valid
    /// memory card image.
    pub fn is_format_valid(&self) -> bool {
        let mut valid = true;

        // Check header
        valid &= self.memory[0] == b'M';
        valid &= self.memory[1] == b'C';
        valid &= checksum(&self.memory[0..127]) == self.memory[127];

        // Check directory entry integrity
        // XXX We could also validate the directory structure itself if we wanted.
        for b in 1..16 {
            let off = b as usize;

            let start = off * SECTOR_SIZE;
            let end = start + SECTOR_SIZE;

            let metadata = &self.memory[start..end];
            valid &= checksum(&metadata[0..127]) == metadata[127];
        }

        valid
    }

    /// Reformat the Memory Card. This obviously erases the entire contents so it should be used
    /// with caution...
    pub fn format(&mut self) {
        // This is a bit overkill technically, but we might as well just erase everything
        for b in self.memory.iter_mut() {
            *b = 0u8;
        }

        // First sector: Magic and checksum
        self.memory[0] = b'M';
        self.memory[1] = b'C';

        self.memory[127] = checksum(&self.memory[0..127]);

        // Directory entries
        for b in 1..16 {
            let off = b as usize;

            let start = off * SECTOR_SIZE;
            let end = start + SECTOR_SIZE;

            let metadata = &mut self.memory[start..end];

            // Status: free and unused
            metadata[0] = 0xa0;

            // Next block pointer set to none
            metadata[8] = 0xff;
            metadata[9] = 0xff;

            metadata[127] = checksum(&metadata[0..127]);
        }

        // Broken sector list
        for s in 0..20 {
            let off = (16 + s) as usize;

            let start = off * SECTOR_SIZE;
            let end = (off + 1) * SECTOR_SIZE;

            let sector = &mut self.memory[start..end];

            // Sector position set to none
            sector[0] = 0xff;
            sector[1] = 0xff;
            sector[2] = 0xff;
            sector[3] = 0xff;

            // Not sure what this is but the card I'm using has those two bytes set as well. It's
            // at the same position as the next block pointer in block entries but it doesn't make
            // a lot of sense here.
            sector[8] = 0xff;
            sector[9] = 0xff;
        }
    }

    fn handle_read(&mut self, seq: u8, cmd: u8) -> (u8, Option<CycleCount>) {
        match seq {
            4 => {
                // Receive sector index MSB. There's no validation for the address here even though
                // technically values greater than 3 make no sense. The card doesn't abort the
                // transaction now, it does it after sending the address back below.
                self.sector_index = u16::from(cmd) << 8;

                (0x00, Some(240))
            }
            5 => {
                self.sector_index |= u16::from(cmd);

                // We return the value written at the previous step
                (self.last_command, Some(240))
            }
            // "Command acknowledge 1" according to No$. Very long delay until DSR, but the
            // variation seems to be only of a couple hundred cycles. Does not care if the index is
            // out of range.
            6 => (0x5c, Some(31_000)),
            // "Command acknowledge 2" according to No$
            7 => (0x5d, Some(570)),
            8 => {
                let r = if self.sector_index <= 0x3ff {
                    // Valid index
                    (self.sector_index >> 8) as u8
                } else {
                    // Invalid index
                    0xff
                };

                (r, Some(261))
            }
            9 => {
                if self.sector_index <= 0x3ff {
                    // Valid index
                    (self.sector_index as u8, Some(570))
                } else {
                    // Invalid index, the sequence ends here
                    (0xff, None)
                }
            }
            10..=137 => {
                // Read the sector byte by byte
                let mut index = (self.sector_index as usize) * 128;
                index += (seq - 10) as usize;

                let b = self.memory[index];

                (b, Some(250))
            }
            138 => {
                let index = (self.sector_index as usize) * 128;
                let mut csum = checksum(&self.memory[index..(index + 128)]);
                // Checksum includes the index
                csum ^= (self.sector_index >> 8) as u8;
                csum ^= self.sector_index as u8;

                (csum, Some(250))
            }
            // Final byte: command result. 'G' for success, can it ever fail?
            139 => (b'G', None),
            _ => unreachable!(),
        }
    }

    fn handle_write(&mut self, seq: u8, cmd: u8) -> (u8, Option<CycleCount>) {
        match seq {
            4 => {
                // Receive sector index MSB. There's no validation for the address here even though
                // technically values greater than 3 make no sense. The transaction goes all the
                // way and a error status is returned at the very end to notify the user.
                self.sector_index = u16::from(cmd) << 8;

                (0x00, Some(240))
            }
            5 => {
                self.sector_index |= u16::from(cmd);

                // We return the value written at the previous step
                (self.last_command, Some(240))
            }
            // Receive the 128bytes of sector data and the checksum
            6..=134 => {
                let index = (seq - 6) as usize;

                self.write_buffer[index] = cmd;

                (self.last_command, Some(240))
            }
            // "Command acknowledge 1" according to No$
            135 => (0x5c, Some(210)),
            // "Command acknowledge 2" according to No$. At this point if the index is correct the
            // data is written to RAM, *even* if the checksum is wrong.
            136 => {
                // Checksum errors override sector errors in my tests.
                //
                // XXX Maybe I did something wrong but it seems that sometimes I get checksum
                // errors ('N') for bad sectors even though the checksum should be ok. Maybe
                // there's more to it than that?
                let mut csum = checksum(&self.write_buffer);

                // We also checksum the index
                csum ^= (self.sector_index >> 8) as u8;
                csum ^= self.sector_index as u8;

                if self.sector_index <= 0x3ff {
                    let mut flash_changed = false;
                    let base = (self.sector_index as usize) * 128;
                    for i in 0..128 {
                        if self.memory[base + i] != self.write_buffer[i] {
                            flash_changed = true;
                            self.memory[base + i] = self.write_buffer[i];
                        }
                    }

                    if flash_changed {
                        // Use wrapping arithmetics in case somebody is hardcore enough to write
                        // 2**32 sectors. In real time that would take about a year of constant
                        // writing.
                        self.write_counter = self.write_counter.wrapping_add(1);
                    }

                    // I've tested that the "not_written" flag goes down at this specific moment:
                    // if I interrupt the sequence before this point it stays up. The last byte
                    // below doesn't matter however.
                    //
                    // The bit only goes down if the write is successful (i.e. it returns 'G': the
                    // index is correct and the checksum is correct)
                    if csum == 0 && self.sector_index <= 0x3ff {
                        self.has_been_written = true;
                    }
                }

                // Store the csum in the now useless write_buffer, we'll need it for the next cycle
                self.write_buffer[0] = csum;

                (0x5d, Some(210))
            }
            // Final byte: command result. 'G' for success, 'N' for bad checksum, 0xff for bad
            // index. can it ever fail?
            137 => {
                // Retreive the checksum we stored in the write_buffer above
                let csum = self.write_buffer[0];

                // Checksum errors override sector errors in my tests.
                let result = if csum != 0 {
                    // Bad checksum
                    b'N'
                } else if self.sector_index > 0x3ff {
                    // Bad index
                    0xff
                } else {
                    // All good
                    b'G'
                };

                (result, None)
            }
            _ => unreachable!(),
        }
    }

    fn handle_id(&mut self, seq: u8, _cmd: u8) -> (u8, Option<CycleCount>) {
        match seq {
            // "Command acknowledge 1" according to No$
            4 => (0x5c, Some(210)),
            // "Command acknowledge 2" according to No$
            5 => (0x5d, Some(210)),
            // Not sure what these bytes mean.
            6 => (0x04, Some(210)),
            7 => (0x00, Some(210)),
            8 => (0x00, Some(210)),
            9 => (0x80, None),
            _ => unreachable!(),
        }
    }
}

impl DeviceInterface for MemoryCard {
    fn description(&self) -> String {
        "PlayStation Memory Card (SCPH-1020)".to_string()
    }

    fn handle_command(&mut self, seq: u8, cmd: u8) -> (u8, DsrState) {
        if self.disabled_frames > 0 {
            return (0xff, DsrState::Idle);
        }

        let (resp, dsr_delay) = match seq {
            // First byte should be 0x81 if the command targets the memory card
            0 => (0xff, if cmd == 0x81 { Some(350) } else { None }),
            1 => {
                // Bit 3 must be set if we haven't written to this card since it's been inserted in
                // the system
                let not_written = !self.has_been_written;
                let response = (not_written as u8) << 3;

                let (dsr, access_type) = match cmd {
                    b'R' => (Some(360), AccessType::Read),
                    b'W' => (Some(450), AccessType::Write),
                    b'S' => (Some(450), AccessType::Id),
                    // Unknown command, abort
                    _ => (None, AccessType::Id),
                };

                self.access_type = access_type;

                (response, dsr)
            }
            // First ID byte (sent for all commands, not just 'S')
            2 => (0x5a, Some(210)),
            // Second ID byte (sent for all commands, not just 'S')
            3 => (0x5d, Some(240)),
            // After that the sequence changes depending on the access_type
            n => match self.access_type {
                AccessType::Read => self.handle_read(n, cmd),
                AccessType::Write => self.handle_write(n, cmd),
                AccessType::Id => self.handle_id(n, cmd),
            },
        };

        let dsr_state = match dsr_delay {
            // The pulse length seems to always be 24 or 35.
            Some(delay) => DsrState::Pending(delay, 24),
            None => DsrState::Idle,
        };

        self.last_command = cmd;

        (resp, dsr_state)
    }

    fn get_memory(&self) -> Option<&[u8; FLASH_SIZE]> {
        Some(&self.memory)
    }

    fn write_counter(&self) -> u32 {
        self.write_counter
    }

    fn connected(&mut self) {
        // This may prevent *some* data corruption when a savestate is loaded (since it triggers a
        // reconnection). The idea is that if the BIOS sees that the write flag has been reset it
        // will *probably* think it's a new memory card and reload all the contents.
        //
        // Of course if the savestate was made during a memory card access, all bets are off.
        self.has_been_written = false;

        // Disable the memory card (that is, make it look as if there's no memory card connected)
        // for about two seconds.
        self.disabled_frames = 120;
    }

    fn new_frame(&mut self) {
        if self.disabled_frames > 0 {
            self.disabled_frames -= 1;
        }
    }
}

/// The various types of accesses to a Memory Card
enum AccessType {
    /// Read a sector
    Read = b'R' as isize,
    /// Write a sector
    Write = b'W' as isize,
    /// Get memory card ID
    Id = b'S' as isize,
}

/// Basic 8bit XOR checksum used by the memory card
fn checksum(d: &[u8]) -> u8 {
    d.iter().fold(0, |c, b| c ^ b)
}

/// Size of a single sector.
pub const SECTOR_SIZE: usize = 128;

/// There are 64 sectors per block
pub const SECTORS_PER_BLOCK: usize = 64;

/// Size of a single block in bytes. There are 16 blocks in a memory card.
pub const BLOCK_SIZE: usize = SECTORS_PER_BLOCK * SECTOR_SIZE;

/// Total size of a memory card in bytes
pub const FLASH_SIZE: usize = BLOCK_SIZE * 16;

#[test]
fn test_format() {
    let mc = MemoryCard::new_formatted();

    assert!(mc.is_format_valid());
}
