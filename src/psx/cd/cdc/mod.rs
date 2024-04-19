#[macro_use]
mod debug;
mod decoder;
mod dsp;
mod resampler;
mod uc;

use super::Disc;
use crate::PsxError;
use cdimage::{DiscPosition, Sector};
pub use uc::ROM_DUMP_SIZE as MC68HC05_ROM_DUMP_SIZE;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Cdc {
    uc: uc::Uc,
    decoder: decoder::Decoder,
    dsp: dsp::Dsp,
    disc: Option<Disc>,
    loading_speed: u8,
    /// Number of audio cycles used to delay the shell closing event when switching disc. It's important to wait
    /// a little to make sure that the controller has the time to register the disc switch.
    #[serde(default)]
    shell_close_delay: Option<u32>,
    #[serde(default)]
    effective_speed: u8,
}

impl Cdc {
    pub fn new(mc68hc05_rom: &[u8; MC68HC05_ROM_DUMP_SIZE], disc: Option<Disc>) -> Cdc {
        let region = disc.as_ref().map(|d| d.region());

        let mut cdc = Cdc {
            uc: uc::Uc::new(mc68hc05_rom),
            decoder: decoder::Decoder::new(),
            dsp: dsp::Dsp::new(region),
            disc,
            loading_speed: 2,
            shell_close_delay: None,
            effective_speed: 1,
        };

        // I start with the sled at some location on the disc to simulate the case where the
        // console was shutdown mid-game.
        //
        // In practice it's irrelevant: the first thing the firmware should do is bring the sled
        // back to the center (polling LMSW) but this way I make sure that this is emulated
        // correctly and we won't freeze or crash if a game decides to reset the drive later.
        let position = "+05:00:00".parse().expect("Bad starting position");
        dsp::set_position(&mut cdc, position);

        cdc.set_shell_open(cdc.disc.is_none());

        cdc
    }

    /// Remove the disc and emulate an open tray
    pub fn take_disc(&mut self) -> Option<Disc> {
        self.set_shell_open(true);
        self.dsp.disc_removed();

        self.disc.take()
    }

    /// Attempt to load a disc. If a `disc` is `Some` and disc is already loaded and its serial
    /// number is different from the new one, generate an error (used for savestate loading).
    ///
    /// This function is meant as a helper for savestate loading.
    ///
    /// If you want to emulate disc switching you should call `load_disc` instead so that the shell
    /// open/shell close sequence timings are handled correctly.
    #[allow(clippy::result_large_err)]
    pub fn set_disc(&mut self, disc: Option<Disc>) -> Result<(), (PsxError, Option<Disc>)> {
        if let (Some(old_disc), Some(new_disc)) = (&self.disc, &disc) {
            if old_disc.serial_number() != new_disc.serial_number() {
                return Err((
                    PsxError::BadSerialNumber {
                        expected: old_disc.serial_number(),
                        got: new_disc.serial_number(),
                    },
                    disc,
                ));
            }
        };

        self.disc = disc;

        if self.shell_close_delay.is_some() {
            // We were in the middle of a disc switch, wait until we close the shell
            self.set_shell_open(true);
            if self.disc.is_none() {
                self.shell_close_delay = None;
            }
        } else {
            self.set_shell_open(self.disc.is_none());
        }

        Ok(())
    }

    pub fn disc_present(&self) -> bool {
        self.disc.is_some()
    }

    pub fn load_disc(&mut self, disc: Disc) {
        // Make sure any previous disc is gone
        self.take_disc();

        self.disc = Some(disc);
        // Wait half a second before closing the shell
        self.shell_close_delay = Some(us_to_audio_cycles(500_000));
    }

    pub fn set_cd_loading_speed(&mut self, loading_speed: u8) {
        self.loading_speed = loading_speed
    }

    /// Advance emulation by 1/44100th of a second
    pub fn run_audio_cycle(&mut self, allow_overclock: bool) -> [i16; 2] {
        // We synchronize every module every 1/44100th of a second. It's not cycle-accurate (all
        // these chips run at several MHz) but given that most events have mechanical constraints
        // that shouldn't really matter since the average jitter is generally well beyond the
        // precision granted by this method.

        let cycles_to_run = if self.loading_speed > 1 {
            // Attempt to increase the loading speed if it's safe to do so
            let can_overclock = allow_overclock
                && self.decoder.is_double_speed()
                && !self.decoder.is_streaming_audio();

            if can_overclock {
                self.loading_speed
            } else {
                // Default to the native speed
                1
            }
        } else {
            // No overclocking requested
            1
        };

        self.effective_speed = cycles_to_run;
        if self.decoder.is_double_speed() {
            self.effective_speed *= 2;
        }

        match self.shell_close_delay {
            None => (),
            Some(c) if c <= u32::from(cycles_to_run) => {
                info!("Closing CD shell");
                self.set_shell_open(self.disc.is_none());
                self.shell_close_delay = None;
            }
            Some(ref mut c) => *c -= u32::from(cycles_to_run),
        }

        for _ in 0..cycles_to_run {
            uc::run_audio_cycle(self);
            dsp::run_audio_cycle(self);
            decoder::run_audio_cycle(self);
        }

        decoder::get_audio_sample(self)
    }

    /// Writes coming from the host CPU (the main MIPS CPU)
    pub fn host_write(&mut self, addr: u8, v: u8) {
        decoder::host_write(self, addr, v);
    }

    /// Reads coming from the host CPU (the main MIPS CPU)
    pub fn host_read(&mut self, addr: u8) -> u8 {
        decoder::host_read(self, addr)
    }

    /// DMA (sector data) reads coming from the host CPU (the main MIPS CPU)
    pub fn host_dma_read(&mut self) -> u8 {
        decoder::host_dma_read(self)
    }

    /// Returns true if one of the host interrupts is currently active
    pub fn irq_active(&self) -> bool {
        self.decoder.host_irq_active()
    }

    pub fn set_debug(&mut self, debug: bool) {
        self.uc.set_debug(debug);
    }

    pub fn set_shell_open(&mut self, opened: bool) {
        self.uc.set_shell_open(opened);
    }

    /// Called when the microcontroller writes to the CXD1815Q's sub-CPU bus (pins A0-A4, D0-D7,
    /// XCS, XWR)
    fn decoder_write(&mut self, addr: u8, val: u8) {
        decoder::sub_cpu_write(self, addr, val);
    }

    /// Called when the microcontroller reads from CXD1815Q's sub-CPU bus (pins A0-A4, D0-D7, XCS,
    /// XRD)
    fn decoder_read(&mut self, addr: u8) -> u8 {
        decoder::sub_cpu_read(self, addr)
    }

    /// Called when the controller ticks the serial clock of the CXD2545Q's serial input
    fn dsp_serial_tick(&mut self, data: bool) {
        dsp::serial_tick(self, data)
    }

    /// Called when the controller latches a command sent to the CXD2545Q's serial input.
    fn dsp_serial_latch(&mut self) {
        dsp::serial_latch(self)
    }

    /// Called when the controller ticks the SCLK signal of the CXD2545Q
    fn dsp_sclk_tick(&mut self) {
        dsp::sclk_tick(self)
    }

    /// Called when the controller ticks the SQCK signal of the CXD2545Q
    fn dsp_sqck_tick(&mut self) {
        dsp::sqck_tick(self)
    }

    /// Called when the DSP reads a new sector
    fn dsp_sector_read(&mut self, sector: Sector) {
        decoder::dsp_sector_read(self, sector);
    }

    /// Copy the ROM from another CD controller instance
    pub fn copy_rom(&mut self, source: &Cdc) {
        self.uc.copy_rom(&source.uc)
    }

    pub fn state(&self) -> CdcState {
        if self.uc.is_shell_open() {
            CdcState::ShellOpen
        } else if self.disc.is_none() {
            CdcState::NoDisc
        } else if self.uc.is_seeking() {
            CdcState::Seeking
        } else if self.uc.is_playing_audio() {
            CdcState::AudioStreaming
        } else if self.uc.is_reading_data() {
            CdcState::DataStreaming
        } else {
            CdcState::Idle
        }
    }

    /// Returns the disc speed (1 for standard CD-DA speed)
    pub fn disc_speed(&self) -> u8 {
        if self.uc.is_shell_open() || self.disc.is_none() {
            0
        } else {
            self.effective_speed
        }
    }

    /// Current sled position on the disc
    pub fn position(&self) -> DiscPosition {
        self.dsp.position()
    }
}

/// Convert a delay in microseconds into a number of 44.1kHz audio cycles with rounding
const fn us_to_audio_cycles(us: u32) -> u32 {
    let us = us as u64;

    let s = (us * 44100 + 500_000) / 1_000_000;

    debug_assert!(s > 0);

    s as u32
}

/// Possible states for the CD drive
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CdcState {
    /// Shell is open
    ShellOpen,
    /// No disc loaded
    NoDisc,
    /// Disc is loaded but not actively in-use
    Idle,
    /// A seek is in progress
    Seeking,
    /// Disc is streaming audio
    AudioStreaming,
    /// Disc is streaming data
    DataStreaming,
}

impl CdcState {
    pub fn is_idle(self) -> bool {
        matches!(self, CdcState::Idle)
    }
}
