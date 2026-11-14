//! Parallel I/O module used to load raw PlayStation executables. This doesn't emulate any real
//! world hardware, it's inspired by mednafen's method of loading EXEs.

use byteorder::{LittleEndian, ReadBytesExt};
use std::fs::File;
use std::io;
use std::path::Path;

use crate::assembler::{Assembler, syntax::*};
use crate::error::{PsxError, Result};
use crate::psx::bios::Bios;
use crate::psx::cd::disc::Region;
use crate::psx::map;

use super::PioModule;

pub struct ExeLoader {
    /// Base address for the executable
    base: u32,
    /// Executable entry point
    entry: u32,
    /// GP value before jumping to the entry point
    initial_gp: u32,
    /// SP value before jumping to the entry point
    initial_sp: u32,
    /// Base address of the 0-filled area
    memfill_base: u32,
    /// Length of the 0-filled area
    memfill_len: u32,
    /// Region of the executable. `None` if the region for the
    /// executable couldn't be determined.
    region: Option<Region>,
    /// "text" section of the executable
    text: Vec<u8>,
    /// Index into `text` for the code FIFO
    text_index: usize,
    /// Code of the loader
    loader: Vec<u8>,
}

impl ExeLoader {
    pub fn load(r: &mut dyn io::Read) -> Result<ExeLoader> {
        let mut buf = [0; 16];

        r.read_exact(&mut buf)?;

        if &buf != b"PS-X EXE\0\0\0\0\0\0\0\0" {
            // Bad magic, this is not a PlayStation executable
            error!("Invalid PS-X EXE header");
            return Err(PsxError::BadPsExe);
        }

        let entry = r.read_u32::<LittleEndian>()?;

        let initial_gp = r.read_u32::<LittleEndian>()?;

        let base = r.read_u32::<LittleEndian>()?;

        let text_len = r.read_u32::<LittleEndian>()?;

        // Let's be on the safe side and reject anormaly big
        // programs. Since the PlayStation RAM is 2MB big it doesn't
        // make sense to have bigger programs
        if text_len > 2 * 1024 * 1024 {
            error!("PS-X EXE .text len is too big");
            return Err(PsxError::BadPsExe);
        }

        // The next two words are Unknown/Unused in the No$ spec,
        // let's ignore them
        let _ = r.read_u32::<LittleEndian>()?;
        let _ = r.read_u32::<LittleEndian>()?;

        let memfill_base = r.read_u32::<LittleEndian>()?;
        let memfill_len = r.read_u32::<LittleEndian>()?;

        // For some reason the initial SP address comes with an
        // "offset" (per No$), not sure what that's for
        let initial_sp = r.read_u32::<LittleEndian>()? + r.read_u32::<LittleEndian>()?;

        // The next 20bytes are padding
        r.read_exact(&mut [0; 20])?;

        // Skip the first part of the license string to get to the region
        r.read_exact(&mut [0; 37])?;

        let mut region_str = [0; 5];

        r.read_exact(&mut region_str)?;

        let region = match &region_str {
            b"Japan" => Some(Region::Japan),
            b"Europ" => Some(Region::Europe),
            b"North" => Some(Region::NorthAmerica),
            // Unknown or missing region
            _ => None,
        };

        // Read through all the huge padding
        r.read_exact(&mut [0; 1930])?;

        // Finally we can read the executable itself
        let mut text = vec![0; text_len as usize];

        r.read_exact(&mut text)?;

        let mut loader = ExeLoader {
            base: base,
            entry: entry,
            initial_gp: initial_gp,
            initial_sp: initial_sp,
            memfill_base: memfill_base,
            memfill_len: memfill_len,
            region: region,
            text: text,
            text_index: 0,
            loader: Vec::new(),
        };

        info!(
            "Loaded PS-EXE: BASE=0x{:08x} ENTRY=0x{:08x} LEN={}",
            base, entry, text_len
        );

        loader.assemble_loader();

        Ok(loader)
    }

    /// Assemble the code for the native loader whose purpose is to
    /// load the executable in RAM
    fn assemble_loader(&mut self) {
        let mut asm = Assembler::from_base(LOADER_ENTRY_ADDRESS);

        let irq_base = map::IRQ_CONTROL.start();
        let cache_control = map::CACHE_CONTROL.start();

        // First let's write a quick "clear_cache" function

        asm.assemble(&[
            // Let's mask all interrupts before we start, the
            // executable will be free to re-enable them.
            Li(T0, irq_base),
            Sh(R0, T0, 4),
            // Let's start by taking care of the "memfill" region
            Li(T0, self.memfill_base),
            Li(T1, self.memfill_len),
            // Skip memfil if len is 0
            Beqz(T1, Label::Local("memfill_done", 'f')),
            // Set T1 to the end address
            Add(T1, T0, T1),
            Local("memfill_loop"),
            // Do the memfill one byte at a time, not very efficient
            // but that way we don't have to worry about alignment
            Addiu(T0, T0, 1),
            Bne(T0, T1, Label::Local("memfill_loop", 'b')),
            Sb(R0, T0, -1),
            Local("memfill_done"),
            // Now we can move on to copying the code from the
            // EXE_FIFO
            Li(T0, self.base),
            Li(T1, self.text.len() as u32),
            // We should probably not have no text to copy, but let's
            // be cautious
            Beqz(T1, Label::Local("text_copy_done", 'f')),
            // Set T1 to the end address
            Add(T1, T0, T1),
            // T2 to the location of the EXE FIFO
            Li(T2, EXE_FIFO_ADDRESS),
            // Reset the FIFO
            Sb(R0, T2, 0),
            Local("text_copy_loop"),
            // Load next text byte
            Lb(T3, T2, 0),
            Addiu(T0, T0, 1),
            Bne(T0, T1, Label::Local("text_copy_loop", 'b')),
            Sb(T3, T0, -1),
            Local("text_copy_done"),
            // Call the "clear_cache" function
            Jal(Label::Global("clear_cache")),
            Nop,
            // Finally we can load the register values and jump into
            // the EXE
            Li(GP, self.initial_gp),
            Li(SP, self.initial_sp),
            Li(T0, self.entry),
            Jalr(RA, T0),
            Nop,
            // We're done, there's probably no point in returning to
            // the caller, let's loop infinitely. If we wanted to
            // return to the caller we'd have to save the registers
            // (particularly SP) before jumping into the EXE so that
            // we can restore them here.
            Local("infinite_loop"),
            B(Label::Local("infinite_loop", 'b')),
            Nop,
            // Clear cache function
            Global("clear_cache"),
            Addiu(SP, SP, -24),
            Sw(RA, SP, 20),
            Sw(FP, SP, 16),
            Move(FP, SP),
            // First we need to move to KSEG1 (uncached region)
            La(T0, Label::Local("uncached", 'f')),
            Lui(T1, 0xa000),
            Or(T0, T0, T1),
            Jr(T0),
            Nop,
            Local("uncached"),
            // We're now running from uncached memory
            Li(T0, cache_control),
            Lw(T5, T0, 0),
            // Enable i-cache, set "tag test mode"
            Li(T1, 0x804),
            Sw(T1, T0, 0),
            // Isolate the cache
            Mfc0(T1, 12),
            Li(T2, 0x00010000),
            Or(T1, T1, T2),
            Mtc0(T1, 12),
            // Write 0 to each 4th word from 0 to 4095 to invalidate
            // each cacheline in the 4KB i-cache.
            Li(T1, 0x1000),
            Li(T2, 0),
            Local("icache_invalidate_loop"),
            Addiu(T2, T2, 16),
            Bne(T2, T1, Label::Local("icache_invalidate_loop", 'b')),
            Sw(R0, T2, -16),
            // De-isolate the cache
            Mfc0(T1, 12),
            Li(T2, !0x00010000),
            And(T1, T1, T2),
            Mtc0(T1, 12),
            // Clear tag test mode
            Li(T1, 0x800),
            Sw(T1, T0, 0),
            // Re-isolate the cache
            Mfc0(T1, 12),
            Li(T2, 0x00010000),
            Or(T1, T1, T2),
            Mtc0(T1, 12),
            // Write 0 to each word from 0 to 4095 to invalidate each
            // word in the 4KB i-cache. I don't think this is truly
            // necessary but the BIOS does it.
            Li(T1, 0x1000),
            Li(T2, 0),
            Local("icache_zero_loop"),
            Addiu(T2, T2, 4),
            Bne(T2, T1, Label::Local("icache_zero_loop", 'b')),
            Sw(R0, T2, -4),
            // De-isolate the cache
            Mfc0(T1, 12),
            Li(T2, !0x00010000),
            And(T1, T1, T2),
            Mtc0(T1, 12),
            // Restore cache control
            Sw(T5, T0, 0),
            // Return
            Move(SP, FP),
            Lw(RA, SP, 20),
            Lw(FP, SP, 16),
            Jr(RA),
            Addiu(SP, SP, 24),
        ])
        .unwrap();

        let (mc, _) = asm.machine_code();

        self.loader = mc;
    }

    pub fn load_file(path: &Path) -> Result<ExeLoader> {
        let mut f = File::open(path)?;

        ExeLoader::load(&mut f)
    }

    pub fn region(&self) -> Option<Region> {
        self.region
    }

    /// Patch the BIOS animation jump to run the loader code
    /// instead. Returns an error if the patching failed.
    pub fn patch_bios(&self, bios: &mut Bios) -> Result<()> {
        let mut asm = Assembler::from_base(0);

        // Assemble the jump instruction
        let instruction = Jal(Label::Absolute(LOADER_ENTRY_ADDRESS));

        asm.assemble(&[instruction]).unwrap();

        let (mc, _) = asm.machine_code();

        // It should only have generated a single instruction
        assert!(mc.len() == 4);

        // reassemble the instruction word
        let instruction = u32::from_le_bytes([mc[0], mc[1], mc[2], mc[3]]);

        // Finally we can try to patch the BIOS
        bios.patch_animation_jump_hook(instruction)
    }
}

impl PioModule for ExeLoader {
    fn load(&mut self, off: u32) -> u8 {
        if off == EXE_FIFO_OFFSET {
            match self.text.get(self.text_index) {
                Some(&b) => {
                    self.text_index += 1;
                    b
                }
                None => {
                    warn!("Load from EXE FIFO when empty");
                    !0
                }
            }
        } else if off >= LOADER_ENTRY_OFFSET {
            let i = off - LOADER_ENTRY_OFFSET;

            // There's nothing after the loader, if we're out of
            // bounds we return full ones.
            *self.loader.get(i as usize).unwrap_or(&!0)
        } else {
            !0
        }
    }

    fn store(&mut self, off: u32, val: u8) {
        if off == EXE_FIFO_OFFSET && val == 0 {
            // Reset the index
            self.text_index = 0;
        }
    }
}

/// Offset of the register containing the machine code FIFO for
/// loading the EXE
const EXE_FIFO_OFFSET: u32 = 0x100;

/// Absolute address of the register containing the machine code FIFO
/// for loading the EXE
const EXE_FIFO_ADDRESS: u32 = map::EXPANSION_1.start() + EXE_FIFO_OFFSET;

/// Offset of the entry point for the loader code in the EXPANSION 1
/// memory range
const LOADER_ENTRY_OFFSET: u32 = 0x200;

/// Absolute address of the entry point for the loader code
const LOADER_ENTRY_ADDRESS: u32 = map::EXPANSION_1.start() + LOADER_ENTRY_OFFSET;
