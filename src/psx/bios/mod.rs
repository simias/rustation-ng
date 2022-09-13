mod db;

use crate::error::{PsxError, Result};
pub use db::Metadata;

pub struct Bios {
    rom: Box<[u8; BIOS_SIZE]>,
    metadata: &'static Metadata,
}

impl Bios {
    /// Create a BIOS image from `binary` and attempt to match it with an entry in the database. If
    /// no match can be found return an error.
    pub fn new(binary: Box<[u8; BIOS_SIZE]>) -> Result<Bios> {
        match db::lookup_blob(&*binary) {
            Some(metadata) => Ok(Bios {
                rom: binary,
                metadata,
            }),
            None => Err(PsxError::UnknownBios),
        }
    }

    /// Return a static pointer to the BIOS's Metadata
    pub fn metadata(&self) -> &'static Metadata {
        self.metadata
    }

    /// Creates a BIOS instance with content set to all 0s.
    #[allow(dead_code)]
    pub fn new_dummy() -> Bios {
        let rom = box_array![0; BIOS_SIZE];

        Bios {
            rom,
            metadata: &db::DATABASE[0],
        }
    }

    /// Return the raw BIOS ROM
    pub fn get_rom(&self) -> &[u8; BIOS_SIZE] {
        &self.rom
    }
}

/// BIOS images are always 512KB in length
pub const BIOS_SIZE: usize = 512 * 1024;
