//! Handling of Memory Card saves on disc

use crate::box_array::BoxArray;
use crate::libretro;
use crate::psx::pad_memcard::devices::memory_card::{MemoryCard, FLASH_SIZE};
use crate::psx::pad_memcard::devices::DeviceInterface;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Structure holding the state of the Memory Card image on disc in order to keep it in sync with
/// the emulated one.
pub struct MemoryCardFile {
    /// Path to the Memory Card image
    file_path: PathBuf,
    /// Counter used to figure out if we need to flush the MemoryCard to the disc yet. Contains
    /// "None" if no writes have been detected since the last flush.
    write_pending_since: Option<u8>,
    /// Last write counter received from the memory card. Used to detect writes.
    last_write_counter: u32,
}

impl MemoryCardFile {
    /// Attempt to load a Memory Card image from `file_path`. If the file does not exist a freshly
    /// formatted Memory Card image will be created instead.
    ///
    /// This function will return an error if `file_path` contains an unknown or unsupported file
    /// format in order to avoid data loss.
    pub fn load_or_create(file_path: &Path) -> io::Result<(MemoryCardFile, MemoryCard)> {
        let mut mcf = MemoryCardFile {
            file_path: file_path.into(),
            write_pending_since: None,
            last_write_counter: 0,
        };

        let mut file = match File::open(file_path) {
            Ok(f) => f,
            Err(e) => {
                if e.kind() == io::ErrorKind::NotFound {
                    // All is good, it just means that the file doesn't exist yet, we can start
                    // with an fresh memory card and save it when we need to
                    info!(
                        "Memory Card file '{}' doesn't appear to exist, using an empty image",
                        file_path.display()
                    );
                    return Ok((mcf, MemoryCard::new_formatted()));
                } else {
                    // We can't seem to access this file
                    return Err(e);
                }
            }
        };

        let metadata = file.metadata()?;

        if !metadata.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Not a file!"));
        }

        if metadata.len() != FLASH_SIZE as u64 {
            let msg = format!(
                "Invalid file size (expected {}B MCR file, got {}B instead)",
                FLASH_SIZE,
                metadata.len()
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
        }

        let mut memory = BoxArray::from_vec(vec![0; FLASH_SIZE]);

        file.read_exact(&mut *memory)?;

        let card = MemoryCard::new_with_memory(memory);

        // Let's add one more test to see if this looks like a proper memory card image
        if !card.is_format_valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported or broken memory card format",
            ));
        }

        mcf.last_write_counter = card.write_counter();

        Ok((mcf, card))
    }

    /// Allocates a dummy MemoryCardFile that won't do anything
    pub fn dummy() -> MemoryCardFile {
        MemoryCardFile {
            file_path: PathBuf::new(),
            write_pending_since: None,
            last_write_counter: 0,
        }
    }

    /// Return the path of the underlying file used to store the Memory Card image
    pub fn path(&self) -> &Path {
        &self.file_path
    }

    /// Check if the memory card contents need to be backed up. Should be called once per frame.
    pub fn maybe_dump(&mut self, mc: &dyn DeviceInterface) {
        let new_write_counter = mc.write_counter();

        let new_write = new_write_counter != self.last_write_counter;

        if new_write {
            self.write_pending_since = Some(0);
            self.last_write_counter = new_write_counter;
        } else {
            // No write since last time
            self.write_pending_since = self.write_pending_since.map(|n| n + 1);
        }

        // XXX this algorithm has one potential weakness: if some game writes *continuously* to the
        // memory card without a break of at least `WRITE_FLUSH_FRAME` frames, we'll never flush
        // the Memory Card to disk.
        //
        // For old-school consoles that use battery-backed RAM for saves (like the Game Boy for
        // instance) that's a genuine problem because some games use the region as a RAM extension
        // and write to it continuously, so you have to force a flush from time to time.
        //
        // PlayStation Memory cards can't really be used that way however, they're super slow to
        // access and would probably deteriorate fairly quick if they were written all the time, so
        // it's unlikely to be a problem.
        //
        // Besides, we should call `force_dump` when we're done with the card, so as long as we
        // don't crash we should be good.
        if let Some(last_write) = self.write_pending_since {
            if last_write >= WRITE_FLUSH_FRAME {
                // We have a write pending and we haven't gotten new writes in `WRITE_FLUSH_FRAME`
                // frames, commit to disk
                self.dump(mc);
            }
        }
    }

    /// Like `maybe_dump` but never postpone a Memory Card dump if one is pending. Can be used
    /// before quitting the emulator or changing memory cards.
    pub fn force_dump(&mut self, mc: &dyn DeviceInterface) {
        self.maybe_dump(mc);

        if self.write_pending_since.is_some() {
            // Still dirty, force dump
            self.dump(mc);
        }
    }

    /// Dump the memory card to disk if a write is pending
    fn dump(&mut self, mc: &dyn DeviceInterface) {
        let memory = match mc.get_memory() {
            Some(m) => m,
            // That shouldn't happen, probably?
            None => {
                warn!("Attempting to flush a Memory Card without memory...");
                return;
            }
        };

        // XXX Should we implement some sort of memory card history to allow the user to undo
        // mistakes?
        if self.file_path.as_os_str().is_empty() {
            // This is a dummy writer. We probably shouldn't end up here.
            warn!("Attempt to dump to a dummy Memory Card file");
            return;
        }

        if let Err(e) = File::create(&self.file_path).and_then(|mut file| file.write_all(memory)) {
            // This is bad, we can't open the memory card file
            error!(
                "Can't open memory card file '{}' for writing: {}",
                self.file_path.display(),
                e
            );
            // Write a message on screen, the user probably wants to know if their progress
            // can't be saved...
            libretro::set_message(3000, "Can't save memory card to disk!");
        }

        info!("Memory Card flushed to '{}'", self.file_path.display());
        self.write_pending_since = None;
    }
}

/// How many frames do we wait after writes to a Memory Card have stopped before we flush the new
/// contents to disk.
///
/// This value has two purposes: it prevents many spurious writes to disk which may be damaging for
/// the hardware and it avoids writing incomplete saves to disk, avoiding corruption if the
/// emulator crashes (or is quitted) mid-save.
const WRITE_FLUSH_FRAME: u8 = 60;
