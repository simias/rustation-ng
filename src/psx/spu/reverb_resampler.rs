//! Resampler used to resample the samples between 44100Hz and 22050Hz in the reverb module.

use super::saturate_to_i16;

/// Resampler for a single channel
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ReverbResampler {
    /// Circular buffer containing the last received samples. Technically we only need 39 samples
    /// but using 64 makes it quicker to manage the indexes.
    #[serde(with = "serde_big_array::BigArray")]
    buffer: [i16; 64],
    /// Index of the last write.
    index: u8,
}

impl ReverbResampler {
    pub fn new() -> ReverbResampler {
        ReverbResampler {
            buffer: [0; 64],
            index: 0,
        }
    }

    pub fn push_sample(&mut self, sample: i16) {
        self.index = self.index.wrapping_add(1) & 63;
        self.buffer[self.index as usize] = sample;
    }

    pub fn resample(&self) -> i16 {
        let sample_base = self.index.wrapping_sub(FIR_COEFFS.len() as u8);

        let mut r = 0i32;

        for (i, &coeff) in FIR_COEFFS.iter().enumerate() {
            let sample_idx = sample_base.wrapping_add(i as u8) & 63;

            let sample = self.buffer[usize::from(sample_idx)];

            r += i32::from(sample) * i32::from(coeff);
        }

        saturate_to_i16(r >> 15)
    }
}

impl Default for ReverbResampler {
    fn default() -> Self {
        ReverbResampler::new()
    }
}

/// FIR coefficients. Mednafen and No$ use the same (although Mednafen uses an optimization that
/// removes zeroes and special-cases the resampling, not sure if it's worth it).
static FIR_COEFFS: [i16; 39] = [
    -1, 0, 2, 0, -10, 0, 35, 0, -103, 0, 266, 0, -616, 0, 1332, 0, -2960, 0, 10246, 16384, 10246,
    0, -2960, 0, 1332, 0, -616, 0, 266, 0, -103, 0, 35, 0, -10, 0, 2, 0, -1,
];
