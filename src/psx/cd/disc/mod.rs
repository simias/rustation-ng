use super::iso9660;
use crate::error::{PsxError, Result};
use crate::psx::gpu::VideoStandard;
pub use cache::Cache as CdCache;
use cache::CachedResult;
use cdimage::{Bcd, DiscPosition, Image, Msf, Sector};
use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use std::fmt;

mod cache;

/// PlayStation disc.
///
/// XXX: add support for CD-DA? Not really useful but shouldn't be very hard either. We need to
/// support audio tracks anyway...
pub struct Disc {
    /// Disc image
    cache: CdCache,
    /// Disc serial number
    serial: SerialNumber,
}

impl Disc {
    /// Reify a disc using `image` as a backend.
    pub fn new(image: Box<dyn Image + Send>) -> Result<Disc> {
        let mut cache = CdCache::new(image);

        let serial = extract_serial_number(&mut cache)?;

        let disc = Disc { cache, serial };

        Ok(disc)
    }

    pub fn read_sector(&mut self, dp: DiscPosition) -> CachedResult<Sector> {
        self.cache.read_sector(dp)
    }

    pub fn region(&self) -> Region {
        // For now I prefer to panic to catch potential issues with the serial number handling
        // code, alternatively we could fallback on `extract_system_region`
        match self.serial.region() {
            Some(r) => r,
            None => panic!("Can't establish the region of {}", self.serial),
        }
    }

    pub fn serial_number(&self) -> SerialNumber {
        self.serial
    }
}

impl Serialize for Disc {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // We only serialize the serial number to make sure that we're reloaded with the same disc
        let mut state = serializer.serialize_struct("Disc", 1)?;
        state.serialize_field("serial", &self.serial)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Disc {
    fn deserialize<D>(_deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        todo!()
    }
}

/// Disc region
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// Japan (NTSC): SCEI
    Japan,
    /// North America (NTSC): SCEA
    NorthAmerica,
    /// Europe (PAL): SCEE
    Europe,
}

impl Region {
    /// Returns the video standard normally used for the given region
    pub fn video_standard(self) -> VideoStandard {
        match self {
            Region::Japan => VideoStandard::Ntsc,
            Region::NorthAmerica => VideoStandard::Ntsc,
            Region::Europe => VideoStandard::Pal,
        }
    }
}

/// Disc serial number
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq)]
pub struct SerialNumber([u8; 10]);

impl SerialNumber {
    /// Extract a serial number from a standard PlayStation binary
    /// name of the form "aaaa_ddd.dd"
    fn from_bin_name(bin: &[u8]) -> Result<SerialNumber> {
        if bin.len() != 11 {
            return Err(PsxError::NoSerialNumber);
        }

        if bin[4] != b'_' {
            // This will fail for the few "lightspan educational" discs since they have a serial
            // number looking like "LSP-123456". Those games are fairly obscure and some of them
            // seem to have weird and nonstandards SYSTEM.CNF anyway.
            return Err(PsxError::NoSerialNumber);
        }

        let mut serial = [0u8; 10];

        fn to_upper(b: u8) -> u8 {
            if b.is_ascii_lowercase() {
                b - b'a' + b'A'
            } else {
                b
            }
        }

        serial[0] = to_upper(bin[0]);
        serial[1] = to_upper(bin[1]);
        serial[2] = to_upper(bin[2]);
        serial[3] = to_upper(bin[3]);
        serial[4] = b'-';
        serial[5] = bin[5];
        serial[6] = bin[6];
        serial[7] = bin[7];
        serial[8] = bin[9];
        serial[9] = bin[10];

        Ok(SerialNumber(serial))
    }

    pub fn region(&self) -> Option<Region> {
        match &self.0[0..4] {
            b"SCPS" | b"SLPS" | b"SLPM" | b"PAPX" => Some(Region::Japan),
            b"SCUS" | b"SLUS" | b"LSP-" => Some(Region::NorthAmerica),
            b"SCES" | b"SCED" | b"SLES" | b"SLED" => Some(Region::Europe),
            _ => None,
        }
    }
}

impl fmt::Display for SerialNumber {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.0))
    }
}

/// Attempt to discover the region of the disc using the license string stored in the system area
/// of the official PlayStation ISO filesystem.
#[allow(dead_code)]
pub fn extract_system_region(image: &mut dyn Image) -> Result<Region> {
    // In order to identify the type of disc we're going to use sector 00:00:04 from Track01 which
    // should contain the "Licensed by..."  string.
    let toc = image.toc();
    let track = toc.track(Bcd::ONE)?;
    let msf = track.disc_position(Msf::from_bcd(0x00, 0x00, 0x04).unwrap())?;

    let sector = image.read_sector(msf)?;

    // On the discs I've tried we always have an ASCII license string in the first 76 data bytes.
    // We'll see if it holds true for all the discs out there...
    let license_blob = &sector.mode2_xa_payload()?[0..76];

    // There are spaces everywhere in the license string (including in the middle of some words),
    // let's clean it up and convert to a canonical string
    let license: String = license_blob
        .iter()
        .filter_map(|&b| match b {
            b'A'..=b'z' => Some(b as char),
            _ => None,
        })
        .collect();

    let region = match license.as_ref() {
        "LicensedbySonyComputerEntertainmentInc" => Region::Japan,
        "LicensedbySonyComputerEntertainmentAmerica" => Region::NorthAmerica,
        "LicensedbySonyComputerEntertainmentofAmerica" => Region::NorthAmerica,
        "LicensedbySonyComputerEntertainmentEurope" => Region::Europe,
        _ => {
            let m = format!("Couldn't identify disc region string: {}", license);
            return Err(PsxError::BadDiscFormat(m));
        }
    };

    Ok(region)
}

/// Attempt to extract the serial number of the disc. All officially
/// licensed PlayStation game should have a serial number.
fn extract_serial_number(image: &mut CdCache) -> Result<SerialNumber> {
    let system_cnf = read_system_cnf(image)?;

    // Now we need to parse the SYSTEM.CNF file to get the content of the "BOOT" line
    let mut boot_path = None;

    for line in system_cnf.split(|&b| b == b'\n') {
        let words: Vec<_> = line
            .split(|&b| b == b' ' || b == b'\t' || b == b'=')
            .filter(|w| !w.is_empty())
            .collect();

        if words.len() == 2 && words[0] == b"BOOT" {
            boot_path = Some(words[1]);
            break;
        }
    }

    let boot_path = match boot_path {
        Some(b) => b,
        None => {
            warn!("Couldn't find BOOT line in SYSTEM.CNF");
            return Err(PsxError::NoSerialNumber);
        }
    };

    // boot_path should look like "cdrom:\FOO\BAR\...\aaaa_ddd.dd;1"
    let path: Vec<_> = boot_path
        .split(|&b| b == b':' || b == b';' || b == b'\\')
        .collect();

    if path.len() < 2 {
        warn!(
            "Unexpected boot path: {}",
            String::from_utf8_lossy(boot_path)
        );
        return Err(PsxError::NoSerialNumber);
    }

    let bin_name = path[path.len() - 2];

    let serial = SerialNumber::from_bin_name(bin_name);

    if serial.is_err() {
        warn!("Unexpected bin name: {}", String::from_utf8_lossy(bin_name));
    }

    serial
}

fn read_system_cnf(image: &mut CdCache) -> Result<Vec<u8>> {
    let dir = iso9660::open_image(image)?;

    let system_cnf = dir.entry_by_name(b"SYSTEM.CNF;1")?;

    // SYSTEM.CNF should be a small text file, 1MB should be way more
    // than necessary
    let len = system_cnf.extent_len();

    if len > 1024 * 1024 {
        let desc = format!("SYSTEM.CNF is too big: {}B", len);
        return Err(PsxError::BadDiscFormat(desc));
    }

    let system_cnf = system_cnf.read_file(image)?;

    Ok(system_cnf)
}
