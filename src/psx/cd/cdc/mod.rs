mod cxd1815q;
mod cxd2545q;
mod mc68hc05;

use super::Disc;
pub use mc68hc05::ROM_DUMP_SIZE as MC68HC05_ROM_DUMP_SIZE;

pub struct Cdc {
    disc: Option<Disc>,
    uc: mc68hc05::Uc,
    decoder: cxd1815q::Decoder,
    dsp: cxd2545q::Dsp,
    date: u32,
}

impl Cdc {
    pub fn new(mc68hc05_rom: &[u8; MC68HC05_ROM_DUMP_SIZE], disc: Option<Disc>) -> Cdc {
        Cdc {
            uc: mc68hc05::Uc::new(mc68hc05_rom),
            decoder: cxd1815q::Decoder::new(),
            dsp: cxd2545q::Dsp::new(),
            date: 0,
            disc,
        }
    }

    fn disc_present(&self) -> bool {
        self.disc.is_some()
    }

    /// Advance emulation by 1/44100th of a second
    pub fn run_audio_cycle(&mut self) {
        // We synchronize every module every 1/44100th of a second. It's not cycle-accurate (all
        // these chips run at several MHz) but given that most events have mechanical constraints
        // that shouldn't really matter since the average jitter is generally well beyond the
        // precision granted by this method.
        mc68hc05::run_audio_cycle(self);
        cxd2545q::run_audio_cycle(self);
        cxd1815q::run_audio_cycle(self);

        self.date += 1;

        if self.date % 44100 == 0 {
            info!("SECOND {}", self.date / 44100);
        }
    }

    /// Advance emulation by the given number of seconds
    pub fn run_seconds(&mut self, seconds: u32) {
        for _ in 0..seconds {
            for _ in 0..44100 {
                self.run_audio_cycle();
            }
        }
    }

    /// Writes coming from the host CPU (the main MIPS CPU)
    pub fn host_write(&mut self, addr: u8, v: u8) {
        cxd1815q::host_write(self, addr, v);
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
        cxd1815q::sub_cpu_write(self, addr, val);
    }

    /// Called when the microcontroller reads from CXD1815Q's sub-CPU bus (pins A0-A4, D0-D7, XCS,
    /// XRD)
    fn decoder_read(&mut self, addr: u8) -> u8 {
        cxd1815q::sub_cpu_read(self, addr)
    }

    /// Called when the controller ticks the serial clock of the CXD2545Q's serial input
    fn dsp_serial_tick(&mut self, data: bool) {
        cxd2545q::serial_tick(self, data)
    }

    /// Called when the controller latches a command sent to the CXD2545Q's serial input.
    fn dsp_serial_latch(&mut self) {
        cxd2545q::serial_latch(self)
    }

    /// Called when the controller ticks the SCLK signal of the CXD2545Q
    fn dsp_sclk_tick(&mut self) {
        cxd2545q::sclk_tick(self)
    }

    /// Called when the controller ticks the SQCK signal of the CXD2545Q
    fn dsp_sqck_tick(&mut self) {
        cxd2545q::sqck_tick(self)
    }

    /// Called when the DSP reads a new sector
    fn dsp_sector_read(&mut self) {
        cxd1815q::dsp_sector_read(self);
    }

    pub fn scan(mut self, mode: ScanMode) {
        mc68hc05::scan(&mut self, mode);
    }
}

/// Convert a microseconds to a number of 44.1kHz audio cycles with rounding
const fn us_to_audio_cycles(us: u32) -> u32 {
    let us = us as u64;

    let s = (us * 44100 + 500_000) / 1_000_000;

    debug_assert!(s > 0);

    s as u32
}

pub enum ScanMode {
    Byte(u16),
    Bit(u16, u8),
}
