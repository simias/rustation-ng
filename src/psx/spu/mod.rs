//! Sound Processing Unit
//!
//! Most of the code is based on Mednafen's implementation

mod fifo;
mod fir;
mod reverb_resampler;

use super::{cd, cpu, irq, sync, AccessWidth, Addressable, CycleCount, Psx};
use fifo::DecoderFifo;
use reverb_resampler::ReverbResampler;
use std::ops::{Index, IndexMut};

const SPUSYNC: sync::SyncToken = sync::SyncToken::Spu;

/// Offset into the SPU internal ram
type RamIndex = u32;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Spu {
    /// RAM index, used for read/writes using CPU or DMA.
    ram_index: RamIndex,
    /// Write index in the capture buffers. There's only one index used for all 4 buffers at any
    /// given time
    capture_index: RamIndex,
    /// If the IRQ is enabled in the control register and the SPU memory is accessed at `irq_addr`
    /// (read *or* write) the interrupt is triggered.
    irq_addr: RamIndex,
    /// True if the interrupt has been triggered and not yet ack'ed
    irq: bool,
    /// Main volume, left
    main_volume_left: Volume,
    /// Main volume, right
    main_volume_right: Volume,
    /// The 24 individual voices
    voices: [Voice; 24],
    /// Which voices should be started (bitfield, one bit per voice)
    voice_start: u32,
    /// Which voices should be stopped (bitfield, one bit per voice)
    voice_stop: u32,
    /// Configures which voices output LFSR noise (bitfield, one bit per voice)
    voice_noise: u32,
    /// Configures which voices are fed to the reverberation module (bitfield, one bit per voice)
    voice_reverb: u32,
    /// Configures which voices are frequency modulated (bitfield, one bit per voice)
    voice_frequency_modulated: u32,
    /// Status bits, cleared on start, set to 1 when loop_end is reached (bitfield, one bit per
    /// voice)
    voice_looped: u32,
    /// Most of the SPU's register behave like a R/W RAM, so to simplify the emulation we just
    /// store most registers in a big buffer
    #[serde(with = "serde_big_array::BigArray")]
    regs: [u16; 320],
    /// SPU internal RAM, 16bit wide
    #[serde(with = "serde_big_array::BigArray")]
    ram: [u16; SPU_RAM_SIZE],
    /// Output audio buffer. Sent to the frontend after each frame, so should be large enough to
    /// store one frame worth of audio samples. Assuming a 50Hz refresh rate @ 44.1kHz that should
    /// be about ~1800 samples per frame at most.
    #[serde(with = "serde_big_array::BigArray")]
    audio_buffer: [i16; 2048],
    /// Write pointer into the audio_buffer
    audio_buffer_index: u32,
    /// Mix volume for the samples coming from the CD, left
    #[serde(default)]
    cd_volume_left: i16,
    /// Mix volume for the samples coming from the CD, right
    #[serde(default)]
    cd_volume_right: i16,
    /// First of the two LFSR counters
    noise_counter1: u16,
    /// Second of the two LFSR counters
    noise_counter2: u8,
    /// Noise Linear Feedback Shift Register
    noise_lfsr: u16,
    /// Mix volume for the samples coming from the reverb, left
    #[serde(default)]
    reverb_out_volume_left: i16,
    /// Mix volume for the samples coming from the reverb, right
    #[serde(default)]
    reverb_out_volume_right: i16,
    /// Start address of the working memory for the reverb
    #[serde(default)]
    reverb_start: RamIndex,
    /// Current index in the working memory for the reverb
    #[serde(default)]
    reverb_index: RamIndex,
    /// Which stereo side should we run reverb on next
    #[serde(default)]
    reverb_run_right: bool,
    /// Reverb input sample downsampler, left
    #[serde(default)]
    reverb_downsampler_left: ReverbResampler,
    /// Reverb input sample downsampler, right
    #[serde(default)]
    reverb_downsampler_right: ReverbResampler,
    /// Reverb output sample upsampler, left
    #[serde(default)]
    reverb_upsampler_left: ReverbResampler,
    /// Reverb outpu sample upsampler, right
    #[serde(default)]
    reverb_upsampler_right: ReverbResampler,
    /// Used to override the emulation and force reverb off
    #[serde(default)]
    reverb_enable_override: bool,
}

impl Spu {
    pub fn new() -> Spu {
        Spu {
            ram_index: 0,
            capture_index: 0,
            irq_addr: 0,
            irq: false,
            main_volume_left: Volume::new(),
            main_volume_right: Volume::new(),
            voices: [
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
                Voice::new(),
            ],
            voice_start: 0,
            voice_stop: 0,
            voice_noise: 0,
            voice_reverb: 0,
            voice_frequency_modulated: 0,
            voice_looped: 0,
            regs: [0; 320],
            ram: [0; SPU_RAM_SIZE],
            audio_buffer: [0; 2048],
            audio_buffer_index: 0,
            cd_volume_left: 0,
            cd_volume_right: 0,
            noise_counter1: 0,
            noise_counter2: 0,
            noise_lfsr: 0,
            reverb_out_volume_left: 0,
            reverb_out_volume_right: 0,
            reverb_start: 0,
            reverb_index: 0,
            reverb_run_right: false,
            reverb_downsampler_left: ReverbResampler::new(),
            reverb_downsampler_right: ReverbResampler::new(),
            reverb_upsampler_left: ReverbResampler::new(),
            reverb_upsampler_right: ReverbResampler::new(),
            reverb_enable_override: true,
        }
    }

    pub fn set_reverb_enable(&mut self, en: bool) {
        self.reverb_enable_override = en
    }

    /// Returns the value of the control register
    fn control(&self) -> u16 {
        self.regs[regmap::CONTROL]
    }

    /// True if the "SPU enable" bit is set in the control register
    fn enabled(&self) -> bool {
        self.control() & (1 << 15) != 0
    }

    fn irq_enabled(&self) -> bool {
        // No$ says that the bit 6 (IRQ9) is "only when bit15=1", I'm not sure what that means.
        // Mednafen doesn't appear to put any condition on the interrupt bit.
        self.control() & (1 << 6) != 0
    }

    /// True if the SPU is muted in the configuration register
    fn muted(&self) -> bool {
        self.control() & (1 << 14) == 0
    }

    /// True if the SPU plays the audio coming from the CD
    fn cd_audio_enabled(&self) -> bool {
        self.control() & 1 != 0
    }

    /// True if the reverberation module is enabled
    fn reverb_enabled(&self) -> bool {
        self.control() & (1 << 7) != 0
    }

    /// True if the audio coming from the CD should be reverberated
    fn cd_audio_reverb(&self) -> bool {
        self.control() & (1 << 2) != 0
    }

    /// Update the status register
    fn update_status(&mut self) {
        let mut status = 0;

        status |= self.control() & 0x3f;
        status |= (self.irq as u16) << 6;

        // Not sure what that's about, copied straight from mednafen. `TRANSFER_CONTROL` is the
        // mystery register that mangles the memory writes if it's not set to 4 (cf. No$)
        if self.regs[regmap::TRANSFER_CONTROL] == 4 {
            // Bit set to true if the capture index targets the high half of the capture buffers
            let capture_high = self.capture_index & 0x100 != 0;

            status |= (capture_high as u16) << 11;
        }

        self.regs[regmap::STATUS] = status;
    }

    /// Returns true if `voice` is configured to output LFSR noise
    fn is_noise(&self, voice: u8) -> bool {
        self.voice_noise & (1 << voice) != 0
    }

    /// Returns true if frequency modulation is enabled for `voice`
    fn is_frequency_modulated(&self, voice: u8) -> bool {
        self.voice_frequency_modulated & (1 << voice) != 0
    }

    /// Returns true if voice should be started
    fn is_voice_started(&self, voice: u8) -> bool {
        self.voice_start & (1 << voice) != 0
    }

    /// Returns true if voice should be stopped
    fn is_voice_stopped(&self, voice: u8) -> bool {
        self.voice_stop & (1 << voice) != 0
    }

    /// Returns true if voice should be fed to the reverberation module
    fn is_voice_reverberated(&self, voice: u8) -> bool {
        self.voice_reverb & (1 << voice) != 0
    }

    /// Advance the noise state machine. Should be called at 44.1kHz
    fn run_noise_cycle(&mut self) {
        let ctrl = self.control();
        let freq_shift = (ctrl >> 10) & 0xf;
        let freq_step = (ctrl >> 8) & 3;

        // XXX This algorithm is taken from Mednafen. No$ has a slightly different implementation.
        let (counter1_inc, counter2_inc) = if freq_shift == 0xf {
            (0x8000, 8)
        } else {
            (2 << freq_shift, (freq_step + 4) as u8)
        };

        self.noise_counter1 = self.noise_counter1.wrapping_add(counter1_inc);
        if self.noise_counter1 & 0x8000 != 0 {
            self.noise_counter1 = 0;

            self.noise_counter2 = self.noise_counter2.wrapping_add(counter2_inc);
            if self.noise_counter2 & 8 != 0 {
                self.noise_counter2 &= 7;

                // Advance the LFSR
                let lfsr = self.noise_lfsr;
                let carry = (lfsr >> 15) ^ (lfsr >> 12) ^ (lfsr >> 11) ^ (lfsr >> 10) ^ 1;
                self.noise_lfsr = (lfsr << 1) | (carry & 1);
            }
        }
    }
}

impl Index<u8> for Spu {
    type Output = Voice;

    fn index(&self, port: u8) -> &Self::Output {
        &self.voices[port as usize]
    }
}

impl IndexMut<u8> for Spu {
    fn index_mut(&mut self, port: u8) -> &mut Self::Output {
        &mut self.voices[port as usize]
    }
}

/// Run the SPU until it's caught up with the CPU
pub fn run(psx: &mut Psx) {
    let mut elapsed = sync::resync(psx, SPUSYNC);

    while elapsed >= SPU_FREQ_DIVIDER {
        elapsed -= SPU_FREQ_DIVIDER;
        run_cycle(psx);
    }

    // If we have some leftover cycles we can just return them to the synchronization module, we'll
    // get them back on the next call to resync
    sync::rewind(psx, SPUSYNC, elapsed);

    // For now force a sync at the next cycle
    sync::next_event(psx, SPUSYNC, SPU_FREQ_DIVIDER - elapsed);
}

/// Get the contents of the sample buffer
pub fn get_samples(psx: &mut Psx) -> &[i16] {
    let end = psx.spu.audio_buffer_index as usize;

    &psx.spu.audio_buffer[..end]
}

/// Clear the sample buffer
pub fn clear_samples(psx: &mut Psx) {
    psx.spu.audio_buffer_index = 0;
}

/// Put the provided stereo pair in the output buffer and flush it if necessary
fn output_samples(psx: &mut Psx, left: i16, right: i16) {
    let idx = psx.spu.audio_buffer_index as usize;

    // If this overflows the frontend isn't reading the samples fast enough
    if psx.spu.audio_buffer.len() > idx + 1 {
        psx.spu.audio_buffer[idx] = left;
        psx.spu.audio_buffer[idx + 1] = right;
        psx.spu.audio_buffer_index += 2;
    } else {
        warn!("Frontend isn't reading our audio samples fast enough");
        // Flush the entire buffer to give us some leeway, better to have one big glitch than many
        // small ones
        psx.spu.audio_buffer_index = 0;
    }
}

/// Emulate one cycle of the SPU
fn run_cycle(psx: &mut Psx) {
    psx.spu.update_status();

    // Sum of the left and right voice volume levels
    let mut left_mix = 0;
    let mut right_mix = 0;
    let mut sweep_factor = 0;

    // Sum of the voices used for reverb
    let mut left_reverb = 0;
    let mut right_reverb = 0;

    for voice in 0..24 {
        let (left, right) = run_voice_cycle(psx, voice, &mut sweep_factor);

        left_mix += left;
        right_mix += right;

        if psx.spu.is_voice_reverberated(voice) {
            left_reverb += left;
            right_reverb += right;
        }
    }

    psx.spu.run_noise_cycle();

    // Voice start/stop should've been processed by `run_voice_cycle`
    psx.spu.voice_start = 0;
    psx.spu.voice_stop = 0;

    if psx.spu.muted() {
        // Mute bit doesn't actually mute CD audio, just the SPU voices.
        left_mix = 0;
        right_mix = 0;

        // Mednafen does this too, I suppose it makes sense?
        left_reverb = 0;
        right_reverb = 0;
    }

    let [cd_left, cd_right] = cd::run_audio_cycle(psx);

    // Write CD audio (pre-volume) to the RAM
    ram_write(psx, psx.spu.capture_index, cd_left as u16);
    ram_write(psx, psx.spu.capture_index | 0x200, cd_right as u16);

    if psx.spu.cd_audio_enabled() {
        let cd_left = (i32::from(cd_left) * i32::from(psx.spu.cd_volume_left)) >> 15;
        let cd_right = (i32::from(cd_right) * i32::from(psx.spu.cd_volume_right)) >> 15;

        left_mix += cd_left;
        right_mix += cd_right;

        if psx.spu.cd_audio_reverb() {
            left_reverb += cd_left;
            right_reverb += cd_right;
        }
    }

    // Reverb
    {
        let reverb_samples = (saturate_to_i16(left_reverb), saturate_to_i16(right_reverb));

        let (reverb_left, reverb_right) = run_reverb_cycle(psx, reverb_samples);

        let reverb_left =
            (i32::from(reverb_left) * i32::from(psx.spu.reverb_out_volume_left)) >> 15;
        let reverb_right =
            (i32::from(reverb_right) * i32::from(psx.spu.reverb_out_volume_right)) >> 15;

        left_mix += reverb_left;
        right_mix += reverb_right;
    }

    left_mix = saturate_to_i16(left_mix) as i32;
    right_mix = saturate_to_i16(right_mix) as i32;

    left_mix = psx.spu.main_volume_left.apply_level(left_mix);
    right_mix = psx.spu.main_volume_right.apply_level(right_mix);

    psx.spu.main_volume_left.run_sweep_cycle();
    psx.spu.main_volume_right.run_sweep_cycle();

    psx.spu.capture_index += 1;
    psx.spu.capture_index &= 0x1ff;

    output_samples(psx, saturate_to_i16(left_mix), saturate_to_i16(right_mix));
}

fn reverb_sample_index(psx: &mut Psx, addr: u16, neg_offset: u32) -> RamIndex {
    let idx = psx.spu.reverb_index + to_ram_index(addr) - neg_offset;

    if idx <= 0x3_ffff {
        idx
    } else {
        // Overflow, wrap around to the start of the reverb working area
        psx.spu.reverb_start.wrapping_add(idx) & 0x3_ffff
    }
}

fn store_reverb_sample(psx: &mut Psx, addr: u16, v: i16) {
    let idx = reverb_sample_index(psx, addr, 0);

    ram_write(psx, idx, v as u16)
}

fn load_reverb_sample(psx: &mut Psx, addr: u16) -> i16 {
    let idx = reverb_sample_index(psx, addr, 0);

    ram_read(psx, idx) as i16
}

fn load_reverb_sample_before(psx: &mut Psx, addr: u16) -> i16 {
    let idx = reverb_sample_index(psx, addr, 1);

    ram_read(psx, idx) as i16
}

/// Advance the reverb state machine. Should be called at 44.1kHz with the new reverb samples.
fn run_reverb_cycle(psx: &mut Psx, (left_in, right_in): (i16, i16)) -> (i16, i16) {
    // Reverb downsamples from 44.1Khz to 22.05kHz using a simple FIR filter
    psx.spu.reverb_downsampler_left.push_sample(left_in);
    psx.spu.reverb_downsampler_right.push_sample(right_in);

    fn iir_mul(a: i16, b: i16) -> i32 {
        (if a > i16::MIN {
            (32768 - i32::from(a)) * i32::from(b)
        } else if b > i16::MIN {
            i32::from(b) * 32768
        } else {
            0
        }) >> 14
    }

    if psx.spu.reverb_enabled() && psx.spu.reverb_enable_override {
        if psx.spu.reverb_run_right {
            // IIR processing
            let sample = i32::from(psx.spu.reverb_downsampler_right.resample());

            let in_mix =
                (sample * i32::from(psx.spu.regs[regmap::REVERB_INPUT_VOLUME_RIGHT] as i16)) >> 15;

            let reflect_vol = i32::from(psx.spu.regs[regmap::REVERB_REFLECT_VOLUME2] as i16);

            let same_side_sample = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_SAME_RIGHT2],
            ));
            let same_side_mix = (same_side_sample * reflect_vol) >> 15;

            let diff_side_sample = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_DIFF_RIGHT2],
            ));
            let diff_side_mix = (diff_side_sample * reflect_vol) >> 15;

            let input_same = saturate_to_i16(same_side_mix + in_mix);
            let input_diff = saturate_to_i16(diff_side_mix + in_mix);

            let reflect_iir_vol = psx.spu.regs[regmap::REVERB_REFLECT_VOLUME1] as i16;
            let input_same_alpha = (i32::from(input_same) * i32::from(reflect_iir_vol)) >> 14;
            let input_diff_alpha = (i32::from(input_diff) * i32::from(reflect_iir_vol)) >> 14;

            let iir_same = saturate_to_i16(
                (input_same_alpha
                    + iir_mul(
                        reflect_iir_vol,
                        load_reverb_sample_before(
                            psx,
                            psx.spu.regs[regmap::REVERB_REFLECT_SAME_RIGHT1],
                        ),
                    ))
                    >> 1,
            );
            let iir_diff = saturate_to_i16(
                (input_diff_alpha
                    + iir_mul(
                        reflect_iir_vol,
                        load_reverb_sample_before(
                            psx,
                            psx.spu.regs[regmap::REVERB_REFLECT_DIFF_RIGHT1],
                        ),
                    ))
                    >> 1,
            );

            store_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_SAME_RIGHT1],
                iir_same,
            );
            store_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_DIFF_RIGHT1],
                iir_diff,
            );

            let early_echo = saturate_to_i16(
                (((i32::from(load_reverb_sample(
                    psx,
                    psx.spu.regs[regmap::REVERB_COMB_RIGHT1],
                )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME1] as i16))
                    >> 14)
                    + ((i32::from(load_reverb_sample(
                        psx,
                        psx.spu.regs[regmap::REVERB_COMB_RIGHT2],
                    )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME2] as i16))
                        >> 14)
                    + ((i32::from(load_reverb_sample(
                        psx,
                        psx.spu.regs[regmap::REVERB_COMB_RIGHT3],
                    )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME3] as i16))
                        >> 14)
                    + ((i32::from(load_reverb_sample(
                        psx,
                        psx.spu.regs[regmap::REVERB_COMB_RIGHT4],
                    )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME4] as i16))
                        >> 14))
                    >> 1,
            );

            let apf_in1 = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_APF_RIGHT1]
                    .wrapping_add(psx.spu.regs[regmap::REVERB_APF_OFFSET1]),
            ));
            let apf_in2 = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_APF_RIGHT2]
                    .wrapping_add(psx.spu.regs[regmap::REVERB_APF_OFFSET2]),
            ));

            let apf_vol1 = i32::from(psx.spu.regs[regmap::REVERB_APF_VOLUME1] as i16);
            let apf_vol2 = i32::from(psx.spu.regs[regmap::REVERB_APF_VOLUME2] as i16);

            let out_1 = saturate_to_i16(i32::from(early_echo) - ((apf_in1 * apf_vol1) >> 15));
            let out_2 = saturate_to_i16(
                ((i32::from(early_echo) * apf_vol1) >> 15)
                    - ((apf_in1 * -apf_vol1) >> 15)
                    - ((apf_in2 * apf_vol2) >> 15),
            );

            store_reverb_sample(psx, psx.spu.regs[regmap::REVERB_APF_RIGHT1], out_1);
            store_reverb_sample(psx, psx.spu.regs[regmap::REVERB_APF_RIGHT2], out_2);

            psx.spu.reverb_upsampler_left.push_sample(0);
            psx.spu
                .reverb_upsampler_right
                .push_sample(saturate_to_i16((i32::from(out_1) + i32::from(out_2)) >> 1));
        } else {
            // IIR processing
            let sample = i32::from(psx.spu.reverb_downsampler_left.resample());

            let in_mix =
                (sample * i32::from(psx.spu.regs[regmap::REVERB_INPUT_VOLUME_LEFT] as i16)) >> 15;

            let reflect_vol = i32::from(psx.spu.regs[regmap::REVERB_REFLECT_VOLUME2] as i16);

            let same_side_sample = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_SAME_LEFT2],
            ));
            let same_side_mix = (same_side_sample * reflect_vol) >> 15;

            let diff_side_sample = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_DIFF_LEFT2],
            ));
            let diff_side_mix = (diff_side_sample * reflect_vol) >> 15;

            let input_same = saturate_to_i16(same_side_mix + in_mix);
            let input_diff = saturate_to_i16(diff_side_mix + in_mix);

            let reflect_iir_vol = psx.spu.regs[regmap::REVERB_REFLECT_VOLUME1] as i16;
            let input_same_alpha = (i32::from(input_same) * i32::from(reflect_iir_vol)) >> 14;
            let input_diff_alpha = (i32::from(input_diff) * i32::from(reflect_iir_vol)) >> 14;

            let iir_same = saturate_to_i16(
                (input_same_alpha
                    + iir_mul(
                        reflect_iir_vol,
                        load_reverb_sample_before(
                            psx,
                            psx.spu.regs[regmap::REVERB_REFLECT_SAME_LEFT1],
                        ),
                    ))
                    >> 1,
            );
            let iir_diff = saturate_to_i16(
                (input_diff_alpha
                    + iir_mul(
                        reflect_iir_vol,
                        load_reverb_sample_before(
                            psx,
                            psx.spu.regs[regmap::REVERB_REFLECT_DIFF_LEFT1],
                        ),
                    ))
                    >> 1,
            );

            store_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_SAME_LEFT1],
                iir_same,
            );
            store_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_REFLECT_DIFF_LEFT1],
                iir_diff,
            );

            let early_echo = saturate_to_i16(
                (((i32::from(load_reverb_sample(
                    psx,
                    psx.spu.regs[regmap::REVERB_COMB_LEFT1],
                )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME1] as i16))
                    >> 14)
                    + ((i32::from(load_reverb_sample(
                        psx,
                        psx.spu.regs[regmap::REVERB_COMB_LEFT2],
                    )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME2] as i16))
                        >> 14)
                    + ((i32::from(load_reverb_sample(
                        psx,
                        psx.spu.regs[regmap::REVERB_COMB_LEFT3],
                    )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME3] as i16))
                        >> 14)
                    + ((i32::from(load_reverb_sample(
                        psx,
                        psx.spu.regs[regmap::REVERB_COMB_LEFT4],
                    )) * i32::from(psx.spu.regs[regmap::REVERB_COMB_VOLUME4] as i16))
                        >> 14))
                    >> 1,
            );

            // All-pass filter
            let apf_in1 = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_APF_LEFT1]
                    .wrapping_add(psx.spu.regs[regmap::REVERB_APF_OFFSET1]),
            ));
            let apf_in2 = i32::from(load_reverb_sample(
                psx,
                psx.spu.regs[regmap::REVERB_APF_LEFT2]
                    .wrapping_add(psx.spu.regs[regmap::REVERB_APF_OFFSET2]),
            ));

            let apf_vol1 = i32::from(psx.spu.regs[regmap::REVERB_APF_VOLUME1] as i16);
            let apf_vol2 = i32::from(psx.spu.regs[regmap::REVERB_APF_VOLUME2] as i16);

            let out_1 = saturate_to_i16(i32::from(early_echo) - ((apf_in1 * apf_vol1) >> 15));
            let out_2 = saturate_to_i16(
                ((i32::from(early_echo) * apf_vol1) >> 15)
                    - ((apf_in1 * -apf_vol1) >> 15)
                    - ((apf_in2 * apf_vol2) >> 15),
            );

            store_reverb_sample(psx, psx.spu.regs[regmap::REVERB_APF_LEFT1], out_1);
            store_reverb_sample(psx, psx.spu.regs[regmap::REVERB_APF_LEFT2], out_2);

            psx.spu
                .reverb_upsampler_left
                .push_sample(saturate_to_i16((i32::from(out_1) + i32::from(out_2)) >> 1));
            psx.spu.reverb_upsampler_right.push_sample(0);
        }
    }

    if psx.spu.reverb_run_right {
        psx.spu.reverb_index = psx.spu.reverb_index.wrapping_add(1);
        if psx.spu.reverb_index > 0x3_ffff {
            psx.spu.reverb_index = psx.spu.reverb_start;
        }
    }
    psx.spu.reverb_run_right = !psx.spu.reverb_run_right;

    let reverb_left = psx.spu.reverb_upsampler_left.resample();
    let reverb_right = psx.spu.reverb_upsampler_right.resample();

    (reverb_left, reverb_right)
}

/// Run `voice` for one cycle and return a pair of stereo samples
fn run_voice_cycle(psx: &mut Psx, voice: u8, sweep_factor: &mut i32) -> (i32, i32) {
    // There's no "enable" flag for the voices, they're effectively always running. Unused voices
    // are just muted. Beyond that the ADPCM decoder is always running, even when the voice is in
    // "noise" mode and the output isn't used. This is important when the SPU interrupt is enabled.
    run_voice_decoder(psx, voice);

    let raw_sample = if psx.spu.is_noise(voice) {
        (psx.spu.noise_lfsr as i16) as i32
    } else {
        psx.spu[voice].next_raw_sample()
    };

    let sample = psx.spu[voice].apply_enveloppe(raw_sample);

    // Voices 1 and 3 write their samples back into SPU RAM (what No$ refers to as "capture")
    if voice == 1 {
        ram_write(psx, 0x400 | psx.spu.capture_index, sample as u16);
    } else if voice == 3 {
        ram_write(psx, 0x600 | psx.spu.capture_index, sample as u16);
    }

    let (left, right) = psx.spu[voice].apply_stereo(sample);

    psx.spu[voice].run_sweep_cycle();

    if psx.spu[voice].start_delay > 0 {
        // We're still in the start delay, we don't run the envelope or frequency sweep yet
        psx.spu[voice].start_delay -= 1;
    } else {
        psx.spu[voice].run_envelope_cycle();

        let mut step = u32::from(psx.spu[voice].step_length);

        if psx.spu.is_frequency_modulated(voice) {
            // Voice 0 cannot be frequency modulated
            debug_assert!(voice != 0);

            let mut s = step as i32;

            s += (s * *sweep_factor) >> 15;

            // XXX What happens if s is negative here?
            step = s as u32;
        }

        let step = if step > 0x3fff { 0x3fff } else { step as u16 };

        psx.spu[voice].consume_samples(step);
    }

    if psx.spu.is_voice_stopped(voice) {
        psx.spu[voice].release();
    }

    if psx.spu.is_voice_started(voice) {
        psx.spu[voice].restart();
        psx.spu.voice_looped &= !(1 << voice);
    }

    if !psx.spu.enabled() {
        // XXX Mednafen doesn't reset the ADSR divider in this situation
        psx.spu[voice].release();
        psx.spu[voice].mute();
    }

    // Save sweep factor for the next voice
    *sweep_factor = sample;

    (left, right)
}

/// Run the ADPCM decoder for one cycle
fn run_voice_decoder(psx: &mut Psx, voice: u8) {
    // XXX This value of 11 is taken from Mednafen. Technically we only consume 4 samples (at most)
    // per cycle so >= 4 would do the trick but apparently the original hardware decodes ahead.
    // This is important if the IRQ is enabled since it means that it would trigger a bit earlier
    // when the block is read.
    //
    // This is still not entirely cycle accurate, so it could be further improved with more
    // testing. Mednafen's codebase has a few comments giving hints on what could be done. More
    // testing required.
    if psx.spu[voice].decoder_fifo.len() >= 11 {
        // We have enough data in the decoder FIFO, no need to decode more
        if psx.spu.irq_enabled() {
            // Test prev address
            let prev = psx.spu[voice].cur_index.wrapping_sub(1) & 0x3_ffff;
            check_for_irq(psx, prev);
            // This is taken from mednafen, not shure why it's necessary
            check_for_irq(psx, prev & 0x3_fff8);
        }
    } else {
        // True if we're starting a new ADPCM block
        let new_block = psx.spu[voice].cur_index % 8 == 0;

        if new_block {
            // Check if looping has been requested in the previous block
            if psx.spu[voice].maybe_loop() {
                psx.spu.voice_looped |= 1 << voice;

                // Mednafen doesn't apply the "release and mute" block flag if we're in noise
                // mode. No$ doesn't seem to mention this corner case, but I suppose that it makes
                // sense to ignore decoder envelope changes if we don't use the data.
                if !psx.spu.is_noise(voice) {
                    psx.spu[voice].maybe_release();
                }
            }
        }

        if psx.spu.irq_enabled() {
            // Test current address
            check_for_irq(psx, psx.spu[voice].cur_index);
            // This is taken from mednafen, not sure why it's necessary
            check_for_irq(psx, psx.spu[voice].cur_index & 0x3_fff8);
        }

        if new_block {
            // We're starting a new block
            let header = ram_read_no_irq(psx, psx.spu[voice].cur_index);

            psx.spu[voice].set_block_header(header);
            psx.spu[voice].next_index();
        }

        // Decode 4 samples
        let encoded = ram_read_no_irq(psx, psx.spu[voice].cur_index);
        psx.spu[voice].next_index();
        psx.spu[voice].decode(encoded);
    }
}

/// Handle DMA writes
pub fn dma_store(psx: &mut Psx, v: u32) {
    let w1 = v as u16;
    let w2 = (v >> 16) as u16;

    // XXX Mednafen only checks for IRQ after the 2nd word.
    transfer(psx, w1);
    transfer(psx, w2);
}

/// Handle DMA reads
pub fn dma_load(psx: &mut Psx) -> u32 {
    let w1 = ram_read(psx, psx.spu.ram_index) as u32;
    psx.spu.ram_index = (psx.spu.ram_index + 1) & 0x3_ffff;
    let w2 = ram_read(psx, psx.spu.ram_index) as u32;
    psx.spu.ram_index = (psx.spu.ram_index + 1) & 0x3_ffff;

    check_for_irq(psx, psx.spu.ram_index);

    w1 | (w2 << 16)
}

pub fn store<T: Addressable>(psx: &mut Psx, off: u32, val: T) {
    match T::width() {
        AccessWidth::Word => {
            // Word writes behave like two u16
            let v = val.as_u32();
            store16(psx, off | 2, (v >> 16) as u16);
            // XXX *Sometimes* on the real hardware this 2nd write doesn't pass. I'm not really
            // sure what causes it exactly, sometimes after 32bit writes the lower half of the
            // register keeps its old value. I suspect that these two consecutive 16bit writes
            // can be interrupted in between sometimes, but I'm not really sure by what or in what
            // circumstances.
            store16(psx, off, v as u16);
        }
        AccessWidth::HalfWord => store16(psx, off, val.as_u16()),
        AccessWidth::Byte => {
            if off & 1 != 0 {
                // Byte writes that aren't 16bit aligned don't do anything
                warn!(
                    "SPU write isn't 16bit-aligned: *0x{:x} = 0x{:x}",
                    off,
                    val.as_u32()
                );
                return;
            }
            // In my tests halfword-aligned byte writes are handled exactly like Halfword writes,
            // they even write the full 16bit register value
            // XXX refactor our access code to handle that properly
            unimplemented!("Byte SPU store!");
        }
    }
}

fn store16(psx: &mut Psx, off: u32, val: u16) {
    let val = val.as_u16();

    let index = (off >> 1) as usize;

    psx.spu.regs[index] = val;

    if index < 0xc0 {
        // Voice configuration
        let voice = &mut psx.spu.voices[index >> 3];

        match index & 7 {
            regmap::voice::VOLUME_LEFT => voice.volume_left.set_config(val),
            regmap::voice::VOLUME_RIGHT => voice.volume_right.set_config(val),
            regmap::voice::ADPCM_STEP_LENGTH => voice.step_length = val,
            regmap::voice::ADPCM_START_INDEX => voice.set_start_index(to_ram_index(val)),
            regmap::voice::ADPCM_ADSR_LO => voice.adsr.set_conf_lo(val),
            regmap::voice::ADPCM_ADSR_HI => voice.adsr.set_conf_hi(val),
            regmap::voice::CURRENT_ADSR_VOLUME => voice.set_level(val as i16),
            regmap::voice::ADPCM_REPEAT_INDEX => {
                let loop_index = to_ram_index(val);
                voice.set_loop_index(loop_index);
            }
            _ => (),
        }
    } else if index < 0x100 {
        match index {
            regmap::MAIN_VOLUME_LEFT => psx.spu.main_volume_left.set_config(val),
            regmap::MAIN_VOLUME_RIGHT => psx.spu.main_volume_right.set_config(val),
            regmap::REVERB_VOLUME_LEFT => psx.spu.reverb_out_volume_left = val as i16,
            regmap::REVERB_VOLUME_RIGHT => psx.spu.reverb_out_volume_right = val as i16,
            regmap::VOICE_ON_LO => to_lo(&mut psx.spu.voice_start, val),
            regmap::VOICE_ON_HI => to_hi(&mut psx.spu.voice_start, val),
            regmap::VOICE_OFF_LO => to_lo(&mut psx.spu.voice_stop, val),
            regmap::VOICE_OFF_HI => to_hi(&mut psx.spu.voice_stop, val),
            regmap::VOICE_FM_MOD_EN_LO => {
                // Voice 0 cannot be frequency modulated
                to_lo(&mut psx.spu.voice_frequency_modulated, val & !1);
            }
            regmap::VOICE_FM_MOD_EN_HI => to_hi(&mut psx.spu.voice_frequency_modulated, val),
            regmap::VOICE_NOISE_EN_LO => to_lo(&mut psx.spu.voice_noise, val),
            regmap::VOICE_NOISE_EN_HI => to_hi(&mut psx.spu.voice_noise, val),
            regmap::VOICE_REVERB_EN_LO => to_lo(&mut psx.spu.voice_reverb, val),
            regmap::VOICE_REVERB_EN_HI => to_hi(&mut psx.spu.voice_reverb, val),
            regmap::VOICE_STATUS_LO => to_lo(&mut psx.spu.voice_looped, val),
            regmap::VOICE_STATUS_HI => to_hi(&mut psx.spu.voice_looped, val),
            regmap::REVERB_BASE => {
                let idx = to_ram_index(val);
                psx.spu.reverb_start = idx;
                psx.spu.reverb_index = idx;
            }
            regmap::IRQ_ADDRESS => {
                psx.spu.irq_addr = to_ram_index(val);
                check_for_irq(psx, psx.spu.ram_index);
            }
            regmap::TRANSFER_START_INDEX => {
                psx.spu.ram_index = to_ram_index(val);
                check_for_irq(psx, psx.spu.ram_index);
            }
            regmap::TRANSFER_FIFO => transfer(psx, val),
            regmap::CONTROL => {
                if psx.spu.irq_enabled() {
                    check_for_irq(psx, psx.spu.ram_index);
                } else {
                    // IRQ is acknowledged
                    psx.spu.irq = false;
                    irq::set_low(psx, irq::Interrupt::Spu);
                }
            }
            regmap::TRANSFER_CONTROL => {
                if val != 4 {
                    // According to No$ this register controls the way the data is transferred to
                    // the sound ram and the only value that makes sense is 4 (or more
                    // specifically, bits [3:1] should be 2), otherwise bytes get repeated using
                    // various patterns.
                    warn!("SPU TRANSFER_CONTROL set to 0x{:x}", val);
                }
            }
            regmap::CD_VOLUME_LEFT => psx.spu.cd_volume_left = val as i16,
            regmap::CD_VOLUME_RIGHT => psx.spu.cd_volume_right = val as i16,
            regmap::EXT_VOLUME_LEFT => (),
            regmap::EXT_VOLUME_RIGHT => (),
            // Reverb configuration
            regmap::REVERB_APF_OFFSET1..=regmap::REVERB_INPUT_VOLUME_RIGHT => (),
            _ => warn!(
                "SPU store index {:x} (off = {:x}, abs = {:x}): {:x}",
                index,
                off,
                0x1f80_1c00 + off,
                val
            ),
        }
    } else if index < 0x130 {
        // Set voice level
        let voice_no = (index >> 1) & 0x1f;
        let voice = &mut psx.spu.voices[voice_no];

        let left = index & 1 == 0;

        let level = val as i16;

        if left {
            voice.volume_left.set_level(level);
        } else {
            voice.volume_right.set_level(level);
        };
    }
}

pub fn load<T: Addressable>(psx: &mut Psx, off: u32) -> T {
    let v = match T::width() {
        AccessWidth::Word => {
            let hi = load16(psx, off | 2) as u32;
            let lo = load16(psx, off) as u32;

            lo | (hi << 16)
        }
        AccessWidth::HalfWord => load16(psx, off) as u32,
        AccessWidth::Byte => {
            let mut h = load16(psx, off) as u32;

            // If the byte is not halfword-aligned we read the high byte
            h >>= (off & 1) * 8;

            h & 0xff
        }
    };

    T::from_u32(v)
}

fn load16(psx: &mut Psx, off: u32) -> u16 {
    // This is probably very heavy handed, mednafen only syncs from the CD code and never on
    // register access
    run(psx);

    let index = (off >> 1) as usize;

    let reg_v = psx.spu.regs[index];

    if index < 0xc0 {
        let voice = &psx.spu.voices[index >> 3];

        match index & 7 {
            regmap::voice::CURRENT_ADSR_VOLUME => voice.level() as u16,
            regmap::voice::ADPCM_REPEAT_INDEX => (voice.loop_index >> 2) as u16,
            _ => reg_v,
        }
    } else if index < 0x100 {
        match index {
            regmap::VOICE_STATUS_LO => psx.spu.voice_looped as u16,
            regmap::VOICE_STATUS_HI => (psx.spu.voice_looped >> 16) as u16,
            regmap::TRANSFER_FIFO => unimplemented!(),
            regmap::CURRENT_VOLUME_LEFT => psx.spu.main_volume_left.level() as u16,
            regmap::CURRENT_VOLUME_RIGHT => psx.spu.main_volume_right.level() as u16,
            // Nobody seems to know what this register is for, but mednafen returns 0
            regmap::UNKNOWN => 0,
            _ => reg_v,
        }
    } else if index < 0x130 {
        // Read voice level
        let voice_no = (index >> 1) & 0x1f;
        let voice = &psx.spu.voices[voice_no];

        let left = index & 1 == 0;

        let v = if left {
            voice.volume_left.level()
        } else {
            voice.volume_right.level()
        };

        v as u16
    } else {
        reg_v
    }
}

/// Write the SPU ram at the `ram_index` an increment it.
fn transfer(psx: &mut Psx, val: u16) {
    let i = psx.spu.ram_index;

    ram_write(psx, i, val);

    psx.spu.ram_index = (i + 1) & 0x3_ffff;

    // `ram_write` already checks for interrupt before the write but mednafen immediately rechecks
    // the incremented address after that. Sounds weird but let's go with it for now.
    check_for_irq(psx, psx.spu.ram_index);
}

fn ram_write(psx: &mut Psx, index: RamIndex, val: u16) {
    check_for_irq(psx, index);

    let index = index as usize;

    debug_assert!(index < psx.spu.ram.len());

    psx.spu.ram[index] = val;
}

fn ram_read_no_irq(psx: &mut Psx, index: RamIndex) -> u16 {
    let index = index as usize;

    debug_assert!(index < psx.spu.ram.len());

    psx.spu.ram[index]
}

fn ram_read(psx: &mut Psx, index: RamIndex) -> u16 {
    check_for_irq(psx, index);

    ram_read_no_irq(psx, index)
}

/// Trigger an IRQ if it's enabled in the control register and `addr` is equal to the `irq_addr`
fn check_for_irq(psx: &mut Psx, index: RamIndex) {
    if psx.spu.irq_enabled() && index == psx.spu.irq_addr {
        psx.spu.irq = true;
        irq::set_high(psx, irq::Interrupt::Spu);
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Voice {
    /// Voice volume left
    volume_left: Volume,
    /// Voice volume right
    volume_right: Volume,
    /// Attack Decay Sustain Release envelope
    adsr: Adsr,
    /// This value configures how fast the samples are played on this voice, which effectively
    /// changes the frequency of the output audio.
    ///
    /// The value is a 14 bit fixed point integer with 12 fractional bits
    step_length: u16,
    /// Remaining fractional steps carried between cycles, giving up the effective phase of the
    /// voice. 12 fractional bits.
    phase: u16,
    /// Value `cur_index` will take upon voice start
    start_index: RamIndex,
    /// Current index in SPU RAM for this voice
    cur_index: RamIndex,
    /// Target address for `cur_index` when an ADPCM block requests looping
    loop_index: RamIndex,
    /// True if `loop_index` has been configured through the register interface and any ADPCM loop
    /// block should be ignored.
    loop_index_force: bool,
    /// Header for the current ADPCM block
    block_header: AdpcmHeader,
    /// Last two ADPCM-decoded samples, used to extrapolate the next one
    last_samples: [i16; 2],
    /// FIFO containing the samples that have been decoded but not yet output
    decoder_fifo: DecoderFifo,
    /// Delay (in SPU cycles) between the moment a voice is enabled and the moment the envelope
    /// and frequency functions start running
    start_delay: u8,
}

impl Voice {
    fn new() -> Voice {
        Voice {
            volume_left: Volume::new(),
            volume_right: Volume::new(),
            adsr: Adsr::new(),
            step_length: 0,
            phase: 0,
            start_index: 0,
            cur_index: 0,
            loop_index: 0,
            loop_index_force: false,
            block_header: AdpcmHeader(0),
            last_samples: [0; 2],
            decoder_fifo: DecoderFifo::new(),
            start_delay: 0,
        }
    }

    /// Perform a loop if it was requested by the previously decoded block. Returns `true` if a
    /// loop has taken place
    fn maybe_loop(&mut self) -> bool {
        let do_loop = self.block_header.loop_end();

        if do_loop {
            self.cur_index = self.loop_index & !7;
        }

        do_loop
    }

    /// Release if it was requested by the previously decoded block. Should only be called if the
    /// block also requested looping.
    fn maybe_release(&mut self) {
        debug_assert!(self.block_header.loop_end());
        if self.block_header.loop_release_and_mute() {
            // XXX Mednafen only change the ADSR step and doesn't reset the divider but there's
            // a comment wondering if it should be reset too. To keep the code simpler here I
            // simply call the same function used when a voice is stopped.
            self.adsr.release();
            self.adsr.level = 0;
        }
    }

    /// Increment `cur_index`, wrapping to 0 if we've reached the end of the SPU RAM
    fn next_index(&mut self) {
        self.cur_index = (self.cur_index + 1) % SPU_RAM_SIZE as u32;
    }

    fn set_start_index(&mut self, addr: RamIndex) {
        // From mednafen: apparently the start index is aligned to a multiple of 8 samples
        self.start_index = addr & !7;
    }

    fn set_level(&mut self, level: i16) {
        self.adsr.set_level(level)
    }

    fn level(&self) -> i16 {
        self.adsr.level
    }

    fn set_block_header(&mut self, header: u16) {
        self.block_header = AdpcmHeader(header);

        if !self.loop_index_force && self.block_header.loop_start() {
            self.loop_index = self.cur_index;
        }
    }

    fn set_loop_index(&mut self, loop_index: RamIndex) {
        self.loop_index = loop_index;
        self.loop_index_force = true;
    }

    /// Decode 4 samples from an ADPCM block
    fn decode(&mut self, mut encoded: u16) {
        let (wp, wn) = self.block_header.weights();
        let mut shift = self.block_header.shift();

        // Taken from Mednafen: normally the shift value should be between 0 and 12 since otherwise
        // you lose precision. Apparently when that happens we only keep the sign bit and extend it
        // 8 times.
        //
        // XXX Should probably be tested on real hardware and added as a unit test.
        if shift > 12 {
            encoded &= 0x8888;
            shift = 8;
        }

        // Decode the four 4bit samples
        for i in 0..4 {
            // Extract the 4 bits and convert to signed to get proper sign extension when shifting
            let mut sample = (encoded << (12 - i * 4) & 0xf000) as i16;

            sample >>= shift;

            let mut sample = i32::from(sample);

            // Previous sample
            let sample_1 = i32::from(self.last_samples[0]);
            // Antepenultimate sample
            let sample_2 = i32::from(self.last_samples[1]);

            // Extrapolate with sample -1 using the positive weight
            sample += (sample_1 * wp) >> 6;
            // Extrapolate with sample -2 using the negative weight
            sample += (sample_2 * wn) >> 6;

            let sample = saturate_to_i16(sample);
            self.decoder_fifo.push(sample);

            // Shift `last_samples` for the next sample
            self.last_samples[1] = self.last_samples[0];
            self.last_samples[0] = sample;
        }
    }

    /// Returns the next "raw" decoded sample for this voice, meaning the post-ADPCM decode and
    /// resampling but pre-ADSR.
    fn next_raw_sample(&self) -> i32 {
        let phase = (self.phase >> 4) as u8;
        let samples = [
            self.decoder_fifo[0],
            self.decoder_fifo[1],
            self.decoder_fifo[2],
            self.decoder_fifo[3],
        ];

        fir::filter(phase, samples)
    }

    /// Run one cycle for the ADSR envelope function
    fn run_envelope_cycle(&mut self) {
        self.adsr.run_cycle();
    }

    fn run_sweep_cycle(&mut self) {
        self.volume_left.run_sweep_cycle();
        self.volume_right.run_sweep_cycle();
    }

    /// Apply the Attack Decay Sustain Release envelope to a sample
    fn apply_enveloppe(&self, sample: i32) -> i32 {
        let level = i32::from(self.adsr.level);

        (sample * level) >> 15
    }

    /// Apply left and right volume levels
    fn apply_stereo(&self, sample: i32) -> (i32, i32) {
        (
            self.volume_left.apply_level(sample),
            self.volume_right.apply_level(sample),
        )
    }

    /// Reinitialize voice
    fn restart(&mut self) {
        self.adsr.attack();
        self.phase = 0;
        self.cur_index = self.start_index & !7;
        self.block_header = AdpcmHeader(0);
        self.last_samples = [0; 2];
        self.decoder_fifo.clear();
        self.start_delay = 4;
        self.loop_index_force = false;
    }

    /// Put the ADSR enveloppe in "release" state if it's not already
    fn release(&mut self) {
        self.adsr.release();
    }

    /// Set the envelope's volume to 0
    fn mute(&mut self) {
        self.adsr.level = 0;
    }

    fn consume_samples(&mut self, step: u16) {
        let step = self.phase + step;

        // Update phase with the remaining fractional part
        self.phase = step & 0xfff;

        // Consume samples as needed
        let consumed = step >> 12;
        self.decoder_fifo.discard(consumed as usize);
    }
}

/// Saturating cast from i32 to i16
fn saturate_to_i16(v: i32) -> i16 {
    if v < i32::from(i16::min_value()) {
        i16::min_value()
    } else if v > i32::from(i16::max_value()) {
        i16::max_value()
    } else {
        v as i16
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Volume {
    level: i16,
    config: VolumeConfig,
}

impl Volume {
    fn new() -> Volume {
        Volume {
            level: 0,
            config: VolumeConfig::Fixed(0),
        }
    }

    fn set_config(&mut self, conf: u16) {
        let fixed = conf & 0x8000 == 0;

        self.config = if fixed {
            let level = (conf << 1) as i16;

            VolumeConfig::Fixed(level)
        } else {
            // XXX TODO
            VolumeConfig::Sweep(EnvelopeParams::new())
        };
        // XXX should we update self.level right now? Mefnaden waits for the next call to
        // run_sweep_cycle but that takes place after the level is read.
    }

    fn level(&self) -> i16 {
        self.level
    }

    fn set_level(&mut self, level: i16) {
        self.level = level
    }

    /// Apply current level to a sound sample
    fn apply_level(&self, sample: i32) -> i32 {
        let level = self.level as i32;

        (sample * level) >> 15
    }

    fn run_sweep_cycle(&mut self) {
        self.level = match self.config {
            VolumeConfig::Fixed(l) => l,
            VolumeConfig::Sweep(_) => unimplemented!(),
        };
    }
}

/// Volume configuration, either fixed or a sweep
#[derive(serde::Serialize, serde::Deserialize)]
enum VolumeConfig {
    /// Fixed volume
    Fixed(i16),
    /// Sweep
    Sweep(EnvelopeParams),
}

/// Attack Decay Sustain Release envelope
#[derive(serde::Serialize, serde::Deserialize)]
struct Adsr {
    state: AdsrState,
    /// Current audio level for this envelope
    level: i16,
    /// Divider used to count until the next envelope step
    divider: u16,
    /// Pre-computed envelope parameters for all 4 ADSR states
    params: [EnvelopeParams; 4],
    /// Volume level used to trigger the switch from Decay to Sustain mode
    sustain_level: i16,
    /// Config register value
    config: AdsrConfig,
}

impl Adsr {
    fn new() -> Adsr {
        let mut adsr = Adsr {
            state: AdsrState::Attack,
            level: 0,
            divider: 0,
            params: [
                EnvelopeParams::new(),
                EnvelopeParams::new(),
                EnvelopeParams::new(),
                EnvelopeParams::new(),
            ],
            sustain_level: 0,
            config: AdsrConfig::new(),
        };

        // Not really needed but it's probably cleaner to make sure that `params` and `config`
        // remain always in sync
        adsr.refresh_params();

        adsr
    }

    fn set_level(&mut self, level: i16) {
        self.level = level;
    }

    fn run_cycle(&mut self) {
        let params = &self.params[self.state as usize];

        let div_step = params.compute_divider_step(self.level);
        debug_assert!(div_step > 0);

        // `div_step`'s max value should be 0x8000, so the addition should never overflow
        debug_assert!(div_step <= 0x8000);
        self.divider += div_step;

        if self.divider < 0x8000 {
            // We haven't reached the next step yet.
            return;
        }

        // Next step reached
        self.divider = 0;

        let level_step = params.compute_level_step(self.level);

        // According to Mednafen's code negative audio levels (normally only possible through a
        // manual write to the register) are treated as underflows *except* during the attack.
        // It's unlikely to occur in practice because the level is reset to 0 at the start of
        // the attack, so the only way this can happen is if the level is rewritten while the
        // attack is in progress.
        //
        // XXX That's probably worth a double-check on the real hardware
        if self.state == AdsrState::Attack {
            self.level = match self.level.checked_add(level_step) {
                Some(l) => l,
                None => {
                    // Overflow
                    self.state = AdsrState::Decay;
                    i16::max_value()
                }
            }
        } else {
            self.level = self.level.wrapping_add(level_step);

            if self.level < 0 {
                // Overflow or underflow
                self.level = if level_step > 0 { i16::max_value() } else { 0 };
            }
        }

        if self.state == AdsrState::Decay && self.level <= self.sustain_level {
            self.state = AdsrState::Sustain;
        }
    }

    /// Refresh the pre-computed `params`
    fn refresh_params(&mut self) {
        self.sustain_level = self.config.sustain_level();
        self.params[AdsrState::Attack as usize] = self.config.attack_params();
        self.params[AdsrState::Decay as usize] = self.config.decay_params();
        self.params[AdsrState::Sustain as usize] = self.config.sustain_params();
        self.params[AdsrState::Release as usize] = self.config.release_params();
    }

    fn set_conf_lo(&mut self, v: u16) {
        self.config.set_lo(v);
        self.refresh_params();
    }

    fn set_conf_hi(&mut self, v: u16) {
        self.config.set_hi(v);
        self.refresh_params();
    }

    fn release(&mut self) {
        self.divider = 0;
        self.state = AdsrState::Release;
    }

    fn attack(&mut self) {
        self.divider = 0;
        self.state = AdsrState::Attack;
        self.level = 0;
    }
}

/// Parameters used to configure an envelope function
#[derive(serde::Serialize, serde::Deserialize)]
struct EnvelopeParams {
    /// Base divider step value (how fast do we reach the next step).
    divider_step: u16,
    /// Base level step value
    level_step: i16,
    /// Envelope mode that modifies the way the steps are calculated
    mode: EnvelopeMode,
}

impl EnvelopeParams {
    fn new() -> EnvelopeParams {
        EnvelopeParams {
            divider_step: 0,
            level_step: 0,
            mode: EnvelopeMode::Linear,
        }
    }

    /// Compute (divider_step, level_step) for the given `shift` and `step` values
    fn steps(shift: u32, step: i8) -> (u16, i16) {
        let step = step as i16;

        if shift < 11 {
            (0x8000, step << (11 - shift))
        } else {
            let div_shift = shift - 11;

            if div_shift <= 15 {
                (0x8000 >> div_shift, step)
            } else {
                (1, step)
            }
        }
    }

    /// Compute the parameters for smooth mode
    fn smooth_mode(step: u32, base_divider: u16, base_level: i16) -> EnvelopeMode {
        let mut smooth_divider = if step > 10 && base_divider > 3 {
            base_divider >> 2
        } else if step >= 10 && base_divider > 1 {
            base_divider >> 1
        } else {
            base_divider
        };

        if smooth_divider == 0 {
            smooth_divider = 1;
        }

        let smooth_level = if step < 10 {
            base_level >> 2
        } else if step == 10 {
            base_level >> 1
        } else {
            base_level
        };

        EnvelopeMode::SmoothUp(smooth_divider, smooth_level)
    }

    fn compute_divider_step(&self, cur_level: i16) -> u16 {
        if let EnvelopeMode::SmoothUp(smooth_divider_step, _) = self.mode {
            if cur_level >= 0x6000 {
                return smooth_divider_step;
            }
        }

        self.divider_step
    }

    fn compute_level_step(&self, cur_level: i16) -> i16 {
        match self.mode {
            EnvelopeMode::Linear => self.level_step,
            EnvelopeMode::Exponential => {
                let ls = self.level_step as i32;
                let cl = cur_level as i32;

                ((ls * cl) >> 15) as i16
            }
            EnvelopeMode::SmoothUp(_, smooth_level_step) => {
                if cur_level >= 0x6000 {
                    smooth_level_step
                } else {
                    self.level_step
                }
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
enum EnvelopeMode {
    /// Divider and Volume steps remain the same throughout
    Linear,
    /// Behaves linearly up until volume reaches 0x6000, then the divider_step is replaced by the
    /// first tuple param and the level_step is replaced by the 2nd parameter
    SmoothUp(u16, i16),
    /// Volume steps are multiplied by the current value of the volume, resulting in
    /// exponentially bigger steps (in absolute value)
    Exponential,
}

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct AdsrConfig(u32);

impl AdsrConfig {
    fn new() -> AdsrConfig {
        AdsrConfig(0)
    }

    fn sustain_level(self) -> i16 {
        let sl = self.0 & 0xf;

        let sl = ((sl + 1) << 11) - 1;

        debug_assert!(sl < 0x8000);

        sl as i16
    }

    fn attack_params(self) -> EnvelopeParams {
        let shift = (self.0 >> 10) & 0x1f;
        let step = 7 - ((self.0 >> 8) & 3);
        let exp = (self.0 >> 15) & 1 != 0;

        let (div_step, lvl_step) = EnvelopeParams::steps(shift, step as i8);

        let mode = if exp {
            EnvelopeParams::smooth_mode(step, div_step, lvl_step)
        } else {
            EnvelopeMode::Linear
        };

        EnvelopeParams {
            divider_step: div_step,
            level_step: lvl_step,
            mode,
        }
    }

    fn decay_params(self) -> EnvelopeParams {
        let shift = (self.0 >> 4) & 0xf;
        let step = -8;

        let (div_step, ls) = EnvelopeParams::steps(shift, step);

        EnvelopeParams {
            divider_step: div_step,
            level_step: ls,
            mode: EnvelopeMode::Exponential,
        }
    }

    fn sustain_params(self) -> EnvelopeParams {
        let shift = (self.0 >> 24) & 0x1f;
        let raw_step = 7 - ((self.0 >> 22) & 3);
        let exp = (self.0 >> 31) & 1 != 0;
        let inv_step = (self.0 >> 30) & 1 != 0;

        let step = if inv_step { !raw_step } else { raw_step };

        let (div_step, lvl_step) = EnvelopeParams::steps(shift, step as i8);

        let mode = if exp {
            if inv_step {
                EnvelopeMode::Exponential
            } else {
                EnvelopeParams::smooth_mode(raw_step, div_step, lvl_step)
            }
        } else {
            EnvelopeMode::Linear
        };

        EnvelopeParams {
            divider_step: div_step,
            level_step: lvl_step,
            mode,
        }
    }

    fn release_params(self) -> EnvelopeParams {
        let shift = (self.0 >> 16) & 0x1f;
        let step = -8;
        let exp = (self.0 >> 21) & 1 != 0;

        let (div_step, lvl_step) = EnvelopeParams::steps(shift, step as i8);

        let mode = if exp {
            EnvelopeMode::Exponential
        } else {
            EnvelopeMode::Linear
        };

        EnvelopeParams {
            divider_step: div_step,
            level_step: lvl_step,
            mode,
        }
    }

    fn set_lo(&mut self, v: u16) {
        to_lo(&mut self.0, v);
    }

    fn set_hi(&mut self, v: u16) {
        to_hi(&mut self.0, v);
    }
}

/// Possible ADSR states
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq, Debug)]
enum AdsrState {
    Attack,
    Decay,
    Sustain,
    Release,
}

/// The first two bytes of a 16-byte ADPCM block
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct AdpcmHeader(u16);

impl AdpcmHeader {
    /// If true the current block is the last one of the loop sequence
    fn loop_end(self) -> bool {
        self.0 & (1 << 8) != 0
    }

    /// If true (and loop_end() is also true) we must release the envelope and set the volume
    /// to 0
    fn loop_release_and_mute(self) -> bool {
        // Shouldn't be called if `loop_end` is false
        debug_assert!(self.loop_end());
        self.0 & (1 << 9) == 0
    }

    /// If true the current block is the target for a subsequent loop_end block.
    fn loop_start(self) -> bool {
        self.0 & (1 << 10) != 0
    }

    /// Returns the pair of positive and negative weights described in the header
    fn weights(self) -> (i32, i32) {
        // Weights taken from No$, Mednafen use the same values.
        let w: [(i32, i32); 16] = [
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

        let off = (self.0 >> 4) & 0xf;

        w[off as usize]
    }

    /// Right shift value to apply to extended encoded samples
    fn shift(self) -> u8 {
        (self.0 & 0xf) as u8
    }
}

/// Convert a register value to a ram index
fn to_ram_index(v: u16) -> RamIndex {
    (RamIndex::from(v) << 2) & 0x3_ffff
}

fn to_hi(r: &mut u32, v: u16) {
    let v = u32::from(v);

    *r &= 0xffff;
    *r |= v << 16;
}

fn to_lo(r: &mut u32, v: u16) {
    let v = u32::from(v);

    *r &= 0xffff_0000;
    *r |= v;
}

#[allow(dead_code)]
mod regmap {
    //! SPU register map: offset from the base in number of *halfwords*

    pub mod voice {
        //! Per-voice regmap, repeated 24 times

        pub const VOLUME_LEFT: usize = 0x0;
        pub const VOLUME_RIGHT: usize = 0x1;
        pub const ADPCM_STEP_LENGTH: usize = 0x2;
        pub const ADPCM_START_INDEX: usize = 0x3;
        pub const ADPCM_ADSR_LO: usize = 0x4;
        pub const ADPCM_ADSR_HI: usize = 0x5;
        pub const CURRENT_ADSR_VOLUME: usize = 0x6;
        pub const ADPCM_REPEAT_INDEX: usize = 0x7;
    }

    pub const MAIN_VOLUME_LEFT: usize = 0xc0;
    pub const MAIN_VOLUME_RIGHT: usize = 0xc1;
    pub const REVERB_VOLUME_LEFT: usize = 0xc2;
    pub const REVERB_VOLUME_RIGHT: usize = 0xc3;
    pub const VOICE_ON_LO: usize = 0xc4;
    pub const VOICE_ON_HI: usize = 0xc5;
    pub const VOICE_OFF_LO: usize = 0xc6;
    pub const VOICE_OFF_HI: usize = 0xc7;
    pub const VOICE_FM_MOD_EN_LO: usize = 0xc8;
    pub const VOICE_FM_MOD_EN_HI: usize = 0xc9;
    pub const VOICE_NOISE_EN_LO: usize = 0xca;
    pub const VOICE_NOISE_EN_HI: usize = 0xcb;
    pub const VOICE_REVERB_EN_LO: usize = 0xcc;
    pub const VOICE_REVERB_EN_HI: usize = 0xcd;
    pub const VOICE_STATUS_LO: usize = 0xce;
    pub const VOICE_STATUS_HI: usize = 0xcf;

    pub const REVERB_BASE: usize = 0xd1;
    pub const IRQ_ADDRESS: usize = 0xd2;
    pub const TRANSFER_START_INDEX: usize = 0xd3;
    pub const TRANSFER_FIFO: usize = 0xd4;
    pub const CONTROL: usize = 0xd5;
    pub const TRANSFER_CONTROL: usize = 0xd6;
    pub const STATUS: usize = 0xd7;
    pub const CD_VOLUME_LEFT: usize = 0xd8;
    pub const CD_VOLUME_RIGHT: usize = 0xd9;
    pub const EXT_VOLUME_LEFT: usize = 0xda;
    pub const EXT_VOLUME_RIGHT: usize = 0xdb;
    pub const CURRENT_VOLUME_LEFT: usize = 0xdc;
    pub const CURRENT_VOLUME_RIGHT: usize = 0xdd;
    pub const UNKNOWN: usize = 0xde;

    pub const REVERB_APF_OFFSET1: usize = 0xe0;
    pub const REVERB_APF_OFFSET2: usize = 0xe1;
    pub const REVERB_REFLECT_VOLUME1: usize = 0xe2;
    pub const REVERB_COMB_VOLUME1: usize = 0xe3;
    pub const REVERB_COMB_VOLUME2: usize = 0xe4;
    pub const REVERB_COMB_VOLUME3: usize = 0xe5;
    pub const REVERB_COMB_VOLUME4: usize = 0xe6;
    pub const REVERB_REFLECT_VOLUME2: usize = 0xe7;
    pub const REVERB_APF_VOLUME1: usize = 0xe8;
    pub const REVERB_APF_VOLUME2: usize = 0xe9;
    pub const REVERB_REFLECT_SAME_LEFT1: usize = 0xea;
    pub const REVERB_REFLECT_SAME_RIGHT1: usize = 0xeb;
    pub const REVERB_COMB_LEFT1: usize = 0xec;
    pub const REVERB_COMB_RIGHT1: usize = 0xed;
    pub const REVERB_COMB_LEFT2: usize = 0xee;
    pub const REVERB_COMB_RIGHT2: usize = 0xef;
    pub const REVERB_REFLECT_SAME_LEFT2: usize = 0xf0;
    pub const REVERB_REFLECT_SAME_RIGHT2: usize = 0xf1;
    pub const REVERB_REFLECT_DIFF_LEFT1: usize = 0xf2;
    pub const REVERB_REFLECT_DIFF_RIGHT1: usize = 0xf3;
    pub const REVERB_COMB_LEFT3: usize = 0xf4;
    pub const REVERB_COMB_RIGHT3: usize = 0xf5;
    pub const REVERB_COMB_LEFT4: usize = 0xf6;
    pub const REVERB_COMB_RIGHT4: usize = 0xf7;
    pub const REVERB_REFLECT_DIFF_LEFT2: usize = 0xf8;
    pub const REVERB_REFLECT_DIFF_RIGHT2: usize = 0xf9;
    pub const REVERB_APF_LEFT1: usize = 0xfa;
    pub const REVERB_APF_RIGHT1: usize = 0xfb;
    pub const REVERB_APF_LEFT2: usize = 0xfc;
    pub const REVERB_APF_RIGHT2: usize = 0xfd;
    pub const REVERB_INPUT_VOLUME_LEFT: usize = 0xfe;
    pub const REVERB_INPUT_VOLUME_RIGHT: usize = 0xff;
}

/// SPU RAM size in multiple of 16bit words
const SPU_RAM_SIZE: usize = 256 * 1024;

/// The SPU runs at 44.1kHz, the CD audio frequency, this way no resampling is required
const AUDIO_FREQ_HZ: CycleCount = 44_100;

/// The CPU frequency is an exact multiple of the audio frequency, so the divider is always an
/// integer (0x300 normally)
const SPU_FREQ_DIVIDER: CycleCount = cpu::CPU_FREQ_HZ / AUDIO_FREQ_HZ;
