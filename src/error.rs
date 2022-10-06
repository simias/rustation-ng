use crate::psx::cd::disc::SerialNumber;
use crate::psx::iso9660;
use cdimage::CdError;
use std::io;
use thiserror::Error;

pub type Result<T> = ::std::result::Result<T, PsxError>;

#[derive(Error, Debug)]
pub enum PsxError {
    #[error("Input output error: {0}")]
    IoError(#[from] io::Error),
    #[error("We couldn't find the disc's serial number")]
    NoSerialNumber,
    #[error("Bad CD serial number: expected {expected} got {got}")]
    BadSerialNumber {
        expected: SerialNumber,
        got: SerialNumber,
    },
    #[error("The provided BIOS is unknown")]
    UnknownBios,
    #[error("We couldn't find a suitable BIOS")]
    NoBiosFound,
    #[error("Invalid BIOS file `{0}`")]
    BadBios(String),
    #[error("Something went wrong while communicating with the frontend: `{0}`")]
    FrontendError(String),
    #[error("CD layer error: {0}")]
    CdError(#[from] CdError),
    #[error("The disc format was incorrect (i.e. probably not a valid PSX disc image): `{0}`")]
    BadDiscFormat(String),
    #[error("CD ISO filesystem error: `{0}`")]
    IsoError(#[from] iso9660::IsoError),
    #[error("Invalid or unknown CDC firmware")]
    BadCdcFirmware,
    #[error("We couldn't find a suitable CDC firmware image")]
    NoCdcFirmwareFound,
    #[error("Deserialization error: {0}")]
    DeserializationError(String),
}
