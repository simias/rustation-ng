use super::Cdc;

/// CXD1815Q
pub struct Decoder {
    irq: Irq,
    host_command: u8,
    /// Set to 1 when a new command has been written to the command register, cleared when the
    /// sub-CPU sets CLRBUSY in the CLRCTL register
    command_busy: bool,
    /// Two address expansion bits
    ra: u8,
    /// Number of cycles before the next sector is read and DECINT is issued (or None if no read is
    /// pending)
    decint_delay: Option<u16>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            irq: Irq::new(),
            host_command: 0,
            command_busy: false,
            ra: 0,
            decint_delay: None,
        }
    }

    fn host_command(&mut self, cmd: u8) {
        self.host_command = cmd;
        self.command_busy = true;

        // Set "HSTCMND" IRQ
        self.irq.status |= 1 << 1;
    }

    /// Dcoder status register
    fn dests(&self) -> u8 {
        // XXX Implement RTADPBSY when streaming ADPCM. The other two bits in the register are only
        // set when the sector sync mark has been missed which is probably irrelevant for us.
        0
    }

    /// Host interface status register
    fn hifsts(&self) -> u8 {
        let mut r = 0u8;

        r |= (self.command_busy as u8) << 7;

        // XXX bit 6: result_fifo_not_full
        // XXX bit 5: result_fifo_empty
        // XXX bit 4: param_fifo_not_empty
        // XXX bit 3: DMABUSY
        // XXX bits 2-0: HINTSTS 2-0

        r
    }

    /// Clear control register
    fn clrctl(&mut self, v: u8) {
        // CLRBUSY
        if v & (1 << 6) != 0 {
            self.command_busy = false;
        }
    }
}

pub fn run_audio_cycle(cdc: &mut Cdc) {
    let decoder = &mut cdc.decoder;

    if let Some(delay) = decoder.decint_delay {
        if delay > 1 {
            decoder.decint_delay = Some(delay - 1);
        } else {
            // Sector read completed, trigger DECINT

            decoder.irq.status |= 1 << 2;
        }
    }
}

pub fn sub_cpu_write(cdc: &mut Cdc, addr: u8, val: u8) {
    let decoder = &mut cdc.decoder;

    // trace!("DECODER write: 0x{:02x} = 0x{:02x}", addr, val);

    match addr {
        0x00 => debug!("DRVIF 0x{:02x}", val),
        0x01 => debug!("CONFIG_1 0x{:02x}", val),
        0x02 => debug!("CONFIG_2 0x{:02x}", val),
        0x03 => debug!("DECCTL 0x{:02x}", val),
        0x04 => debug!("DLADR-L 0x{:02x}", val),
        0x05 => debug!("DLADR-M 0x{:02x}", val),
        0x06 => debug!("DLADR-H 0x{:02x}", val),
        0x07 => debug!("CHPCTL 0x{:02x}", val),
        0x09 => {
            if decoder.irq.mask != val {
                debug!("INTMSK 0x{:02x}", val);
                decoder.irq.mask = val;
            }
        }
        0x0a => {
            trace!("CLRCTL 0x{:02x}", val);
            decoder.clrctl(val);
        }
        // CRLINT
        0x0b => {
            trace!("CLRINT 0x{:02x}", val);
            decoder.irq.status &= !val;
        }
        0x0c => debug!("HXFR-L 0x{:02x}", val),
        0x0d => debug!("HXFR-H 0x{:02x}", val),
        0x10 => debug!("DADRC-L 0x{:02x}", val),
        0x11 => debug!("DADRC-M 0x{:02x}", val),
        0x12 => debug!("DADRC-H 0x{:02x}", val),
        0x16 => debug!("HIFCTL 0x{:02x}", val),
        0x17 => debug!("RESULT 0x{:02x}", val),
        _ => unimplemented!("sub CPU write 0x{:02x} @ 0x{:02x}", val, addr),
    }

    refresh_irq(cdc);
}

pub fn host_write(cdc: &mut Cdc, addr: u8, v: u8) {
    let decoder = &mut cdc.decoder;

    if addr == 0 {
        decoder.ra = v & 3;
        return;
    }

    match (addr << 2) | decoder.ra {
        0b01_00 => decoder.host_command(v),
        _ => unimplemented!("Host write 0x{:02x} @ {}:{}", v, addr, decoder.ra),
    }

    refresh_irq(cdc);
}

pub fn sub_cpu_read(cdc: &mut Cdc, addr: u8) -> u8 {
    let decoder = &mut cdc.decoder;

    // trace!("DECODER read 0x{:02x}", addr);
    match addr {
        0x01 => decoder.dests(),
        // INTSTS
        0x07 => decoder.irq.status,
        0x11 => decoder.hifsts(),
        0x13 => decoder.host_command,
        _ => unimplemented!("sub CPU read 0x{:02x}", addr),
    }
}

/// Called when the DSP has read a sector (this is called at the moment the DSP drives SCOR high)
pub fn dsp_sector_read(cdc: &mut Cdc) {
    let decoder = &mut cdc.decoder;

    // From the moment the DSP's SCOR pulse to the moment the DECINT IRQ triggers there's about
    // 6.87ms or ~303 audio cycles.
    //
    // XXX That's during the ToC read, needs to test with other modes (streaming/audio decoding,
    // the datasheet seems to hint that it's handled differently in this case). Also in 2x where
    // it's probably faster than that since otherwise it wouldn't be able to maintain that speed
    // (2x means one sector every 294 audio cycles).
    decoder.decint_delay = Some(303);
}

fn refresh_irq(cdc: &mut Cdc) {
    // Interrupt pin is active low
    let xint_lvl = !cdc.decoder.irq.is_active();

    cdc.uc.set_decoder_xint(xint_lvl);
}

#[derive(Copy, Clone)]
struct Irq {
    /// Active interrupts
    status: u8,
    /// Interrupt mask for the output IRQ signal
    mask: u8,
}

impl Irq {
    fn new() -> Irq {
        Irq { status: 0, mask: 0 }
    }

    fn is_active(self) -> bool {
        (self.status & self.mask) != 0
    }
}
