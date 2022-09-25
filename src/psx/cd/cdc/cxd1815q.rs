use super::Cdc;
use cdimage::Sector;
use std::fmt;

/// CXD1815Q
pub struct Decoder {
    state: DecoderState,
    irq: IrqController,
    host_command: u8,
    /// Set to 1 when a new command has been written to the command register, cleared when the
    /// sub-CPU sets CLRBUSY in the CLRCTL register
    command_busy: bool,
    /// Two address expansion bits
    ra: u8,
    /// Value of the decoder control register
    decctl: DecCtl,
    /// Decoder current address
    dadrc: u16,
    /// Decoder last address
    dladr: u16,
    /// Writing to the RAM buffer is stopped
    write_stopped: bool,
    /// Counter for the DECTOUT IRQ
    dectout_sectors: u8,
    /// For HDR register: contains the 4 CD-ROM header bytes
    hdr: HdrBuf,
    /// For SHDR register: contains the 4 CD-ROM XA subheader bytes
    shdr: HdrBuf,
    /// ECC status register
    eccsts: u8,
    /// RAM used to store decoded sectors
    ram: [u8; 32 * 1024],
    /// Host command parameter FIFO
    host_params: HostFifo,
    /// Host command response FIFO
    host_result: HostFifo,
    /// Host interrupt status
    hintsts: u8,
    /// Host interrupt mask
    hintmsk: u8,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            state: DecoderState::Idle,
            irq: IrqController::new(),
            host_command: 0,
            command_busy: false,
            ra: 0,
            decctl: DecCtl(0),
            dladr: 0,
            dadrc: 0,
            write_stopped: false,
            dectout_sectors: 0,
            hdr: HdrBuf::new(),
            shdr: HdrBuf::new(),
            eccsts: 0,
            ram: [0; 32 * 1024],
            host_params: HostFifo::new(),
            host_result: HostFifo::new(),
            hintsts: 0,
            hintmsk: 0,
        }
    }

    fn host_command(&mut self, cmd: u8) {
        self.host_command = cmd;
        self.command_busy = true;

        debug!("Host command 0x{:02x} {:?}", cmd, self.host_params);

        self.irq.trigger(Irq::HstCmnd);
    }

    /// ECC status register
    fn eccsts(&self) -> u8 {
        self.eccsts
    }

    /// Decoder status register
    fn dests(&self) -> u8 {
        // XXX Implement RTADPBSY when streaming ADPCM. The other two bits in the register are only
        // set when the sector sync mark has been missed which is probably irrelevant for us.
        0
    }

    /// Host interface status register
    fn hifsts(&self) -> u8 {
        let mut r = 0u8;

        r |= self.hintsts & 7;
        // XXX bit 3: DMABUSY
        r |= ((!self.host_params.is_empty()) as u8) << 4;
        r |= (self.host_result.is_empty() as u8) << 5;
        r |= ((!self.host_result.is_full()) as u8) << 6;
        r |= (self.command_busy as u8) << 7;

        r
    }

    /// Clear control register
    fn clrctl(&mut self, v: u8) {
        // CLRBUSY
        if v & (1 << 6) != 0 {
            if self.command_busy {
                debug!("Host command done, result: {:?}", self.host_result);
            }
            self.command_busy = false;
        }

        // CLRRSLT
        if v & (1 << 5) != 0 {
            self.host_result.clear();
        }
    }

    /// Host status register
    pub fn hsts(&self) -> u8 {
        let mut r = 0;

        r |= self.ra & 3;
        // TODO: ADPCM busy (ADPBUSY)
        r |= (self.host_params.is_empty() as u8) << 3;
        r |= ((!self.host_params.is_full()) as u8) << 4;
        r |= (!self.host_result.is_empty() as u8) << 5;
        // TODO: DRQSTS
        r |= (self.command_busy as u8) << 7;

        r
    }

    /// Returns true if at least one unmasked host interrupt is active
    pub fn host_irq_active(&self) -> bool {
        self.hintsts & self.hintmsk != 0
    }

    pub fn push_param(&mut self, param: u8) {
        if self.host_params.is_full() {
            warn!("Decoder param FIFO overflow!");
        }

        self.host_params.push(param);
    }

    pub fn push_result(&mut self, r: u8) {
        if self.host_result.is_full() {
            warn!("Decoder result FIFO overflow!");
        }

        self.host_result.push(r);
    }
}

pub fn run_audio_cycle(cdc: &mut Cdc) {
    match cdc.decoder.state {
        DecoderState::Idle => (),
        DecoderState::SectorPending {
            ref sector,
            ref mut decint_delay,
        } => {
            let mode = cdc.decoder.decctl.mode();

            if *decint_delay > 1 {
                *decint_delay -= 1;
            } else {
                // Sector read completed, trigger DECINT if necessary
                if mode.decint() {
                    cdc.decoder.irq.trigger(Irq::DecInt);
                }

                let raw_data = sector.data_2352();

                // I *think* that HDR and SDHR are updated even if we don't write to RAM, but I
                // haven't verified. I also *think* that they're updated regardless of the mode
                // (even though it doesn't make sense to set them if we're in CD-DA mode)
                cdc.decoder.hdr.reset(array_ref![raw_data, 12, 4]);
                cdc.decoder.shdr.reset(array_ref![raw_data, 16, 4]);

                // Load the sector in RAM. Normally this should certainly not happen all at once
                // but it's good enough for now.
                if mode.write_to_ram() {
                    let mut addr = cdc.decoder.dadrc;

                    // Round the current address to the next KB. I don't actually know if this is
                    // accurate or if it must be done by the sub-CPU. In order for CMADR to work
                    // the sector has to be stored at a kilobyte boundary
                    addr = addr.wrapping_add(0x3ff) & 0x7c00;

                    if matches!(
                        mode,
                        DecoderMode::MonitorOnly
                            | DecoderMode::WriteOnly
                            | DecoderMode::RealTimeCorrection
                    ) {
                        // Check for sync field
                        let has_sync = raw_data[0] == 0
                            && raw_data[12] == 0
                            && raw_data[1..12].iter().all(|&b| b == 0xff);

                        if has_sync {
                            cdc.decoder.dectout_sectors = 0;

                            cdc.decoder.eccsts = 0;

                            let ecc_start;

                            // Update ECCSTS mode
                            cdc.decoder.eccsts |= match (
                                cdc.decoder.decctl.mode_form_sel(),
                                raw_data[15],
                                raw_data[18] & (1 << 5) != 0,
                            ) {
                                // AUTODIST is not set, use the configured value
                                //
                                // XXX We should probably test if the configured value matches the
                                // actual sector format, otherwise things will obviously go badly.
                                // In practice I believe that the BIOS only uses AUTODIST so it
                                // shouldn't matter.
                                (Some(_mf), _mode, _form) => {
                                    unimplemented!("sector read without autodist")
                                }
                                // Mode 1
                                (None, 1, _form) => {
                                    ecc_start = 2064;
                                    0b00
                                }
                                // Mode 2 Form 1
                                (None, 2, false) => {
                                    ecc_start = 2072;
                                    0b10
                                }
                                // Mode 2 Form 2
                                (None, 2, true) => {
                                    ecc_start = 2348;
                                    0b11
                                }
                                (None, m, _form) => {
                                    // Probably means that it's either not a CD-ROM sector, or it's
                                    // corrupted. It could also be Mode 0 which is a CD-ROM sector
                                    // that contains no data. According to the datasheet in this
                                    // mode the correction is inhibited.
                                    warn!("Unhandled sector mode {}", m);
                                    ecc_start = 2064;

                                    // Set CORINH
                                    cdc.decoder.eccsts |= 1 << 5;

                                    // Not sure what value is used here
                                    0b00
                                }
                            };

                            // Update EDCALL0
                            if raw_data[ecc_start]
                                | raw_data[ecc_start + 1]
                                | raw_data[ecc_start + 2]
                                | raw_data[ecc_start + 3]
                                == 0
                            {
                                cdc.decoder.eccsts |= 1 << 7;
                            }

                            if sector.edc_valid() {
                                // Set EDCOK
                                cdc.decoder.eccsts |= 1 << 3;
                                // Set ECCOK
                                cdc.decoder.eccsts |= 1 << 2;
                            } else {
                                unimplemented!("Read sector with EDC errors");
                            }
                        } else {
                            // We get the IRQ if the sync is not detected after 3 sectors
                            if cdc.decoder.dectout_sectors >= 2 {
                                cdc.decoder.irq.trigger(Irq::DrvOvrn);
                            } else {
                                cdc.decoder.dectout_sectors += 1;
                            }
                        }
                    }

                    // XXX Pure guesswork For CD-DA. In other modes we skip the sync bytes.
                    let sector_off = if mode == DecoderMode::CdDa { 0 } else { 12 };

                    for &b in &raw_data[sector_off..] {
                        if cdc.decoder.write_stopped {
                            break;
                        }

                        cdc.decoder.ram[usize::from(addr)] = b;

                        // Apparently we stop *after* we've written to dladr
                        if addr == cdc.decoder.dladr {
                            if cdc.decoder.decctl.endladr() {
                                cdc.decoder.write_stopped = true;
                                cdc.decoder.irq.trigger(Irq::DrvOvrn);
                            } else if mode == DecoderMode::CdDa {
                                // The datasheet says that DRVOVRN is also set when DADRC == DLADR in
                                // CD-DA mode *even* if ENDLADR is not set
                                cdc.decoder.irq.trigger(Irq::DrvOvrn);
                            }
                        }

                        addr = (addr + 1) & 0x7fff;
                    }

                    cdc.decoder.dadrc = addr;
                }
                cdc.decoder.state = DecoderState::Idle;

                refresh_irq(cdc);
            }
        }
    }
}

pub fn sub_cpu_write(cdc: &mut Cdc, addr: u8, val: u8) {
    let decoder = &mut cdc.decoder;

    // trace!("DECODER write: 0x{:02x} = 0x{:02x}", addr, val);

    match addr {
        0x00 => debug!("DRVIF 0x{:02x}", val),
        0x01 => {
            // The PlayStation normally uses 32Kx8b RAM config
            assert_eq!((val >> 2) & 7, 0, "Unexpected RAM config");
            debug!("CONFIG_1 0x{:02x}", val);
        }
        0x02 => debug!("CONFIG_2 0x{:02x}", val),
        0x03 => {
            decoder.decctl = DecCtl(val);
            decoder.dectout_sectors = 0;

            debug!("DECCTL 0x{:02x} {:?}", val, decoder.decctl.mode());
        }
        0x04 => {
            debug!("DLADR-L 0x{:02x}", val);
            decoder.dladr &= 0xff00;
            decoder.dladr |= u16::from(val);
            // XXX guess
            decoder.write_stopped = false;
        }
        0x05 => {
            debug!("DLADR-M 0x{:02x}", val);
            decoder.dladr &= 0xff;
            decoder.dladr |= u16::from(val) << 8;
            // XXX guess
            decoder.write_stopped = false;
        }
        0x06 => {
            debug!("DLADR-H 0x{:02x}", val);
            // We have a 32K RAM, DLADR-H should always be zero
            assert_eq!(val, 0, "Unexpected DLADR-H");
            // XXX guess
            decoder.write_stopped = false;
        }
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
        0x0c => {
            if val != 0 {
                todo!("HXFR-L 0x{:02x}", val);
            }
        }
        0x0d => {
            if val != 0x40 {
                todo!("HXFR-H 0x{:02x}", val);
            }
        }
        0x10 => {
            debug!("DADRC-L 0x{:02x}", val);
            decoder.dadrc &= 0xff00;
            decoder.dadrc |= u16::from(val);
            // XXX guess
            decoder.write_stopped = false;
        }
        0x11 => {
            debug!("DADRC-M 0x{:02x}", val);
            decoder.dadrc &= 0xff;
            decoder.dadrc |= u16::from(val) << 8;
            // XXX guess
            decoder.write_stopped = false;
        }
        0x12 => {
            debug!("DADRC-H 0x{:02x}", val);
            // We have a 32K RAM, DADRC-H should always be zero
            assert_eq!(val, 0, "Unexpected DLADR-H");
            // XXX guess
            decoder.write_stopped = false;
        }
        // HIFCTL
        0x16 => {
            decoder.hintsts &= !7;
            decoder.hintsts |= val & 7;

            if val & 7 != 0 {
                debug!(
                    "Triggering host IRQ {}, result: {:?}",
                    val & 7,
                    decoder.host_result
                );
            }
        }
        0x17 => decoder.push_result(val),
        _ => unimplemented!("sub CPU write 0x{:02x} @ 0x{:02x}", val, addr),
    }

    refresh_irq(cdc);
}

pub fn host_write(cdc: &mut Cdc, addr: u8, v: u8) {
    let decoder = &mut cdc.decoder;

    match (addr, decoder.ra) {
        // ADDRESS
        (0, _) => decoder.ra = v & 3,
        // COMMAND
        (1, 0) => decoder.host_command(v),
        // PARAMETER
        (2, 0) => decoder.push_param(v),
        // HINTMSK
        (2, 1) => decoder.hintmsk = v & 0x1f,
        // HCLRCTL
        (3, 1) => {
            if v & 0x80 != 0 {
                unimplemented!("HCLRCTL CHPRST");
            }

            if v & 0x20 != 0 {
                unimplemented!("HCLRCTL SMADPCRL");
            }

            // CRLPRM
            if v & 0x40 != 0 {
                decoder.host_params.clear();
            }

            let to_ack = v & 0x1f;

            decoder.hintsts &= !to_ack;
        }
        _ => todo!("Host write 0x{:02x} @ {}:{}", v, addr, decoder.ra),
    }

    refresh_irq(cdc);
}

pub fn host_read(cdc: &mut Cdc, addr: u8) -> u8 {
    let decoder = &mut cdc.decoder;

    match (addr, decoder.ra) {
        // HSTS
        (0, _) => decoder.hsts(),
        // RESULT
        (1, _) => {
            assert!(!decoder.host_result.is_empty());
            decoder.host_result.pop()
        }
        // HINTSTS
        (3, 1) => decoder.hintsts,
        _ => todo!("Host read @ {}:{}", addr, decoder.ra),
    }
}

pub fn sub_cpu_read(cdc: &mut Cdc, addr: u8) -> u8 {
    let decoder = &mut cdc.decoder;

    // trace!("DECODER read 0x{:02x}", addr);
    match addr {
        0x00 => decoder.eccsts(),
        0x01 => decoder.dests(),
        // HDR
        0x03 => decoder.hdr.read(),
        // CMADR
        0x05 => ((decoder.dadrc >> 10) & 0x7f) as u8,
        // INTSTS
        0x07 => decoder.irq.status,
        0x11 => decoder.hifsts(),
        // HSTPRM
        0x12 => {
            assert!(!decoder.host_params.is_empty());
            decoder.host_params.pop()
        }
        // HSTCMD
        0x13 => decoder.host_command,
        _ => unimplemented!("sub CPU read 0x{:02x}", addr),
    }
}

/// Called when the DSP has read a sector (this is called at the moment the DSP drives SCOR high)
pub fn dsp_sector_read(cdc: &mut Cdc, sector: Sector) {
    assert!(cdc.decoder.state.is_idle());

    // From the moment the DSP's SCOR pulse to the moment the DECINT IRQ triggers there's about
    // 6.87ms or ~303 audio cycles.
    //
    // XXX That's during the ToC read, needs to test with other modes (streaming/audio decoding,
    // the datasheet seems to hint that it's handled differently in this case). Also in 2x where
    // it's probably faster than that since otherwise it wouldn't be able to maintain that speed
    // (2x means one sector every 294 audio cycles).
    cdc.decoder.state = DecoderState::SectorPending {
        sector,
        decint_delay: 303,
    };
}

fn refresh_irq(cdc: &mut Cdc) {
    // Interrupt pin is active low
    let xint_lvl = !cdc.decoder.irq.is_active();

    cdc.uc.set_decoder_xint(xint_lvl);
}

#[derive(Clone)]
struct IrqController {
    /// Active interrupts
    status: u8,
    /// Interrupt mask for the output IRQ signal
    mask: u8,
}

impl IrqController {
    fn new() -> IrqController {
        IrqController { status: 0, mask: 0 }
    }

    fn is_active(&self) -> bool {
        (self.status & self.mask) != 0
    }

    fn trigger(&mut self, irq: Irq) {
        self.status |= 1u8 << (irq as u8);
    }
}

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Irq {
    /// Host chip reset issued
    HCRIsd = 0,
    /// Host command
    HstCmnd = 1,
    /// Decoder interrupt
    DecInt = 2,
    /// Host DMA complete
    HDMACmp = 3,
    /// Real-time ADPCM end
    RTADPEnd = 4,
    /// Result empty
    RsltEmpt = 5,
    /// Decoder time out
    DecTOut = 6,
    /// Drive overrun
    DrvOvrn = 7,
}

enum DecoderState {
    Idle,
    /// A sector was received from the DSP
    SectorPending {
        sector: Sector,
        decint_delay: u16,
    },
}

impl DecoderState {
    fn is_idle(&self) -> bool {
        matches!(self, DecoderState::Idle)
    }
}

/// Decoder control register
#[derive(Copy, Clone, Debug)]
struct DecCtl(u8);

impl DecCtl {
    /// Returns true if DLADR (drive last address) should be enabled
    fn endladr(self) -> bool {
        use DecoderMode::*;

        // ENDLADR bit
        if self.0 & (1u8 << 7) == 0 {
            false
        } else {
            // Per the datasheet, the ENDLADR function is only effective in these modes
            matches!(self.mode(), WriteOnly | RealTimeCorrection | CdDa)
        }
    }

    /// If autodist is enabled, returns None, otherwise returns the value of the modesel and
    /// formsel bits
    fn mode_form_sel(self) -> Option<u8> {
        if self.0 & 8 == 1 {
            None
        } else {
            Some((self.0 >> 4) & 3)
        }
    }

    fn mode(self) -> DecoderMode {
        match self.0 & 7 {
            0b000 | 0b001 => DecoderMode::Disabled,
            0b010 | 0b011 => DecoderMode::MonitorOnly,
            0b100 => DecoderMode::WriteOnly,
            0b101 => DecoderMode::RealTimeCorrection,
            0b110 => DecoderMode::RepeatCorrection,
            0b111 => DecoderMode::CdDa,
            _ => unreachable!(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DecoderMode {
    Disabled,
    MonitorOnly,
    WriteOnly,
    RealTimeCorrection,
    RepeatCorrection,
    CdDa,
}

impl DecoderMode {
    /// If this mode writes to RAM, returns the sector offset of the data written to RAM
    fn write_to_ram(self) -> bool {
        use DecoderMode::*;
        // I'm not sure of this at all, maybe the data is always written to RAM? The datasheet is
        // really not clear, I should probably test more
        match self {
            RealTimeCorrection => true,
            // This mode is configured by the firmware so we need to handle it here but it's not
            // actually used: I'm fairly sure the firmware means to send the contents of address
            // 0xb6 to DECCTL, but instead it sends a literal $0xb6. It's only done at init
            // however, so it doesn't really matter.
            //
            // I don't fully understand how this mode works, but I just treat it like
            // a normal CD-ROM read: we skip over the sync bytes and write the rest.
            RepeatCorrection => true,
            // I'm really not sure about this one and need to run more tests. Since the datasheet
            // says that DLADR is effective in this mode, it means that we write to the buffer? But
            // then do we write the full sector or do we skip the first 12 bytes? The block diagram
            // in the datasheet, if accurate, has the DAC interface take its source either from the
            // ADPCM decoder which is connected to the memory buffer bus, or straight from the
            // output of the descrambler, before it goes to the memory bus. That seems to imply
            // that CD-DA playback doesn't go through RAM, which seems very reasonable.
            //
            // I guess I could just probe the RAM lines while playing an audio CD...
            CdDa => true,
            // Probably?
            Disabled => false,
            _ => unimplemented!("{:?}", self),
        }
    }

    /// Returns `true` if the decoder triggers DECINT in this mode
    fn decint(self) -> bool {
        use DecoderMode::*;
        // In `RepeatCorrection` correction mode "the status is established each time one
        // correction is completed". I don't think we need to bother with that for now.
        matches!(self, MonitorOnly | WriteOnly | RealTimeCorrection)
    }
}

/// A 4-byte buffer that's read one byte at a time
struct HdrBuf {
    buf: [u8; 4],
    pos: u8,
}

impl HdrBuf {
    fn new() -> HdrBuf {
        HdrBuf {
            buf: [0; 4],
            pos: 0,
        }
    }

    fn reset(&mut self, b: &[u8; 4]) {
        self.pos = 0;
        self.buf.clone_from_slice(b);
    }

    fn read(&mut self) -> u8 {
        let b = self.buf[usize::from(self.pos)];

        // I haven't tested if we actually loop around if we read more than 4 bytes
        self.pos = (self.pos + 1) & 3;

        b
    }
}

/// 16byte FIFO used to store command arguments and responses
#[derive(Clone)]
pub struct HostFifo {
    /// Data buffer
    buffer: [u8; 16],
    /// Write pointer (4bits + carry)
    write_idx: u8,
    /// Read pointer (4bits + carry)
    read_idx: u8,
}

impl HostFifo {
    pub fn new() -> HostFifo {
        HostFifo {
            buffer: [0; 16],
            write_idx: 0,
            read_idx: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        // If both pointers point at the same cell and have the same carry the FIFO is empty.
        self.write_idx == self.read_idx
    }

    pub fn is_full(&self) -> bool {
        // The FIFO is full if both indexes point to the same cell while having a different carry.
        self.write_idx == self.read_idx ^ 0x10
    }

    pub fn clear(&mut self) {
        self.write_idx = 0;
        self.read_idx = 0;
        self.buffer = [0; 16];
    }

    pub fn push(&mut self, val: u8) {
        let idx = (self.write_idx & 0xf) as usize;

        self.buffer[idx] = val;

        self.write_idx = self.write_idx.wrapping_add(1) & 0x1f;
    }

    pub fn pop(&mut self) -> u8 {
        debug_assert!(!self.is_empty());

        let idx = (self.read_idx & 0xf) as usize;

        self.read_idx = self.read_idx.wrapping_add(1) & 0x1f;

        self.buffer[idx]
    }
}

impl fmt::Debug for HostFifo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fifo = self.clone();

        let mut first = true;

        write!(f, "[")?;

        while !fifo.is_empty() {
            let v = fifo.pop();

            if first {
                first = false;
            } else {
                write!(f, ", ")?;
            }

            write!(f, "0x{:02x}", v)?;
        }

        write!(f, "]")
    }
}
