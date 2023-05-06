//! Optimized data structure holding the parts of the PSX address space that can contain executable
//! code.

use super::bios::BIOS_SIZE;
use super::{cpu, AccessWidth, Addressable};
use crate::box_array::BoxArray;
use crate::error::{PsxError, Result};
use crate::sha::sha256;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// This structure manages all executable portions of memory and offers fast lookup to speed up
/// instruction fetches.
///
/// This assumes that these regions behave like actual memory and not hardware register: reading
/// these locations has no side-effect. This is obviously true for the RAM and BIOS but may not be
/// the case for Expansion memory, so that may cause compatibility issues if we ever need to
/// implement support for extensions that associate side-effects to instruction fetches. I don't
/// know if such extensions exist.
pub struct XMemory {
    /// We currently only support executing from RAM and BIOS so we need three concrete pages:
    ///
    /// * The RAM page
    /// * The BIOS page (the first 512KiB contain the BIOS, the rest is padded with 0xff)
    /// * The "bad" page that's filled with 0xff and used as placeholder for all pages that are not
    ///   executable.
    memory: BoxArray<u32, MEMORY_SIZE>,
    /// Look up table containing PAGE_SIZE offsets in `memory` for all pages in the system
    offset_lut: [u8; PAGE_COUNT],
    /// BIOS SHA-256, used to make sure that we load the same BIOS when restoring the savestate
    bios_sha256: [u8; 32],
}

impl XMemory {
    pub fn new() -> XMemory {
        let mut xmem = XMemory {
            // 0xffff_ffff isn't a valid instruction, so we'll know right away if we're executing
            // from a bad location. Also, 0xff is normally what's returned from unmapped memory
            // reads so it's sort of accurate for these regions.
            memory: BoxArray::from_vec(vec![0xffff_ffff; (PAGE_SIZE_BYTES * 3) >> 2]),
            offset_lut: [MemoryPage::Bad as u8; PAGE_COUNT],
            bios_sha256: [0; 32],
        };

        // Remap executable pages
        for &region in &REGION_OFFSETS {
            // RAM: mirrored 4 times
            for i in 0..4 {
                xmem.remap(region + i * RAM_SIZE as u32, MemoryPage::Ram);
            }
            // BIOS
            xmem.remap(region + 0x1fc0_0000, MemoryPage::Bios);
        }

        xmem
    }

    /// Make `offset_lut` point at `target` for memory address `addr`
    fn remap(&mut self, addr: u32, target: MemoryPage) {
        let page = (addr >> PAGE_SHIFT) as usize;

        // Make sure our address ranges don't overlap
        assert!(
            self.offset_lut[page] == MemoryPage::Bad as u8,
            "Remapping over good mapping!"
        );

        self.offset_lut[page] = target as u8;
    }

    /// Set the contents of the BIOS
    pub fn set_bios(&mut self, bios: &[u8; BIOS_SIZE]) {
        let bios_base = (MemoryPage::Bios as u32) << PAGE_SHIFT;

        // Since BIOS_SIZE is less than our PAGE_SIZE the leftover bytes are going to be left with
        // 0xff
        for i in 0..BIOS_SIZE {
            self.store(bios_base + i as u32, bios[i]);
        }

        // Update bios_sha256
        self.bios_sha256 = sha256(bios);
    }

    /// Copy the BIOS from another XMemory instance, returning an error if the checksums differ
    pub fn copy_bios(&mut self, source: &XMemory) -> Result<()> {
        let bios_base = ((MemoryPage::Bios as usize) << PAGE_SHIFT) / 4;
        let bios_len = BIOS_SIZE / 4;

        if self.bios_sha256 != source.bios_sha256 && self.bios_sha256.iter().any(|&b| b != 0) {
            return Err(PsxError::BadBios("BIOS SHA-256 don't match".to_string()));
        }

        for (i, &w) in source
            .memory
            .iter()
            .enumerate()
            .skip(bios_base)
            .take(bios_len)
        {
            self.memory[i] = w;
        }

        Ok(())
    }

    /// Fetch data from memory at `offset`
    fn load<T: Addressable>(&self, offset: u32) -> T {
        let offset = offset as usize;

        let word = self.memory[offset >> 2];

        let v = match T::width() {
            AccessWidth::Word => word,
            AccessWidth::HalfWord => {
                let which = (offset >> 1) & 1;

                (word >> (which * 16)) & 0xffff
            }
            AccessWidth::Byte => {
                let which = offset & 3;

                (word >> (which * 8)) & 0xff
            }
        };

        Addressable::from_u32(v)
    }

    /// Store data to memory at `offset`
    fn store<T: Addressable>(&mut self, offset: u32, val: T) {
        let offset = offset as usize;

        let val = val.as_u32();

        match T::width() {
            AccessWidth::Word => self.memory[offset / 4] = val,
            AccessWidth::HalfWord => {
                let mut word = self.memory[offset / 4];

                let shift = (offset << 3) & 16;

                word &= !(0xffff << shift);
                word |= val << shift;

                self.memory[offset >> 2] = word;
            }
            AccessWidth::Byte => {
                let mut word = self.memory[offset / 4];

                let shift = (offset << 3) & 24;

                word &= !(0xff << shift);
                word |= val << shift;

                self.memory[offset >> 2] = word;
            }
        }
    }

    /// Read from RAM at `offset`
    pub fn ram_load<T: Addressable>(&self, offset: u32) -> T {
        let ram_base = (MemoryPage::Ram as u32) << PAGE_SHIFT;

        // The two MSBs are ignored, the 2MB RAM is mirrored four times over the first 8MB of
        // address space
        let offset = offset & 0x1f_ffff;

        self.load(ram_base + offset)
    }

    /// Write `val` to RAM at `offset`
    pub fn ram_store<T: Addressable>(&mut self, offset: u32, val: T) {
        let ram_base = (MemoryPage::Ram as u32) << PAGE_SHIFT;

        // The two MSBs are ignored, the 2MB RAM is mirrored four times over the first 8MB of
        // address space
        let offset = offset & 0x1f_ffff;

        self.store(ram_base + offset, val);
    }

    /// Read from BIOS at `offset`
    pub fn bios_load<T: Addressable>(&self, offset: u32) -> T {
        let bios_base = (MemoryPage::Bios as u32) << PAGE_SHIFT;

        self.load(bios_base + offset)
    }

    /// Fetch instruction at absolute address `addr`
    pub fn load_instruction(&self, addr: u32) -> cpu::Instruction {
        let page = addr >> PAGE_SHIFT;

        let mem_page = self.offset_lut[page as usize] as u32;

        let mut offset = mem_page << PAGE_SHIFT;

        offset |= addr & ((1 << PAGE_SHIFT) - 1);

        // We index one word at a time
        offset >>= 2;

        let word = self.memory[offset as usize];

        cpu::Instruction::new(word)
    }
}

#[derive(Serialize, Deserialize)]
struct SerializedMemory {
    ram: BoxArray<u32, RAM_SIZE_WORDS>,
    bios_sha256: [u8; 32],
}

impl Serialize for XMemory {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let ram_base = ((MemoryPage::Ram as usize) << PAGE_SHIFT) / 4;
        let ram_len = RAM_SIZE / 4;
        let ram: Vec<_> = self
            .memory
            .iter()
            .skip(ram_base)
            .take(ram_len)
            .cloned()
            .collect();

        let s = SerializedMemory {
            ram: BoxArray::from_vec(ram),
            bios_sha256: self.bios_sha256,
        };

        s.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for XMemory {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = SerializedMemory::deserialize(deserializer)?;

        let mut xmem = XMemory::new();

        xmem.bios_sha256 = s.bios_sha256;

        for (i, &w) in s.ram.iter().enumerate() {
            xmem.ram_store((i * 4) as u32, w);
        }

        Ok(xmem)
    }
}

/// Order of the pages in `XMemory::memory`
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum MemoryPage {
    Ram = 0,
    Bios = 1,
    Bad = 2,
}

/// Defines how big each cache page will be (log2 since it's a shift value).
///
/// I use 2MB since it's the largest usable size: anything bigger won't let us address the RAM
/// properly since it's supposed to be mirrored 4 times contiguously.
///
/// This means that pages are actually larger than the full BIOS but that's not a problem since
/// there are no other executable regions within 2MB of the BIOS address space, so we can just pad
/// with dummy bytes.
const PAGE_SHIFT: u8 = 21;

/// Page size in bytes
const PAGE_SIZE_BYTES: usize = 1 << PAGE_SHIFT;

/// Total number of pages to cover the full 32bit address space
const PAGE_COUNT: usize = 1 << (32 - PAGE_SHIFT);

/// Offsets for the three memory regions containing executable code: KUSEG, KSEG0 and KSEG1
const REGION_OFFSETS: [u32; 3] = [0x0000_0000, 0x8000_0000, 0xa000_0000];

/// System RAM: 2MB
const RAM_SIZE: usize = 2 * 1024 * 1024;

/// RAM size in number of 32bit words
const RAM_SIZE_WORDS: usize = RAM_SIZE / 4;

/// Total size of the memory buffer, in 32bit words
const MEMORY_SIZE: usize = (PAGE_SIZE_BYTES * 3) >> 2;
