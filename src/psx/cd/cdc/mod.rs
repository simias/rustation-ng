mod decoder;
mod dsp;
mod resampler;
mod uc;

use super::Disc;
use cdimage::Sector;
pub use uc::ROM_DUMP_SIZE as MC68HC05_ROM_DUMP_SIZE;

pub struct Cdc {
    uc: uc::Uc,
    decoder: decoder::Decoder,
    dsp: dsp::Dsp,
    disc: Option<Disc>,
}

impl Cdc {
    pub fn new(mc68hc05_rom: &[u8; MC68HC05_ROM_DUMP_SIZE], disc: Option<Disc>) -> Cdc {
        let region = disc.as_ref().map(|d| d.region());

        let mut cdc = Cdc {
            uc: uc::Uc::new(mc68hc05_rom),
            decoder: decoder::Decoder::new(),
            dsp: dsp::Dsp::new(region),
            disc,
        };

        // I start with the sled at some location on the disc to simulate the case where the
        // console was shutdown mid-game.
        //
        // In practice it's irrelevant: the first thing the firmware should do is bring the sled
        // back to the center (polling LMSW) but this way I make sure that this is emulated
        // correctly and we won't freeze or crash if a game decides to reset the drive later.
        let position = "+05:00:00".parse().expect("Bad starting position");
        dsp::set_position(&mut cdc, position);

        cdc
    }

    fn disc_present(&self) -> bool {
        self.disc.is_some()
    }

    /// Advance emulation by 1/44100th of a second
    pub fn run_audio_cycle(&mut self) -> [i16; 2] {
        // We synchronize every module every 1/44100th of a second. It's not cycle-accurate (all
        // these chips run at several MHz) but given that most events have mechanical constraints
        // that shouldn't really matter since the average jitter is generally well beyond the
        // precision granted by this method.
        uc::run_audio_cycle(self);
        dsp::run_audio_cycle(self);

        decoder::run_audio_cycle(self)
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
}

/// Convert a microseconds to a number of 44.1kHz audio cycles with rounding
const fn us_to_audio_cycles(us: u32) -> u32 {
    let us = us as u64;

    let s = (us * 44100 + 500_000) / 1_000_000;

    debug_assert!(s > 0);

    s as u32
}
