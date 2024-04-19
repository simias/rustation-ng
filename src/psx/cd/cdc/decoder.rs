//! CXD1815Q (similar to CXD1815Q) emulation

use super::{resampler::AudioResampler, Cdc};
use crate::bitwise::Bitwise;
use cdimage::sector::{XaBitsPerSample, XaCodingAudio, XaSamplingFreq};
use cdimage::Sector;
use std::fmt;

/// CXD1815Q
#[derive(serde::Serialize, serde::Deserialize)]
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
    /// Current minute address
    cmadr: u8,
    /// Decoder last address
    dladr: u16,
    /// Host address
    hadr: u16,
    /// Host address counter
    hadrc: u16,
    /// Host transfer
    hxfr: u16,
    /// Host transfer counter
    hxfrc: u16,
    /// Disable host transfer counter
    dishxfrc: bool,
    /// Writing to the RAM buffer is stopped
    write_stopped: bool,
    /// Counter for the DECTOUT IRQ
    dectout_state: DecTOutState,
    /// For HDR register: contains the 4 CD-ROM header bytes
    hdr: HdrBuf,
    /// For SHDR register: contains the 4 CD-ROM XA subheader bytes
    shdr: HdrBuf,
    /// ECC status register
    eccsts: u8,
    /// Host interrupt status
    hintsts: u8,
    /// Host interrupt mask
    hintmsk: u8,
    /// Attenuation value 0 to 3
    atv: [u8; 4],
    /// Attenuation value 0 to 3 (pending application through ADPCTL)
    atv_pending: [u8; 4],
    /// Real-time coding information
    rtci: XaCodingAudio,
    /// ADPMUTE
    adp_mute: bool,
    /// Real-time ADPCM playback mute
    rt_mute: bool,
    /// CD-DA playback
    cd_da: bool,
    /// CD-DA mute
    cd_da_mute: bool,
    /// Double speed
    dblspd: bool,
    /// Counter until ADPBUSY goes low (in audio cycles)
    adpcm_busy_counter: u16,
    /// The last two previously decoded ADPCM samples for the left and right channels
    adpcm_last: [[i16; 2]; 2],
    /// Current phase for audio resampling, in 1/7th of a sample. We need this because the XA ADPCM
    /// streams have a frequency of 37.8kHz or 18.9kHz, respectively 6/7 and 3/7 the CD-DA audio
    /// frequency of 44.1kHz
    adpcm_audio_phase: u8,
    /// Temporary buffer holding decoded samples before 44.1kHz resampling
    #[serde(with = "serde_big_array::BigArray")]
    sample_buffer: [[i16; 2]; 4032],
    /// Two resamplers for the left and right stereo channels
    resamplers: [AudioResampler; 2],
    /// RAM used to store decoded sectors
    #[serde(with = "serde_big_array::BigArray")]
    ram: [u8; 32 * 1024],
    /// Host command parameter FIFO
    host_params: HostFifo,
    /// Host command response FIFO
    host_result: HostFifo,
    /// Buffer for output samples before they're sent to the SPU
    output_buffer: OutputBuffer,
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
            hadr: 0,
            hadrc: 0,
            hxfr: 0,
            hxfrc: 0,
            dishxfrc: false,
            dadrc: 0,
            cmadr: 0,
            write_stopped: false,
            dectout_state: DecTOutState::Idle,
            hdr: HdrBuf::new(),
            shdr: HdrBuf::new(),
            eccsts: 0,
            hintsts: 0,
            // Datasheet says that all write registers are 0x00 at reset, except for this one that
            // has the HCRISD set
            hintmsk: 1,
            // The BIOS CD player doesn't set the volume and expects these reset values, so it's
            // important to get them right
            atv: [0x80, 0, 0x80, 0],
            atv_pending: [0; 4],
            rtci: XaCodingAudio(0),
            adp_mute: false,
            rt_mute: false,
            cd_da: false,
            cd_da_mute: false,
            dblspd: false,
            adpcm_busy_counter: 0,
            adpcm_last: [[0; 2]; 2],
            adpcm_audio_phase: 0,
            sample_buffer: [[0; 2]; 4032],
            resamplers: [AudioResampler::new(), AudioResampler::new()],
            ram: [0; 32 * 1024],
            host_params: HostFifo::new(),
            host_result: HostFifo::new(),
            output_buffer: OutputBuffer::new(),
        }
    }

    pub fn is_double_speed(&self) -> bool {
        self.dblspd
    }

    pub fn is_streaming_audio(&self) -> bool {
        !self.output_buffer.is_empty()
    }

    fn host_command(&mut self, cmd: u8) {
        self.host_command = cmd;
        self.command_busy = true;

        const CMD_NAME: [&str; 0x1f] = [
            "Err_0x00",
            "Getstat",
            "Setloc",
            "Play",
            "Forward",
            "Backward",
            "ReadN",
            "MotorOn",
            "Stop",
            "Pause",
            "Init",
            "Mute",
            "Demute",
            "Setfilter",
            "Setmode",
            "Getparam",
            "GetlocL",
            "GetlocP",
            "SetSession",
            "GetTN",
            "GetTD",
            "SeekL",
            "SeekP",
            "Err_0x18",
            "Err_0x19",
            "Test",
            "GetID",
            "ReadS",
            "Reset",
            "GetQ",
            "ReadTOC",
        ];

        let name = CMD_NAME.get(usize::from(cmd)).unwrap_or(&"Unknown");

        cdc_debug!("Host command 0x{:02x} {} {:?}", cmd, name, self.host_params);

        self.irq.trigger(Irq::HstCmnd);
    }

    /// ECC status register
    fn eccsts(&self) -> u8 {
        self.eccsts
    }

    /// Decoder status register
    fn decsts(&self) -> u8 {
        let mut r = 0;

        // RTADPBSY
        r.set_bit(5, self.adpcm_busy_counter > 0);
        // Bit 1 is "SHRTSCT (short sector)"
        // Bit 0 is "NOSYNC"

        r
    }

    /// Host interface status register
    fn hifsts(&self) -> u8 {
        let mut r = 0u8;

        r |= self.hintsts & 7;
        // DMABUSY
        r.set_bit(3, self.hxfrc > 0);
        r.set_bit(4, !self.host_params.is_empty());
        r.set_bit(5, self.host_result.is_empty());
        r.set_bit(6, !self.host_result.is_full());
        r.set_bit(7, self.command_busy);

        r
    }

    /// Clear control register
    fn clrctl(&mut self, v: u8) {
        // CLRBUSY
        if v.bit(6) {
            if self.command_busy {
                cdc_debug!("Host command done, result: {:?}", self.host_result);
            }
            self.command_busy = false;
        }

        // CLRRSLT
        if v.bit(5) {
            self.host_result.clear();
        }
    }

    /// Host status register
    pub fn hsts(&self) -> u8 {
        let mut r = 0;

        r |= self.ra & 3;
        r.set_bit(2, self.adpcm_busy_counter > 0);
        r.set_bit(3, self.host_params.is_empty());
        r.set_bit(4, !self.host_params.is_full());
        r.set_bit(5, !self.host_result.is_empty());
        // DRQSTS
        r.set_bit(6, self.hxfrc > 0);
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

    pub fn adpcm_decode_sector(&mut self, sector_addr: u16) {
        // Data starts after the header + subheader
        let data_start = usize::from(sector_addr + 4 + 8);

        // Each sector contains an "audio block" of 2304B and 20B of padding (that should be 0) for
        // a total of 2324 bytes (the length of an XA Form2 payload)
        let data = &self.ram[data_start..(data_start + 2304)];

        let shift_4bpp = match self.rtci.bits_per_sample() {
            XaBitsPerSample::S4Bits => 1,
            XaBitsPerSample::S8Bits => 0,
        };

        let units_per_group = 4 << shift_4bpp;
        let samples_8bpp = shift_4bpp == 0;

        let stereo = self.rtci.stereo();

        // Total number of samples we're about to decode
        let total_samples = 18 * (4 << shift_4bpp) * 28;

        // 1 for stereo, 0 for mono
        let stereo_one = stereo as usize;

        // Number of generated stereo samples
        let stereo_samples = if stereo {
            total_samples / 2
        } else {
            total_samples
        };

        let audio_frequency = match self.rtci.sampling_frequency() {
            XaSamplingFreq::F18_9 => {
                // Sampling frequency is 18.9kHz, 3/7 * 44.1kHz
                AudioFrequency::Xa18k9
            }
            XaSamplingFreq::F37_8 => {
                // Sampling frequency is 37.8kHz, 6/7 * 44.1kHz
                AudioFrequency::Xa37k8
            }
        };

        if self.rt_mute || self.adp_mute {
            for s in self.sample_buffer[0..usize::from(stereo_samples)].iter_mut() {
                *s = [0, 0];
            }
        } else {
            // Offsets in the output buffer, per channel
            let mut output_offsets = [0; 2];

            // Each audio block contains 18 "sound groups" of 128 bytes
            for group in 0..18 {
                let group_off = 128 * group;
                // Each group has a 16 byte "Sound Parameters" header...
                let sp = &data[group_off..group_off + 16];
                // ... and 112 bytes of "Sample Audio Data"
                let audio_data = &data[group_off + 16..group_off + 128];

                // Each group has between 4 and 8 "Sound Units" depenting on the sample bit depth
                for unit in 0..units_per_group {
                    // The params are stored twice, the second time at the same address | 4
                    let param = sp[((unit << 1) & 8) | (unit & 3)];
                    let shift = param & 0xf;
                    let weights: [(i32, i32); 16] = [
                        (0, 0),
                        (60, 0),
                        (115, -52),
                        (98, -55),
                        (122, -60),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                        (0, 0),
                    ];
                    let (wp, wn) = weights[(param >> 4) as usize];

                    let channel = unit & stereo_one;

                    for i in 0..28 {
                        let encoded = if samples_8bpp {
                            audio_data[(i << 2) | unit]
                        } else {
                            // 4bpp: 2 samples per byte
                            let s = audio_data[(i << 2) | (unit >> 1)];

                            // Convert the sample to 8 bit by setting the low 4 bits to 0
                            if unit & 1 == 0 {
                                s << 4
                            } else {
                                s & 0xf0
                            }
                        };

                        // Convert to signed 16 bits
                        let sample = (u16::from(encoded) << 8) as i16;
                        // Convert to 32bits to handle overflows
                        let mut sample = i32::from(sample);

                        // ADPCM decode
                        sample >>= shift;
                        let sample_1 = i32::from(self.adpcm_last[channel][0]);
                        let sample_2 = i32::from(self.adpcm_last[channel][1]);
                        sample += (sample_1 * wp + sample_2 * wn) >> 6;

                        // Saturate to 16 bits
                        let sample = if sample > i16::max_value() as i32 {
                            i16::max_value()
                        } else if sample < i16::min_value() as i32 {
                            i16::min_value()
                        } else {
                            sample as i16
                        };

                        // Rotate last samples
                        self.adpcm_last[channel][1] = self.adpcm_last[channel][0];
                        self.adpcm_last[channel][0] = sample;

                        // Store the data in the output buffer
                        let sample_off = output_offsets[channel];
                        self.sample_buffer[sample_off][channel] = sample;
                        output_offsets[channel] += 1;
                    }
                }
            }
        }

        let nsamples = self.resample_44100(stereo_samples, stereo, audio_frequency);

        // XXX Not sure about this, I assume that the ADPBUSY flag remains high as long as the last
        // decoded sample hasn't been output to the SPU, but maybe instead the decoding is batched
        // and buffered somewhere?
        self.adpcm_busy_counter = nsamples;
    }

    /// Resample `sample_count` samples from `self.sample_buffer` from `frequency` to 44.1kHz and
    /// queue them for output to the SPU. Also applies the ATV.
    ///
    /// Returns the number of 44.1kHz samples generated.
    fn resample_44100(
        &mut self,
        sample_count: u16,
        stereo: bool,
        frequency: AudioFrequency,
    ) -> u16 {
        let samples = &self.sample_buffer[..usize::from(sample_count)];
        let mut nout_samples = 0;

        let l_to_l = i32::from(self.atv[0]);
        let l_to_r = i32::from(self.atv[1]);
        let r_to_r = i32::from(self.atv[2]);
        let r_to_l = i32::from(self.atv[3]);

        let mix = move |[l, r]: [i16; 2]| -> [i16; 2] {
            let l = i32::from(l);
            let r = i32::from(r);

            let l = (l * l_to_l + r * r_to_l) >> 7;
            let r = (r * l_to_r + r * r_to_r) >> 7;

            let l = if l >= i32::from(i16::MAX) {
                i16::MAX
            } else if l <= i32::from(i16::MIN) {
                i16::MIN
            } else {
                l as i16
            };

            let r = if r >= i32::from(i16::MAX) {
                i16::MAX
            } else if r <= i32::from(i16::MIN) {
                i16::MIN
            } else {
                r as i16
            };

            [l, r]
        };

        match frequency {
            AudioFrequency::Da2x => {
                // We're running at 2 * 44.1kHz, that means that we run at twice the SPU audio
                // frequency and must skip every other sample.
                //
                // XXX Mednafen doesn't resample here, should we? Do games actually use this mode
                // for some reason?
                for &[l, r] in samples.iter().step_by(2) {
                    self.output_buffer.push_sample_44100(mix([l, r]));
                    nout_samples += 1;
                }
            }
            AudioFrequency::Da1x => {
                // We're running at 44.1kHz, in other words we're running at the normal CD-DA
                // frequency and we don't have to resample
                for &[l, r] in samples.iter() {
                    self.output_buffer.push_sample_44100(mix([l, r]));
                    nout_samples += 1;
                }
            }
            AudioFrequency::Xa18k9 | AudioFrequency::Xa37k8 => {
                // We're running at a fraction of 44.1kHz, we need to resample

                // This value is the ratio of the audio frequency to the output frequency of
                // 44.1kHz in multiples of 1/7th. So for instance for Xa37k8 the input frequency
                // is 37.8kHz so phase step will be 6 because 37.8kHz = 6/7 * 44.1kHz
                let phase_step = frequency as u8;

                for &[l, r] in samples.iter() {
                    while self.adpcm_audio_phase < 7 {
                        let l = self.resamplers[0].resample(self.adpcm_audio_phase);
                        let r = if stereo {
                            self.resamplers[1].resample(self.adpcm_audio_phase)
                        } else {
                            l
                        };
                        self.adpcm_audio_phase += phase_step;

                        self.output_buffer.push_sample_44100(mix([l, r]));
                        nout_samples += 1;
                    }

                    self.adpcm_audio_phase -= 7;
                    self.resamplers[0].push_sample(l);
                    self.resamplers[1].push_sample(r);
                }
            }
        }

        nout_samples
    }
}

pub fn run_audio_cycle(cdc: &mut Cdc) {
    match cdc.decoder.adpcm_busy_counter {
        0 => (),
        1 => {
            cdc.decoder.adpcm_busy_counter = 0;
            cdc.decoder.irq.trigger(Irq::RTADPEnd);
        }
        _ => cdc.decoder.adpcm_busy_counter -= 1,
    }

    // Handle decoder timeout IRQ
    match cdc.decoder.dectout_state {
        DecTOutState::Idle => (),
        DecTOutState::Armed {
            ref mut cycles_since_last_sync,
        } => {
            *cycles_since_last_sync += 1;

            // The timeout is set to 3 sectors
            let timeout = (44_100 / if cdc.decoder.dblspd { 150 } else { 75 }) * 3;

            if *cycles_since_last_sync >= timeout {
                cdc.decoder.irq.trigger(Irq::DecTOut);
                cdc.decoder.dectout_state = DecTOutState::Idle;
            }
        }
    }

    match cdc.decoder.state {
        DecoderState::Idle => (),
        DecoderState::SectorPending {
            ref sector,
            ref mut decint_delay,
        } => {
            if *decint_delay > 1 {
                *decint_delay -= 1;
            } else {
                let mode = cdc.decoder.decctl.mode();

                cdc_debug!("Sector read {} {:?}", sector.q().amsf(), mode);

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

                if matches!(
                    mode,
                    DecoderMode::MonitorOnly
                        | DecoderMode::WriteOnly
                        | DecoderMode::RealTimeCorrection
                ) {
                    // Check for sync field
                    let has_sync = raw_data[0] == 0
                        && raw_data[11] == 0
                        && raw_data[1..11].iter().all(|&b| b == 0xff);

                    if has_sync {
                        // Not sure if that's correct or if DECTOUT can only re-trigger when the
                        // sub-CPU writes to DECCTL
                        cdc.decoder.dectout_state.rearm();

                        cdc.decoder.eccsts = 0;

                        if sector.edc_valid() {
                            // Set EDCOK
                            cdc.decoder.eccsts |= 1 << 3;
                            // Set ECCOK
                            cdc.decoder.eccsts |= 1 << 2;
                        } else {
                            unimplemented!("Read sector with EDC errors");
                        }

                        let edc_start;

                        // Update ECCSTS mode
                        cdc.decoder.eccsts |= match (
                            cdc.decoder.decctl.mode_form_sel(),
                            raw_data[15],
                            raw_data[18] & (1 << 5) != 0,
                        ) {
                            // Mode 1
                            (None | Some(0b00), 1, _form) => {
                                edc_start = 2064;
                                0b00
                            }
                            // Mode 2 Form 1
                            (None | Some(0b10), 2, false) => {
                                edc_start = 2072;
                                0b10
                            }
                            // Mode 2 Form 2
                            (None | Some(0b11), 2, true) => {
                                edc_start = 2348;
                                0b11
                            }
                            (None, m, _form) => {
                                // Probably means that it's either not a CD-ROM sector, or it's
                                // corrupted. It could also be Mode 0 which is a CD-ROM sector
                                // that contains no data. According to the datasheet in this
                                // mode the correction is inhibited.
                                warn!("Unhandled sector mode {}", m);
                                edc_start = 2064;

                                // Set CORINH
                                cdc.decoder.eccsts |= 1 << 5;

                                // Not sure what value is used here
                                0b00
                            }
                            // AUTODIST is not set and the configured value does not match the
                            // sector format, this will trigger ECC/EDC errors
                            //
                            // I think the firmware only sets !AUTODIST early after the decoder
                            // reinit because of a bug (it writes 0xb6 to DECCTL instead of *0xb6).
                            // In this case the ECC format is forced to Mode 2, Form 2. Otherwise
                            // AUTODIST should always be set as far as I can tell.
                            (Some(mb), mode, form) => {
                                warn!(
                                    "Incompatible decoder ECC config and sector format: \
                                      {:x} DECCTL{}, mode: {}, form: {}",
                                    cdc.decoder.decctl.0, mb, mode, form
                                );

                                // The following is guesswork

                                // Clear EDCOK
                                cdc.decoder.eccsts &= !(1 << 3);
                                // Clear ECCOK
                                cdc.decoder.eccsts &= !(1 << 2);

                                edc_start = match mb {
                                    // XXX 0b01 is undocumented, so that's a guess
                                    0b00 | 0b01 => 2064,
                                    0b10 => 2072,
                                    0b11 => 2348,
                                    _ => unreachable!(),
                                };

                                mb
                            }
                        };

                        // Update EDCALL0
                        if raw_data[edc_start]
                            | raw_data[edc_start + 1]
                            | raw_data[edc_start + 2]
                            | raw_data[edc_start + 3]
                            == 0
                        {
                            cdc_debug!("ECC @ {} is all zeroes", edc_start);
                            cdc.decoder.eccsts |= 1 << 7;
                        }
                    }
                } else {
                    // Maybe?
                    cdc.decoder.eccsts = 0;
                }

                // Load the sector in RAM. Normally this should certainly not happen all at once
                // but it's good enough for now.
                if mode.write_to_ram() {
                    let mut addr = cdc.decoder.dadrc;

                    // Round the current address to the next KB. I don't actually know if this is
                    // accurate or if it must be done by the sub-CPU. In order for CMADR to work
                    // the sector has to be stored at a kilobyte boundary
                    addr = addr.wrapping_add(0x3ff) & 0x7c00;

                    // Again, not documented, but given the real hardware behaviour it does seem
                    // that there only are 8 sector buffers and the rest is reserved for ADPCM
                    // sound maps. I really need to test this more however, I find it pretty
                    // suspicious.
                    if addr >= 0x6000 {
                        addr = 0;
                    }

                    cdc.decoder.cmadr = (addr >> 10) as u8;

                    // XXX Not sure if this is accurate. Surely it wouldn't make sense to drop 12
                    // bytes from audio sectors? On the other hand I don't think that CD-DA
                    // playback goes through RAM, the DSP is probably just fed directly (the block
                    // diagram in the datasheet also seems to confirm that).
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

                        addr += 1;
                    }

                    cdc.decoder.dadrc = addr;
                }

                if matches!(mode, DecoderMode::CdDa | DecoderMode::Disabled)
                    && cdc.decoder.cd_da
                    && !cdc.decoder.cd_da_mute
                {
                    // Send the sector to the SPU
                    let nsamples = raw_data.len() / 4;

                    for (n, pair) in cdc.decoder.sample_buffer[0..nsamples]
                        .iter_mut()
                        .enumerate()
                    {
                        let raw_pos = n << 2;

                        let left_lo = raw_data[raw_pos] as u16;
                        let left_hi = raw_data[raw_pos + 1] as u16;
                        let right_lo = raw_data[raw_pos + 2] as u16;
                        let right_hi = raw_data[raw_pos + 3] as u16;

                        let left = left_lo | (left_hi << 8);
                        let right = right_lo | (right_hi << 8);

                        pair[0] = left as i16;
                        pair[1] = right as i16;
                    }

                    cdc.decoder.resample_44100(
                        nsamples as u16,
                        true,
                        if cdc.decoder.dblspd {
                            AudioFrequency::Da2x
                        } else {
                            AudioFrequency::Da1x
                        },
                    );
                }

                cdc.decoder.state = DecoderState::Idle;

                refresh_irq(cdc);
            }
        }
    }
}

pub fn get_audio_sample(cdc: &mut Cdc) -> [i16; 2] {
    if cdc.decoder.output_buffer.len() == 1 {
        // We're about to pop the last sample, that probably means that we're no longer streaming
        // audio. Reset the resamplers to make sure that we restart from a clean state.
        //
        // I expect that on the real hardware this isn't necessary because the resamplers run
        // non-stop (resampling silence when nothing is playing), but I'm not sure. The firmware
        // doesn't seem to do anything special when starting a new play command.
        cdc.decoder.resamplers[0].clear();
        cdc.decoder.resamplers[1].clear();
        cdc.decoder.adpcm_audio_phase = 0;
        cdc.decoder.adpcm_last = [[0; 2]; 2];
    }

    cdc.decoder.output_buffer.pop_sample_44100()
}

pub fn sub_cpu_write(cdc: &mut Cdc, addr: u8, val: u8) {
    let decoder = &mut cdc.decoder;

    // trace!("DECODER write: 0x{:02x} = 0x{:02x}", addr, val);

    match addr {
        0x00 => cdc_debug!("DRVIF 0x{:02x}", val),
        // CONFIG1
        0x01 => {
            // The PlayStation normally uses 32Kx8b RAM config
            assert_eq!((val >> 2) & 7, 0, "Unexpected RAM config");
            cdc_debug!("CONFIG_1 0x{:02x}", val);
        }
        // CONFIG2
        0x02 => {
            if val.bit(3) {
                unimplemented!("CONFIG2 SMBF2");
            }
            cdc_debug!("CONFIG_2 0x{:02x}", val);
        }
        0x03 => {
            decoder.decctl = DecCtl(val);

            if matches!(
                decoder.decctl.mode(),
                DecoderMode::MonitorOnly | DecoderMode::WriteOnly | DecoderMode::RealTimeCorrection
            ) {
                decoder.dectout_state.rearm();
            }

            cdc_debug!("DECCTL 0x{:02x} {:?}", val, decoder.decctl.mode());
        }
        0x04 => {
            cdc_debug!("DLADR-L 0x{:02x}", val);
            decoder.dladr &= 0xff00;
            decoder.dladr |= u16::from(val);
            // XXX guess
            decoder.write_stopped = false;
        }
        0x05 => {
            cdc_debug!("DLADR-M 0x{:02x}", val);
            decoder.dladr &= 0xff;
            decoder.dladr |= u16::from(val) << 8;
            // XXX guess
            decoder.write_stopped = false;
        }
        0x06 => {
            cdc_debug!("DLADR-H 0x{:02x}", val);
            // We have a 32K RAM, DLADR-H should always be zero
            assert_eq!(val, 0, "Unexpected DLADR-H");
            // XXX guess
            decoder.write_stopped = false;
        }
        0x07 => {
            cdc_debug!("CHPCTL 0x{:02x}", val);
            decoder.dblspd = val.bit(1);
            decoder.cd_da = val.bit(4);
            decoder.cd_da_mute = val.bit(5);
            decoder.rt_mute = val.bit(6);
        }
        0x09 => {
            if decoder.irq.mask != val {
                cdc_debug!("INTMSK 0x{:02x}", val);
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
            cdc_debug!("HXFR-L 0x{:02x}", val);
            decoder.hxfr &= 0xff00;
            decoder.hxfr |= u16::from(val);
        }
        0x0d => {
            cdc_debug!("HXFR-H 0x{:02x}", val);
            decoder.hxfr &= 0xff;
            decoder.hxfr |= u16::from(val & 0xf) << 8;

            // Bit4 is HADR bit 16, but given the amount of RAM that we have that should always be 0
            if val.bit(4) {
                unimplemented!("HADR bit16 set");
            }

            decoder.dishxfrc = val.bit(7);
        }
        0x0e => {
            cdc_debug!("HADR-L 0x{:02x}", val);
            decoder.hadr &= 0xff00;
            decoder.hadr |= u16::from(val);
        }
        0x0f => {
            cdc_debug!("HADR-M 0x{:02x}", val);
            decoder.hadr &= 0xff;
            decoder.hadr |= u16::from(val) << 8;
        }
        0x10 => {
            cdc_debug!("DADRC-L 0x{:02x}", val);
            decoder.dadrc &= 0xff00;
            decoder.dadrc |= u16::from(val);
            // XXX guess
            decoder.write_stopped = false;
        }
        0x11 => {
            cdc_debug!("DADRC-M 0x{:02x}", val);
            decoder.dadrc &= 0xff;
            decoder.dadrc |= u16::from(val) << 8;
            // XXX guess
            decoder.write_stopped = false;
        }
        0x12 => {
            cdc_debug!("DADRC-H 0x{:02x}", val);
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
                cdc_debug!(
                    "Triggering host IRQ {}, result: {:?}",
                    val & 7,
                    decoder.host_result
                );
            }
        }
        0x17 => decoder.push_result(val),
        0x19 => {
            if val.bit(7) {
                // RTADPEN
                let addr = u16::from(val & 0x7f) << 10;
                decoder.adpcm_decode_sector(addr);
            }
        }
        0x1b => decoder.rtci = XaCodingAudio(val),
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
        // HCHPCTL
        (3, 0) => {
            if v.bit(5) {
                unimplemented!("HCHPCTL SMEN");
            }

            if v.bit(6) {
                unimplemented!("HCHPCTL BFWR");
            }

            if v.bit(7) {
                if decoder.hxfrc == 0 {
                    // Start read
                    decoder.hadrc = decoder.hadr;

                    if decoder.dishxfrc {
                        warn!("Decoder read attempt with DISHXFRC");
                        // Not sure what happens here exactly
                        decoder.hxfrc = 0;
                    } else {
                        decoder.hxfrc = decoder.hxfr;
                        let madr = usize::from(decoder.hadr & 0x7c00);
                        cdc_debug!(
                            "Host DMA read started {} 0x{:04x} {:02x}:{:02x}:{:02x}",
                            decoder.hxfrc,
                            decoder.hadr,
                            decoder.ram[madr],
                            decoder.ram[madr + 1],
                            decoder.ram[madr + 2],
                        );
                    }
                }
            } else {
                // BFRD to zero: force end of transfer
                // XXX Will probably need tweaking if we ever implement BFWR
                decoder.hxfrc = 0;
            }
        }
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
        // ATV0
        (2, 2) => decoder.atv_pending[0] = v,
        // ATV1
        (3, 2) => decoder.atv_pending[1] = v,
        // ATV2
        (1, 3) => decoder.atv_pending[2] = v,
        // ATV3
        (2, 3) => decoder.atv_pending[3] = v,
        // ADPCTL
        (3, 3) => {
            cdc_debug!("ADPCTL");
            if v.bit(5) {
                decoder.atv = decoder.atv_pending;
            }

            decoder.adp_mute = v.bit(0);
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
            // assert!(!decoder.host_result.is_empty());
            let result = decoder.host_result.pop();

            if decoder.host_result.is_empty() {
                // This is not actually used by the firmware, the handler is a nop.
                decoder.irq.trigger(Irq::RsltEmpt);
            }

            result
        }
        // RDDATA
        (2, _) => host_dma_read(cdc),
        // HINTMSK
        (3, 0) => decoder.hintmsk,
        // HINTSTS
        (3, 1) => decoder.hintsts,
        _ => todo!("Host read @ {}:{}", addr, decoder.ra),
    }
}

pub fn host_dma_read(cdc: &mut Cdc) -> u8 {
    let decoder = &mut cdc.decoder;

    let data = decoder.ram[usize::from(decoder.hadrc)];

    match decoder.hxfrc {
        0 => {
            // It seems that on the real hardware when one attempts to read the RX data register
            // while BFRD is low it returns always the same bytes which seems to be located at the
            // *closest* multiple of 8 bytes. I think there's an 8byte buffer behind this register
            // somewhere.
            //
            // I also observe that if I wait too long and a new sector gets read while I'm in the
            // middle of the previous one I can still read the previous sector data up to the next
            // 8byte boundary (need to make more intensive checks). Not that it should matter
            // anyway, it's still garbage as far as the software is concerned.
            warn!("DMA read with HXFRC 0");
        }
        1 => {
            decoder.hxfrc = 0;
            decoder.hadrc += 1;
            decoder.irq.trigger(Irq::HDMACmp);
        }
        n => {
            decoder.hxfrc = n - 1;
            decoder.hadrc += 1;
        }
    }

    data
}

pub fn sub_cpu_read(cdc: &mut Cdc, addr: u8) -> u8 {
    let decoder = &mut cdc.decoder;

    // trace!("DECODER read 0x{:02x}", addr);
    match addr {
        // ECCSTS
        0x00 => decoder.eccsts(),
        // DECSTS
        0x01 => decoder.decsts(),
        // HDRFLG
        // Should report errors (post-correction) on the various header and sub-header bytes
        0x02 => 0,
        // HDR
        0x03 => decoder.hdr.read(),
        // SHDR
        0x04 => decoder.shdr.read(),
        // CMADR
        0x05 => cdc.decoder.cmadr,
        // INTSTS
        0x07 => decoder.irq.status,
        // HIFSTS
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
    //
    // For now I decided to keep it simple and arbitrarily set the delay at 100 cycles all the
    // time, we can improve that later.
    cdc.decoder.state = DecoderState::SectorPending {
        sector: Box::new(sector),
        decint_delay: 100,
    };
}

fn refresh_irq(cdc: &mut Cdc) {
    // Interrupt pin is active low
    let xint_lvl = !cdc.decoder.irq.is_active();

    cdc.uc.set_decoder_xint(xint_lvl);
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
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
        cdc_debug!("Decoder trigger {:?}", irq);
        self.status |= 1u8 << (irq as u8);
    }
}

#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
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

#[derive(serde::Serialize, serde::Deserialize)]
enum DecoderState {
    Idle,
    /// A sector was received from the DSP
    SectorPending {
        sector: Box<Sector>,
        decint_delay: u16,
    },
}

impl DecoderState {
    fn is_idle(&self) -> bool {
        matches!(self, DecoderState::Idle)
    }
}

/// Decoder control register
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, Debug)]
struct DecCtl(u8);

impl DecCtl {
    /// Returns true if DLADR (drive last address) should be enabled
    fn endladr(self) -> bool {
        use DecoderMode::*;

        // Per the datasheet, the ENDLADR function is only effective in these modes
        self.0.bit(7) && matches!(self.mode(), WriteOnly | RealTimeCorrection | CdDa)
    }

    /// If autodist is enabled, returns None, otherwise returns the value of the modesel and
    /// formsel bits
    fn mode_form_sel(self) -> Option<u8> {
        if self.0.bit(3) {
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
#[derive(serde::Serialize, serde::Deserialize)]
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
#[derive(serde::Serialize, serde::Deserialize, Clone)]
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

/// Possible frequencies for CD audio. The values are the ratio of the frequency to the standard
/// 44.1kHz frequency in 1/7th of a sample.
#[derive(Copy, Clone, PartialEq, Eq)]
enum AudioFrequency {
    /// CD-DA (normal CD audio) at 2x drive speed, 2 * 44.1kHz
    #[allow(dead_code)]
    Da2x = 14,
    /// CD-DA (normal CD audio) at 1x drive speed. That's the usual frequency for audio tracks and
    /// the frequency the PSX's SPU works with.
    Da1x = 7,
    /// Compressed CD-ROM XA ADPCM audio sector at 37.8kHz, that is 6/7 * 44.1kHz
    Xa37k8 = 6,
    /// Compressed CD-ROM XA ADPCM audio sector at 18.9kHz, that is 3/7 * 44.1kHz
    Xa18k9 = 3,
}

/// Maximum number of samples in the output buffer.
///
/// Rationale for this value: the maximum number of resampled samples we can get out of a single sector are
/// 9408 (for 4bpp mono 18.9kHz audio).
///
/// Since we have to deal with firmware jitter and the start delay it's important to have a few
/// hundred cycles on top of this to deal with "early" sectors, so I just picked the next power of
/// two which should be more than enough.
///
/// Note that for the OutputBuffer code to work, it *has* to be a power of two
const OUTPUT_BUFFER_SIZE: usize = 16 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct OutputBuffer {
    /// Stereo samples circular buffer
    #[serde(with = "serde_big_array::BigArray")]
    samples: [[i16; 2]; OUTPUT_BUFFER_SIZE],
    /// Write pointer
    write_idx: u16,
    /// Read pointer
    read_idx: u16,
    /// Number of cycles to wait until we start output samples
    start_delay: u16,
}

impl OutputBuffer {
    pub fn new() -> OutputBuffer {
        OutputBuffer {
            samples: [[0; 2]; OUTPUT_BUFFER_SIZE],
            write_idx: 0,
            read_idx: 0,
            start_delay: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.write_idx == self.read_idx
    }

    fn is_full(&self) -> bool {
        self.write_idx == self.read_idx ^ (OUTPUT_BUFFER_SIZE as u16)
    }

    pub fn len(&self) -> u16 {
        let sz = OUTPUT_BUFFER_SIZE as u16;
        (self.write_idx.wrapping_sub(self.read_idx)) & (sz - 1)
    }

    fn push_sample_44100(&mut self, sample: [i16; 2]) {
        if self.is_full() {
            warn!("Decoder output buffer overflow");
        } else {
            let sz = OUTPUT_BUFFER_SIZE as u16;

            let idx = (self.write_idx & (sz - 1)) as usize;

            self.samples[idx] = sample;

            self.write_idx = self.write_idx.wrapping_add(1) & ((sz << 1) - 1);
        }
    }

    fn pop(&mut self) -> [i16; 2] {
        debug_assert!(!self.is_empty());

        let sz = OUTPUT_BUFFER_SIZE as u16;
        let idx = (self.read_idx & (sz - 1)) as usize;
        self.read_idx = self.read_idx.wrapping_add(1) & ((sz << 1) - 1);

        self.samples[idx]
    }

    /// Must be called at 44100Hz
    fn pop_sample_44100(&mut self) -> [i16; 2] {
        // Rationale for start_delay:
        //
        // The emulator code will stream at precisely 75 or 150 (2x) sectors per second. However
        // we only generate ADPCM samples when the emulated firmware triggers RTADPEN, so it will
        // introduce some jitter between sectors. To avoid running out of samples, I add a bit of
        // latency before we start streaming samples in order to have a bit of a buffer to deal
        // with the jitter.
        if self.is_empty() {
            self.start_delay = 44_100 / 150;
            [0, 0]
        } else if self.start_delay == 0 {
            self.pop()
        } else {
            self.start_delay -= 1;
            [0, 0]
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
enum DecTOutState {
    Idle,
    Armed { cycles_since_last_sync: u16 },
}

impl DecTOutState {
    fn rearm(&mut self) {
        *self = DecTOutState::Armed {
            cycles_since_last_sync: 0,
        };
    }
}

#[test]
fn output_buffer_size_pow_2() {
    assert_eq!(
        (OUTPUT_BUFFER_SIZE - 1) & OUTPUT_BUFFER_SIZE,
        0,
        "OUTPUT_BUFFER_SIZE is not a power of two"
    );
}
