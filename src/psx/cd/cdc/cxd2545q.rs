use super::{us_to_audio_cycles, Cdc};
use cdimage::{DiscPosition, Msf};
use std::cmp::min;

/// CXD2545Q: DSP with built-in digital servo
pub struct Dsp {
    state: State,
    /// Shift register for serial writes coming from the microcontroller (24 bits)
    command_sr: u32,
    /// Shift register used for serial data read (through SENS/SCLK, configured through command
    /// 0x39)
    sens_sr: u16,
    /// Set by the 0x9 "Function specification" command. As far as I can tell it only changes
    /// the meaning of the SENS output. Unlike what the name implies setting this bit to 0 does not
    /// in fact prevent the execution of auto sequences (they seem to run perfectly normally,
    /// including the optional timeout).
    aseq: bool,
    /// Focus control, as configured by command 0x0
    focus_control: u8,
    /// Tracking control, as configured by command 0x1
    tracking_control: u8,
    /// Tracking mode, as configured by command 0x2
    tracking_mode: u8,
    /// True when the laser is focused on the disc
    focus_ok: bool,
    /// Focus bias data
    focus_bias: i16,
    /// Delay until the AGOK/XAVEBSY signals go up (see the 0x38 command)
    agok_xavebsy_delay: u32,
    /// True when the spindle is configured as CLVP (PLL servo mode) or CLVA (automatic CLVS/CLVP
    /// switching mode). In this mode the linear velocity should be precisely 1x or 2x and we can
    /// reliably read/play the disc.
    clvp_engaged: bool,
    /// If true we read SOCT data instead of SUBQ from SQSO/SQCK
    soct_en: bool,
    /// SOCT shift register (read through SQSO/SQCK when `soct_en` is true)
    soct_sr: u32,
    /// Subchannel Q data output through SQSO/SQCK. The last two bytes are 15bit PCM data (absolute
    /// values)
    subq_data: [u8; 12],
    /// Current bit index in `subq_data`
    subq_data_pos: u8,
    /// Current value output on SQSO when `soct_en` is false.
    subq_out: bool,
    /// XXX hack
    subq_idx: usize,
    /// Number of audio cycles until the next sector is read, or None if we're not currently
    /// reading
    next_sector: Option<u16>,
    /// Number of audio cycles until the falling edge of the SCOR pulse, or None if we're not
    /// currently pulsing SCOR.
    end_of_scor: Option<u16>,
    /// SCEx string that's sent to the microcontroller during the lead-in for licensed discs.
    scex: u64,
    /// Position of the current bit of the SCEx string being sent to the controller (when we're in
    /// the lead-in).
    scex_pos: u8,
    /// Divider used to know when to move to the next position in the SCEx string.
    scex_divider: u8,
    /// Command preset table register 7: Auto sequence (N) track jump count setting
    track_jump_count: u16,
    /// Command preset table register 8: MODE setting
    mode_setting: u16,
    /// Sled kick level 1, 2, 3 or 4
    sled_kick_level: u8,
    /// Next sector to be read
    position: DiscPosition,
}

impl Dsp {
    pub fn new() -> Dsp {
        // In my tests it seems that my PlayStation starts reading the lead-in roughly one minute
        // before we reach the program area
        let position = "<99:00:00".parse().expect("Bad starting position");

        let mut dsp = Dsp {
            state: State::Idle,
            command_sr: 0,
            sens_sr: 0,
            aseq: false,
            focus_control: 0x00,
            tracking_control: 0x11,
            tracking_mode: 0x20,
            focus_ok: false,
            focus_bias: 0,
            agok_xavebsy_delay: 0,
            clvp_engaged: false,
            soct_en: false,
            soct_sr: 0,
            subq_data: [0; 12],
            subq_data_pos: 0,
            subq_idx: 0,
            subq_out: false,
            next_sector: None,
            end_of_scor: None,
            scex: 0,
            scex_pos: 0,
            scex_divider: 0,
            track_jump_count: 0x100,
            mode_setting: 0,
            sled_kick_level: 1,
            position,
        };

        dsp.build_scex_string();

        dsp
    }

    fn is_busy(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    // Command 0x9: Function specification
    fn set_function_specification(&mut self, v: u8) {
        self.aseq = v & 0x20 != 0;
    }

    /// Returns the value of the GFS signal (high when frame sync and the insertion protection
    /// timing match)
    fn gfs(&self) -> bool {
        // GFS (frame sync) output
        //
        // On the real hardware this signal is tricky, it appears to never be 100% completely
        // stable (i.e. even will all setup and calibration done I sometimes see it drop).
        //
        // It's extremely unstable before the AGT/AGF sequences. A lot less so after.
        //
        // The datasheet of the CXD2545Q LOCK output says:
        //
        //   GFS is sampled at 460Hz; when GFS is high, this pin outputs a high signal. If GFS
        //   is low eight consecutive samples, this pin outputs low.
        //
        // That seems to imply that a very short drop of the GFS is not enough to consider that
        // there's a delock, so I guess having it drop here and there shouldn't be cause for
        // concern.
        //
        // After AGT/AGE the drops are very uncommon and they always appear to last about 135us.
        // There can be upwards of 1s between drops, although sometimes I get two in less than
        // 20ms. It appears very random.

        // For now let's keep it simple: if we are focused and the spindle is in PLL mode, pretend
        // that GFS is locked
        self.focus_ok && self.clvp_engaged
    }

    /// Returns true if the drive is in a state where sectors are being read
    fn can_read_sector(&self) -> bool {
        // If we have frame sync we're probably reading sectors. There are many factors to take
        // into account to figure out if and how we read sectors but for the time being that's
        // probably a good enough approximation.
        //
        // The `is_busy` part is a gross oversimplification, in practice I noticed that we would
        // get SCOR pulses sometimes while seeking with bogus data.
        self.gfs() && !self.is_busy()
    }

    /// Returns true if the configured disc speed is 2x, false if it's 1x.
    fn is_speed_2x(&self) -> bool {
        // XXX For now we only support 1x
        false
    }

    /// Build the SCEx license string for the loaded disc.
    fn build_scex_string(&mut self) {
        // Start by setting the SCEx string to zero. That's the default level when nothing is
        // output.
        //
        // XXX If the disc is unlicensed (audio or otherwise) we should stop here. Or maybe just
        // set one bit to simulate the random noise I see when no SCEx string is present?
        self.scex = 0;

        let mut pos = 0;

        let mut push_scex_bit = |b| {
            debug_assert!(pos < SCEX_STRING_BITS);

            self.scex |= u64::from(b) << pos;

            pos += 1;
        };

        // The magic unlock sequence, SCEI for Japan, SCEA for America and SCEE for Europe
        //
        // XXX Handle other regions
        let magic = [b'S', b'C', b'E', b'E'];

        // Each byte is sent using an UART-like protocol: 8bits per byte, LSB-first, one start bit,
        // two stop bits, no parity.
        //
        // The only difference with a proper UART is that the polarity of the signal is inverted:
        // start bits are high, stop bits are low, data bits are complemented and the line is low
        // when idle.
        for &byte in &magic {
            // Start bit
            push_scex_bit(true);

            // Push the current byte, LSB-first, negated
            for i in 0..8 {
                let b = (byte & (1 << i)) != 0;

                push_scex_bit(!b);
            }

            // Two stop bits
            push_scex_bit(false);
            push_scex_bit(false);
        }
    }

    fn focus_servo_enabled(&self) -> bool {
        self.focus_control & 0b1000 != 0
    }

    fn tracking_servo_enabled(&self) -> bool {
        (self.tracking_mode >> 2) & 3 != 0
    }

    fn sled_servo_enabled(&self) -> bool {
        self.tracking_mode & 3 != 0
    }
}

pub fn run_audio_cycle(cdc: &mut Cdc) {
    let mut dsp = &mut cdc.dsp;

    if dsp.agok_xavebsy_delay > 0 {
        dsp.agok_xavebsy_delay -= 1;
    }

    match dsp.state {
        State::Idle => (),
        State::BusyFrozen => (),
        State::BusyWait(ref mut ac) => {
            *ac -= 1;

            if *ac == 0 {
                dsp.state = State::Idle;
                update_sens(cdc);
            }
        }
        State::FocusOn(ref mut ac) => {
            *ac -= 1;

            if *ac == 0 {
                auto_focus_finish(cdc);
                update_sens(cdc);
            }
        }
        State::Jump(ref mut ac) => {
            *ac -= 1;

            if *ac == 0 {
                jump_finish(cdc);
                update_sens(cdc);
            }
        }
    }

    // Check for SCOR pulse
    if let Some(eos) = cdc.dsp.end_of_scor {
        if eos > 1 {
            cdc.dsp.end_of_scor = Some(eos - 1);
        } else {
            // End of SCOR pulse
            cdc.dsp.end_of_scor = None;
            cdc.uc.set_dsp_scor(false);
        }
    }

    check_sector_read(cdc);
    handle_scex(cdc);
}

fn set_position(cdc: &mut Cdc, new_pos: DiscPosition) {
    cdc.dsp.position = new_pos;

    // Update LMSW state
    let lmsw = match cdc.dsp.position {
        DiscPosition::Program(_) => true,
        DiscPosition::LeadIn(msf) => {
            // With Metal Gear Solid disc 1 I read the first lead-in sector at 98:53:49 the LMSW
            // signal goes high at around 99:29:47 in the lead-in (with some bouncing for ~5
            // sectors). That means that the signal remains low for ~2700 sectors from the start of
            // the lead-in.
            //
            // Obviously the exact position will vary depending on the drive and on the disc
            let limit = Msf::from_bcd(0x99, 0x29, 0x47).unwrap();

            msf > limit
        }
    };

    cdc.uc.set_lmsw(lmsw);
}

fn check_sector_read(cdc: &mut Cdc) {
    if let Some(next) = cdc.dsp.next_sector {
        if next > 1 {
            cdc.dsp.next_sector = Some(next - 1);
            return;
        }

        // We have a new sector
        read_sector(cdc);

        cdc.dsp.next_sector = None;
    }

    // See if we need to schedule a new sector read
    if cdc.dsp.next_sector.is_none() && cdc.dsp.can_read_sector() {
        // Schedule the next sector read.
        let mut audio_cycles_to_next_sector = 44_100 / 75;

        if cdc.dsp.is_speed_2x() {
            audio_cycles_to_next_sector /= 2;
        }

        cdc.dsp.next_sector = Some(audio_cycles_to_next_sector);
    }
}

fn read_sector(cdc: &mut Cdc) {
    if !cdc.dsp.can_read_sector() {
        return;
    }

    // Pulse the SCOR signal. At 1x it remains high for about 136us, which is almost exactly 6 audio
    // cycles at 44.1kHz. Only half as long if we're at 2x.
    let scor_pulse_length = if cdc.dsp.is_speed_2x() { 3 } else { 6 };

    cdc.dsp.end_of_scor = Some(scor_pulse_length);
    cdc.uc.set_dsp_scor(true);

    // Fetch the new sector
    debug!("Reading sector {}", cdc.dsp.position);
    let disc = cdc
        .disc
        .as_mut()
        .expect("Attempted to read a sector without disc");
    let sector = match disc.read_sector(cdc.dsp.position) {
        Ok(s) => s,
        Err(e) => panic!("Can't read sector {}: {}", cdc.dsp.position, e),
    };

    // XXX TODO
    let subq_crc_ok = true;

    let mut subq = sector.q().to_raw();

    // The last two bytes of the data read from the subq pin are *not* the checksum (the checksum
    // is apparently checked internally then discarded). Instead I'm not sure *what* they are,
    // maybe the "15-bit peak data" according to the datasheet, but in my test I keep reading
    // two alternating values: [0x9f, 0x7f] and [0xfc, 0xff]
    let msf = match cdc.dsp.position {
        DiscPosition::LeadIn(msf) => msf,
        DiscPosition::Program(msf) => msf,
    };

    if msf.sector_index() & 1 != 0 {
        subq[10] = 0x9f;
        subq[11] = 0x7f;
    } else {
        subq[10] = 0xfc;
        subq[11] = 0xff;
    };

    cdc.dsp.subq_data_pos = 0;
    cdc.dsp.subq_data = subq;

    // We output CRC OK bit on SUBQ when we pulse SCOR. We'll then shift the 96 SUBQ bits in
    // `sqck_tick`
    cdc.dsp.subq_out = subq_crc_ok;

    if !cdc.dsp.soct_en {
        // We output the SUBQ on the external PIN
        cdc.uc.set_dsp_subq(cdc.dsp.subq_out);
    }

    cdc.dsp_sector_read();

    let new_pos = cdc.dsp.position.next().expect("Reached max position");
    set_position(cdc, new_pos);
}

fn handle_scex(cdc: &mut Cdc) {
    // XXX TODO
    let in_lead_in = true;

    // In my tests the SCEx string was only received during the lead-in. In Rayman and Ridge
    // Racer Revolution it seems to stop almost immediately when we reach the track 01 prelude, in
    // Metal Gear Solid it seems to stop about 15 seconds before the end of the lead-in.
    //
    // Since as far as I know no CD dump format bothers to store this info (and I suspect that
    // dumping it in the first place would be pretty tricky with most CD drives) we're just going
    // to go the easy route and output the SCEx during the full duration of the lead-in and never
    // after that.
    //
    // It's important not to output the SCEx signal all the time otherwise it will trigger the
    // anti-modchip protections some games have.
    let scex = if in_lead_in {
        if cdc.dsp.scex_divider == 0 {
            cdc.dsp.scex_divider = AUDIO_CYCLES_PER_SCEX_BIT;

            // Move on to the next position in the SCEx string, or loop back to the start if
            // we reached the end
            cdc.dsp.scex_pos += 1;

            // The value of
            if usize::from(cdc.dsp.scex_pos) >= SCEX_STRING_BITS {
                cdc.dsp.scex_pos = 0;
            }
        }
        cdc.dsp.scex_divider -= 1;

        cdc.dsp.scex & (1 << cdc.dsp.scex_pos) != 0
    } else {
        // Not in lead-in, output 0
        //
        // XXX In reality this is not completely stable after the lead-in (or even with unlicensed
        // discs for that matter). I sometimes get a few spikes here and there, although that
        // doesn't really seem to match anything and could just be noise.
        false
    };

    cdc.uc.set_dsp_scex(scex);
}

/// Called when the microcontroller outputs a rising edge on the serial clock.
pub fn serial_tick(cdc: &mut Cdc, data: bool) {
    // Shift into the shift register
    cdc.dsp.command_sr >>= 1;
    cdc.dsp.command_sr |= (data as u32) << 23;

    // The value of the SENS output changes with the current value of the shift register (no latch
    // necessary).
    update_sens(cdc);
}

/// Called when the microcontroller outputs a falling edge on the XLAT signal
pub fn serial_latch(cdc: &mut Cdc) {
    // XXX Not sure whether the shift register is cleared after each command and no
    // easy way to test. The firmware does explicitly pad some commands with zeroes
    // so it seems to hint that the buffer is not flushed (although that could also be
    // poor optimization in the firmware).

    let cmd = cdc.dsp.command_sr;

    let op = cmd >> 20;

    debug!("DSP latch 0x{:06x}", cmd);

    if op <= 3 {
        // Servo command
        //
        // Note from the datasheet regarding auto-sequences (2-8 p.54):
        //
        //   The servo block is used in an exclusive manner during the auto sequence execution
        //   (when XBUSY = low), so that commands from the CPU are not transferred to the servo
        //   block, but can be sent to the signal processor block.

        // The servo block is used exclusively by the DSP during an auto-sequence so commands can't
        // go through.
        if cdc.dsp.is_busy() {
            warn!("Servo access while DSP is busy, ignoring");
            return;
        }

        match op {
            // Focus control
            0x0 => {
                let fc = (cmd >> 16) as u8;
                cdc.dsp.focus_control = fc;

                if fc & 0b1100 == 0b1000 {
                    debug!("Focus servo ON (focus gain normal)");
                } else if fc & 0b1100 == 0b1100 {
                    debug!("Focus servo ON (focus gain down)");
                } else if fc & 0b1010 == 0b0000 {
                    debug!("Focus servo OFF, 0V out");
                } else if fc & 0b1011 == 0b0010 {
                    debug!("Focus search voltage down");
                } else if fc & 0b1011 == 0b0011 {
                    debug!("Focus search voltage up");
                }
            }
            // Tracking control
            0x1 => {
                let tc = (cmd >> 16) as u8;

                if cdc.dsp.tracking_control != tc {
                    cdc.dsp.tracking_control = tc;

                    debug!(
                        "Tracking control: anti-shock {}, brake {}, \
                       tracking gain {} filter select {}",
                        if tc & 0xc == 8 { "on" } else { "off" },
                        if tc & 4 != 0 { "on" } else { "off" },
                        if tc & 2 != 0 { "up" } else { "normal" },
                        2 - (tc & 1)
                    );
                }
            }
            // Tracking mode
            0x2 => {
                let tm = (cmd >> 16) as u8;
                cdc.dsp.tracking_mode = tm;

                // XXX we need to implement tracking mode off, reverse sled to emulate the sequence
                // used to reset the sled to the start of the disc

                match (tm >> 2) & 3 {
                    0 => debug!("Tracking servo OFF"),
                    1 => debug!("Tracking servo ON"),
                    2 => debug!("Forward track jump"),
                    3 => unimplemented!("Reverse track jump"),
                    _ => unreachable!(),
                }

                match tm & 3 {
                    0 => debug!("Sled servo OFF"),
                    1 => debug!("Sled servo ON"),
                    2 => debug!("Forward sled move"),
                    3 => debug!("Reverse sled move"),
                    _ => unreachable!(),
                }
            }
            // SELECT
            0x3 => {
                match cmd >> 16 {
                    k @ 0x30..=0x33 => {
                        let kl = ((k & 3) + 1) as u8;

                        if cdc.dsp.sled_kick_level != kl {
                            cdc.dsp.sled_kick_level = kl;
                            debug!("Sled kick level ±{}", kl);
                        }
                    }
                    // Coefficient RAM + focus bias and TRVSC data
                    0x34 => {
                        let addr2 = (cmd >> 10) & 0x3f;

                        match addr2 {
                            0b0000_00..=0b0100_11 => {
                                debug!("Write K{:02X} = 0x{:02x}", (cmd >> 8) & 0xff, cmd & 0xff)
                            }
                            0b1111_00 => debug!("TRVSC data: 0x{:03x}", cmd & 0x3ff),
                            // Focus BIAS data
                            0b1111_01 => {
                                // Sign extend and mask bit 0 which is not used according to the
                                // docs, not that it makes a huge difference for us here.
                                let focus_bias = (cmd << 6) as i16;
                                let focus_bias = focus_bias >> 6;

                                // Bias data appears to be signed (which makes sense, but it took
                                // me an embarrassing amount of time to figure it out). For our
                                // emulation purposes it doesn't really matter *except* that our
                                // defocus triggers below are going to be all messed up if we treat
                                // it as unsigned.
                                cdc.dsp.focus_bias = focus_bias;
                                debug!("Focus bias data: {}", focus_bias);

                                // The actual defocus level changes from drive to drive, one of
                                // them seems to lose it at about ±144, the one that I use for
                                // testing at the moment ±192. The bounds are not exactly the same
                                // in positive and negative, but it's not really important.
                                //
                                // At any rate there's no reason for the firmware to ever try it unless
                                // we mess up the PER "RF jitter amount" values.
                                if cdc.dsp.focus_ok && (focus_bias.abs() > 192) {
                                    cdc.dsp.focus_ok = false;
                                    warn!("Focus bias too large! Losing focus.");
                                }
                            }
                            _ => {
                                unimplemented!("Unknown 0x34 command: 0x{:x} (0b{:b})", cmd, addr2)
                            }
                        }
                    }
                    // FZSL/SLED MOVE/Voltage/AUTO GAIN
                    0x37 => debug!("FZSL/SLED MOVE/Voltage/AUTO GAIN: 0x{:04x}", cmd & 0xffff),
                    // Level/AUTO GAIN/DFSW/(Initialize)
                    0x38 => {
                        debug!("Level/AUTO GAIN/DFSW/(Initialize): 0x{:04x}", cmd & 0xffff);

                        // Tracking zero level measurement
                        let is_tclm = cmd & (1 << 4) != 0;

                        // RF zero level measurement
                        let is_rflm = cmd & (1 << 11) != 0;

                        // Focus zero level measurement
                        let is_flm = cmd & (1 << 13) != 0;

                        // VC level measurement
                        let is_vclm = cmd & (1 << 15) != 0;

                        // Focus automatic gain adjustment
                        let is_agt = cmd & (1 << 8) != 0;

                        // Focus automatic gain adjustment
                        let is_agf = cmd & (1 << 9) != 0;

                        if is_tclm | is_rflm | is_flm | is_vclm {
                            // When an AVGR measurement is requested SENS goes low for between
                            // 2.9ms and 5.8ms (per the datasheet, in my tests the length of the
                            // measure does vary a lot between tests but does stay between these
                            // bounds).
                            //
                            // In practice the firmware doesn't check SENS however, it just waits
                            // more than 5.8ms (more than 15ms actually) and assumes it's done, so
                            // exact timing shouldn't matter.
                            cdc.dsp.agok_xavebsy_delay = us_to_audio_cycles(3_000);
                        } else if is_agt {
                            // The delay varies a lot, I got as long as 0.6seconds
                            // I decided to use the lowest value I measured, about 350ms
                            //
                            // XXX same remark as for AGF below
                            cdc.dsp.agok_xavebsy_delay = us_to_audio_cycles(350_000);
                        } else if is_agf {
                            // The delay varies a lot, I got as long as 1.4 seconds
                            // I decided to use the lowest value I measured, about 500ms
                            //
                            // XXX in practice the datasheet says that AGOK remains high for up to
                            // 11.4us before going down. I measured ~8us during a test run.
                            cdc.dsp.agok_xavebsy_delay = us_to_audio_cycles(500_000);
                        }

                        // XXX While AGT or AGF is true the laser appears to "hover" at the same
                        // location instead of following the disc. It seems to loop over the same
                        // ~8 sectors again and again.
                    }
                    // Serial data read mode select
                    0x39 => {
                        cdc.dsp.sens_sr = match (cmd >> 8) & 0xff {
                            // Read RFDC
                            // On my system this returns 0x80 during early init (at the point this
                            // value is read)
                            0x1e => 0x80 << 8,
                            n => unimplemented!("DSP serial read 0x{:02x}", n),
                        };
                        update_sens(cdc);
                    }
                    // Focus bias ON
                    0x3a => {
                        // Only one bit in this register
                        if cmd & (1 << 14) != 0 {
                            debug!("FBON");
                        } else {
                            debug!("FBOFF");
                        }
                    }
                    // Operation for MIRR/DFCT/FOK
                    // There's a bunch of different values in this register configuring low level
                    // things we (probably) don't care about
                    0x3b => debug!("Operation for MIRR/DFCT/FOK: 0x{:03x}", (cmd >> 4) & 0xfff),
                    // "Others"
                    0x3f => debug!("Others: 0x{:04x}", cmd & 0xffff),
                    sel => unimplemented!("SELECT 0x{:02x}", sel),
                }
            }
            _ => unimplemented!("Servo command 0x{:x}", op),
        }
    } else {
        match op {
            // Auto sequence
            0x4 => auto_sequence(cdc, (cmd >> 8) as u16),
            0x5 => {
                let tr = (cmd >> 16) & 0xf;

                debug!("Blind/Overflow/Break: 0x{:x}", tr);
            }
            0x6 => {
                let cmd = (cmd >> 12) as u8;
                let kick_d = (cmd >> 4) & 0xf;
                let kick_f = cmd & 0xf;

                debug!("Kick D 0x{:x}, Kick F 0x{:x}", kick_d, kick_f);
            }
            0x7 => {
                cdc.dsp.track_jump_count = (cmd >> 4) as u16;

                debug!("Track jump count: {}", cdc.dsp.track_jump_count);
            }
            // MODE setting
            0x8 => {
                let ms = (cmd >> 8) as u16;

                if cdc.dsp.mode_setting != ms {
                    cdc.dsp.mode_setting = ms;

                    let cdrom = cmd & (1 << 19) != 0;
                    let dout_mute = cmd & (1 << 18) != 0;
                    let dout_mute_f = cmd & (1 << 17) != 0;
                    let wsel = cmd & (1 << 16) != 0;
                    let vco_sel = cmd & (1 << 15) != 0;
                    let ashs = cmd & (1 << 14) != 0;
                    let soct = cmd & (1 << 13) != 0;

                    // update_soct will be called often this function and will take care of
                    // refreshing soct_sr if needed, nothing to do here.
                    cdc.dsp.soct_en = soct;

                    debug!(
                        "MODE setting: CDROM {}, DOUT Mute {}, D.out Mute-F {}, WSEL {}, \
                         VCO SEL {}, ASHS {}, SOCT {}",
                        cdrom, dout_mute, dout_mute_f, wsel, vco_sel, ashs, soct
                    );
                }
            }
            // Function specification
            0x9 => cdc.dsp.set_function_specification((cmd >> 12) as u8),
            // Audio contlor
            0xa => debug!("DSP audio ctrl: 0x{:02x}", (cmd >> 12) & 0xff),
            // Spindle servo coefficient setting
            0xc => debug!("Spindle servo coef 0x{:02x}", (cmd >> 12) & 0xff),
            // CLV CTRL
            0xd => debug!("DSP CLV ctrl: 0x{:x}", (cmd >> 16) & 0xf),
            // CLV mode
            0xe => {
                let clv_mode = (cmd >> 16) & 0xf;
                let mut clvp_engaged = false;

                match clv_mode {
                    0b0000 => debug!("CLV stop"),
                    // Kick: sends the disc rotating very fast. It's not "just" a short kick.
                    0b1000 => debug!("CLV kick"),
                    0b1010 => debug!("CLV brake"),
                    0b1110 => debug!("CLVS"),
                    0b1100 => debug!("CLVH"),
                    0b1111 => {
                        debug!("CLVP");
                        clvp_engaged = true;
                    }
                    0b0110 => {
                        debug!("CLVA");
                        // CLVA automatically switches between CLVS (rough mode) and CLVP (PLL
                        // mode), for the sake of simplicity for now we're going to pretend that in
                        // this mode we're always properly lock to the PLL
                        clvp_engaged = true;
                    }
                    n => unimplemented!("Unknown CLV config 0x{:x}", n),
                }

                cdc.dsp.clvp_engaged = clvp_engaged;
            }
            _ => unimplemented!("DSP command 0x{:06x}", cmd),
        }
    }

    update_sens(cdc);
    update_soct(cdc);
}

/// Called when the microcontroller outputs a falling edge on the SCLK signal
pub fn sclk_tick(cdc: &mut Cdc) {
    cdc.dsp.sens_sr <<= 1;
    update_sens(cdc);
}

/// Called when the microcontroller outputs a falling edge on the SQCK signal
pub fn sqck_tick(cdc: &mut Cdc) {
    let val = if cdc.dsp.soct_en {
        // We're reading the SOCT value
        cdc.dsp.soct_sr >>= 1;
        cdc.dsp.soct_sr & 1 != 0
    } else {
        // We're reading SUBQ
        let pos = cdc.dsp.subq_data_pos;
        let byte_pos = pos / 8;
        let bit = pos % 8;

        match cdc.dsp.subq_data.get(byte_pos as usize) {
            Some(byte) => {
                cdc.dsp.subq_data_pos += 1;

                // We send LSB first
                (byte >> bit) & 1 != 0
            }
            // No more data to send
            None => false,
        }
    };

    cdc.uc.set_dsp_subq(val);
}

fn auto_sequence(cdc: &mut Cdc, command: u16) {
    let sequence = (command >> 8) & 0xf;
    let timeout = (command >> 4) & 0xf;
    let long_timeout = (command & 8) != 0;

    // Timeout is counted based on 16.9344MHz / 3. That means that short timeouts go from 2.9ms to
    // 43.6ms while long timeouts range from 0.186s to 2.787s. This matches both the datasheet and
    // measurements made on the real hardware.
    let timeout = i32::from(timeout) << (if long_timeout { 20 } else { 14 });

    // If timeout is non-zero the command will interrupt itself after the configured delay if it
    // can't complete. Otherwise it'll keep running until it completes or is interrupted.
    if timeout != 0 && sequence != 0 {
        // Focus ON command (0x4) lowers the lens and XBUSY goes up after the timeout. The
        // datasheet says that it sends 0x02 to the DSP in this situation which adds up.
        //
        // Other commands need testing.
        unimplemented!("Auto sequence timeout!");
    }

    if cdc.dsp.is_busy() && sequence != 0 {
        unimplemented!("Auto sequence while we're busy");
    }

    match sequence {
        // Cancel auto sequence.
        //
        // In my tests cancelling a sequence is not instant, it takes about 50us after the cancel
        // command is sent for XBUSY to go back up (with the auto-focus command running, haven't
        // tried the others). This meshes with the firmware code that busy-waits a few dozen
        // microseconds after sending the cancel command.
        //
        // Cancelling auto-sequences works whether a timeout has been specified
        // or not. If no auto-command was running nothing happens. It doesn't appear to set busy at
        // all, even for a very short time.
        0x0 => {
            if cdc.dsp.is_busy() {
                warn!("Cancelling auto-sequence");
                cdc.dsp.state = State::BusyWait(us_to_audio_cycles(50));
            }
        }
        0x7 => auto_focus_on(cdc),
        0x8 => track_jump_1(cdc, SledDirection::Forward),
        0x9 => track_jump_1(cdc, SledDirection::Reverse),
        0xc => track_jump_2n(cdc, SledDirection::Forward),
        0xd => track_jump_2n(cdc, SledDirection::Reverse),
        _ => unimplemented!("Auto command {:x}", sequence),
    }
}

fn auto_focus_on(cdc: &mut Cdc) {
    // Newer PSX firmware kicks the spindle before the focus sequence, meaning that the disc still
    // rotates with inertia when the sequence executes. I suspect that it does that to make the
    // focusing more reliable, however I've had no issue focusing my perfectly still MGS disc so we
    // shouldn't have to worry about the rotation of the disc here. Besides older firmwares (such
    // as the one in the SCPH-1002) don't kick before the focus sequence has successfully
    // completed.

    // Timings on SCPH-5552 (tested with several discs, both games and CD-DA):
    //
    // Normal focus-on sequence: focus_control[0x02] -> 0x47xx -> wait for FOK: 850ms between
    // 0x47xx and FOK
    //
    // If however the servo is idle before the sequence (focus_control 0x00 instead of 0x02) the
    // delay between 0x47xx and FOK is much lower at about 300ms. I suspect that it's simply
    // because the sensor starts higher this way so it has to travel less until the focus point is
    // reached. It probably also means that it might fail to focus on some discs/drives. This
    // sequence is not standard however, normally you always do 0x02 -> 0x47xx (per the datasheet).
    // As far as I can tell that's what the official firmwares are always doing too.
    //
    // The XBUSY signal goes down about 6.5ms after FOK

    let time_to_focus_ms = match cdc.dsp.focus_control & 3 {
        // Idle servo
        0 => 300,
        // Normal setting
        2 => 850,
        // 3 should prevent the focus from working since the lens should be all the way up
        // (assuming that it's got the time to go all the way up). I'm not sure what 1 would do.
        n => unimplemented!("Attempting focus with non-standard focus_control: {}", n),
    };

    // Of course focus won't work if laser is OFF or the disc is not there but we'll handle this in
    // auto_focus_on
    cdc.dsp.state = State::FocusOn(us_to_audio_cycles(time_to_focus_ms * 1000));
}

/// Called at the moment the auto sequence should focus the disc
fn auto_focus_finish(cdc: &mut Cdc) {
    let fok = cdc.disc_present() && cdc.uc.is_laser_on();

    cdc.dsp.focus_ok = fok;

    cdc.dsp.state = if fok {
        info!("Focus OK");
        State::BusyWait(us_to_audio_cycles(6_500))
    } else {
        // Focusing failed
        State::BusyFrozen
    };
}

fn jump_finish(cdc: &mut Cdc) {
    info!("Jump complete {}", cdc.dsp.position);
    cdc.dsp.state = State::Idle;
    update_sens(cdc)
}

/// Common to all track jumps
fn track_jump_common(cdc: &mut Cdc) {
    // p.57:
    //
    //   Always use them when focus, tracking, and sled servo are on. Note that tracking gain-up
    //   and braking-on ($17) should be sent beforehand because they are not performed.

    if !cdc.dsp.focus_servo_enabled()
        || !cdc.dsp.tracking_servo_enabled()
        || !cdc.dsp.sled_servo_enabled()
    {
        unimplemented!("Attempted to execute jump track with bad preconditions");
    }
}

/// The time it takes to jump a given number of tracks is not linear, the sled's inertia means that
/// short jumps will take more time (per track skipped) than large ones.
///
/// Here I just list some measured values on the real hardware for a jump_2n with various track
/// counts. I ran each test several times and selected the shortest delay.
///
/// Note that the first value is the actual track count, so for a `jump2n(25)` I list 50
const TRACK_JUMP_DELAY_TO_XBUSY: [(u32, u32); 6] = [
    (2, us_to_audio_cycles(2_300)),
    (50, us_to_audio_cycles(6_290)),
    (100, us_to_audio_cycles(8_230)),
    (200, us_to_audio_cycles(10_800)),
    (400, us_to_audio_cycles(13_800)),
    (1_000, us_to_audio_cycles(17_400)),
];

/// Compute the time it will take for the drive to jump over `track_count` tracks. Here "track"
/// means skipping over one rotation of the CD spiral, not one ToC track.
fn jump_delay(track_count: u32) -> u32 {
    assert_ne!(track_count, 0, "Jump with 0 track count!");
    // For I use a simple algorithm: we find where the jump count lies within
    // TRACK_JUMP_DELAY_TO_XBUSY then use linear interpolation between neighboring values to decide
    // how long it will take. If this is not precise enough we could either add more values in
    // the table or use a more sophisticated model.
    //
    // Technically we could go ever further and emulate the acceleration of the sled's motor etc...
    // but it's probably a bit overkill
    match TRACK_JUMP_DELAY_TO_XBUSY
        .iter()
        .position(|&(t, _d)| u32::from(t) >= track_count)
    {
        Some(0) => {
            // We're at the low bound
            let (tc, d) = TRACK_JUMP_DELAY_TO_XBUSY[0];

            // Linear interpolation + round
            (track_count * d + tc / 2) / tc
        }
        Some(n) => {
            let (htc, hd) = TRACK_JUMP_DELAY_TO_XBUSY[n];
            let (ltc, ld) = TRACK_JUMP_DELAY_TO_XBUSY[n - 1];

            let dd = hd - ld;
            let dtc = htc - ltc;

            let dx = track_count - ltc;

            ld + (dx * dd + dtc / 2) / dtc
        }
        None => {
            // In practice very large jump values may lead to a defocus in my tests, so it's
            // probably not used much in practice.
            warn!(
                "Jump track count {} is greater than max measured value",
                track_count
            );
            let &(tc, d) = TRACK_JUMP_DELAY_TO_XBUSY.last().unwrap();

            // Linear interpolation + round
            (track_count * d + tc / 2) / tc
        }
    }
}

fn track_jump_2n(cdc: &mut Cdc, dir: SledDirection) {
    track_jump_common(cdc);

    let track_count = 2 * u32::from(cdc.dsp.track_jump_count);

    if dir != SledDirection::Forward {
        todo!();
    }

    let new_pos = cdc
        .dsp
        .position
        .offset_turns(track_count as i32)
        .expect("bad jump");

    info!(
        "track jump {:?} {} ({} -> {})",
        dir, track_count, cdc.dsp.position, new_pos
    );

    let xbusy_delay = jump_delay(track_count);
    cdc.dsp.state = State::Jump(xbusy_delay);
    set_position(cdc, new_pos);
}

fn track_jump_1(cdc: &mut Cdc, dir: SledDirection) {
    track_jump_common(cdc);

    let turns = match dir {
        SledDirection::Forward => 1,
        SledDirection::Reverse => -1,
    };

    let new_pos = cdc.dsp.position.offset_turns(turns).expect("bad jump");

    // In my tests we end up 2 to 3 sectors after the theoretical sector one track away. That means
    // for instance that when I do a track jump at the very start of the program area I'd expect to
    // jump over ~9.8 sectors but I see +11 to +12 tracks while jumping ahead, -7/-8 when jumping
    // backwards.
    //
    // I think it can be easily explained: for one thing we start jumping potentially in the middle
    // of a track, and we probably end up in the middle of an other, so we need to wait for the
    // drive to finish the partial sector, then wait for one complete sector to report a new SUBQ.
    //
    // For now I just offset by one, we'll see in practice if the firmware likes it. I prefer to
    // unershoot than overshoot as a rule be cause if the firmware ends up just a little before the
    // target it'll just wait to get there, if it's ahead it has no choice but to seek backwards
    let new_pos = new_pos.next().unwrap();

    // The delay varies between ~480us and ~650us with the bulk of the measurements between 500 and
    // 650us. There don't appear to be timing differences between forward and backward jumps
    let xbusy_delay = us_to_audio_cycles(550);

    info!("track jump {} ({} -> {})", turns, cdc.dsp.position, new_pos);

    cdc.dsp.state = State::Jump(xbusy_delay);

    set_position(cdc, new_pos);
}

/// Update the value output from the SENS pin depending on the current value of the serial shift
/// register.
fn update_sens(cdc: &mut Cdc) {
    let sens_select = cdc.dsp.command_sr >> 16;

    let sens = if (!cdc.dsp.aseq && sens_select < 0xa) || sens_select == 0xd || sens_select == 0xf {
        // In this configuration SENS is supposed to be high-Z, in practice I read 0 with my
        // test setup
        false
    } else {
        match sens_select {
            0x38 => cdc.dsp.agok_xavebsy_delay == 0,
            // Used to read multi-bit values (using SCLK to shift the register)
            0x39 => cdc.dsp.sens_sr & 0x8000 != 0,
            // XBUSY (i.e. !busy)
            0x40..=0x4f => !cdc.dsp.is_busy(),
            // FOK (focus ok)
            0x50..=0x5f => cdc.dsp.focus_ok,
            0xa0..=0xaf => cdc.dsp.gfs(),
            // COUT
            0xc0..=0xcf => false,
            // XXX implement the rest
            _ => false,
        }
    };

    cdc.uc.set_dsp_sens(sens);
}

/// Must be called on serial latch. If SOCT is enabled this will refresh the value in `soct_sr`
fn update_soct(cdc: &mut Cdc) {
    let dsp = &mut cdc.dsp;

    if !dsp.soct_en {
        // In case SOCT was just deactivated, make sure we output the current value of SUBQ to
        // remain coherent
        cdc.uc.set_dsp_subq(dsp.subq_out);
        return;
    }

    // "RF jitter amount"
    //
    // This value is used by the firmware to calibrate the bias data. The value is rather unstable
    // on my system but as far as I can tell I get the smallest average values with a bias of
    // around -48 then it increases as I get away from that (in either direction)
    let per = 30 + min((dsp.focus_bias + 48).abs(), 0x90) as u8;

    // Error correction state (3 bits each). On my Metal Gear Solid disc the value is generally 0
    // with a 1 seen from time to time on one or the other of these values, which stands for "One
    // C1/C2 error corrected".
    let c1f = 0;
    let c2f = 0;

    // Focus and GFS are returned here. As far as I can tell they're exactly the same values as
    // those returned from SENS. The 5502 firmware appears to only use the PER and C1F/C2F values
    // from the SOCT, so these values don't really matter.
    let fok = dsp.focus_ok;
    let gfs = dsp.gfs();

    // Normally LOCK is based on GFS (we have a delock if the GFS drops for more than a given
    // amount of time) however for the time being our emulation of the GFS is "perfect", i.e. it
    // never glitches so we can consider that the LOCK and GFS are effectively the same.
    //
    // Not that it matters anyway, as far as I can tell the firmware ignores this value entirely,
    // discarding it immediately after reading it.
    let lock = gfs;

    // Emphasis flag. I don't think it's very common in the wild. Is it just the last value
    // extracted from the SUBQ control LSB?
    //
    // This value, like the LOCK, also appears to be discarded immediately after being read so it's
    // probably does not matter if we never implement it properly.
    let emph = false;

    // Build raw SOCT value in the shift register
    dsp.soct_sr = per as u32;
    dsp.soct_sr |= (c1f as u32) << 8;
    dsp.soct_sr |= (c2f as u32) << 11;
    dsp.soct_sr |= (fok as u32) << 14;
    dsp.soct_sr |= (gfs as u32) << 15;
    dsp.soct_sr |= (lock as u32) << 16;
    dsp.soct_sr |= (emph as u32) << 17;

    // The first bit of SOCT is output right after latch, *not* after the first pulse of SQCK so we
    // need to update right away
    cdc.uc.set_dsp_subq(dsp.soct_sr & 1 != 0);
}

enum State {
    /// No event pending
    Idle,
    /// We're locked in a busy state until the auto-sequence is cancelled
    BusyFrozen,
    /// Number of audio cycles until the busy state ends
    BusyWait(u32),
    /// Number of audio cycles until the focus sequence ends
    FocusOn(u32),
    /// Number of audio cycles until the Jump sequence ends
    Jump(u32),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum SledDirection {
    /// Towards the outer parts of the disc
    Forward,
    /// Towards the inner parts of the disc
    Reverse,
}

const AUDIO_CYCLES_PER_SCEX_BIT: u8 = (44_100 / 250) as u8;

/// Number of bits in the SCEx string before we loop to the first one. The actual SCEx bytes (+
/// start/stop bits) take less than this number of bits to output, but it's padded with zeroes to
/// match to expected period of the output on the real hardware.
const SCEX_STRING_BITS: usize = 63;

#[test]
fn jump_delay_test() {
    for (tc, d) in TRACK_JUMP_DELAY_TO_XBUSY {
        assert_eq!(jump_delay(tc), d);
    }

    assert_eq!(jump_delay(1), us_to_audio_cycles(1_150));
    assert_eq!(jump_delay(49), us_to_audio_cycles(6_190));
    assert_eq!(jump_delay(51), us_to_audio_cycles(6_327));
    assert_eq!(jump_delay(70), us_to_audio_cycles(7_052));
    assert_eq!(jump_delay(90), us_to_audio_cycles(7_845));
    assert_eq!(jump_delay(300), us_to_audio_cycles(12_313));
    assert_eq!(jump_delay(500), us_to_audio_cycles(14_399));
    assert_eq!(jump_delay(2_000), us_to_audio_cycles(34_785));
    assert_eq!(jump_delay(10_000), us_to_audio_cycles(173_922));
}
