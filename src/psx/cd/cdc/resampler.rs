/// CD audio resampler for a single channel
#[derive(serde::Serialize, serde::Deserialize)]
pub struct AudioResampler {
    /// Circular buffer containing the last received samples. Technically we only need 25 samples
    /// but using 32 makes it quicker to manage the indexes.
    buffer: [i16; 32],
    /// Index of the last write.
    index: u8,
}

impl AudioResampler {
    pub fn new() -> AudioResampler {
        AudioResampler {
            buffer: [0; 32],
            index: 0,
        }
    }

    pub fn clear(&mut self) {
        self.buffer = [0; 32];
    }

    pub fn resample(&self, phase: u8) -> i16 {
        let coefs = &FIR_COEFFS[phase as usize];

        let mut r = 0;

        for i in 0..25 {
            let buffer_index = (usize::from(self.index) + i) & 0x1f;

            let s = i32::from(self.buffer[buffer_index]);
            let c = i32::from(coefs[i]);

            r += s * c;
        }

        r >>= 15;

        if r > i16::max_value() as i32 {
            i16::max_value()
        } else if r < i16::min_value() as i32 {
            i16::min_value()
        } else {
            r as i16
        }
    }

    pub fn push_sample(&mut self, sample: i16) {
        self.index = self.index.wrapping_sub(1) & 0x1f;

        self.buffer[self.index as usize] = sample;
    }
}

/// CD Audio resampling FIR filter: 7phases, 25taps.
///
/// Much nicer than the one in the SPU but that's sort of expected, the SPU has 24 of them, needs
/// more phases due to the much more flexible frequency and I suppose that CD audio might be more
/// quality-sensitive since it's used to play background music.
///
/// XXX This table is from mednafen, No$ has a different one. Not sure who's right. Note that our
/// table is backwards compared to mednafen because we sum from most recent to most ancient while
/// they do it the other way around.
static FIR_COEFFS: [[i16; 25]; 7] = [
    [
        0, 2, -8, 16, -35, 43, 26, -235, 635, -1352, 2810, -5882, 21472, 15367, -4681, 2062, -839,
        347, -68, -23, 70, -35, 17, -5, 0,
    ],
    [
        -1, 3, -8, 17, -16, 10, 107, -365, 848, -1571, 3021, -6016, 26516, 9036, -2680, 1024, -266,
        9, 52, -84, 65, -34, 10, -2, 0,
    ],
    [
        -1, 3, -8, 6, 5, -27, 166, -424, 882, -1471, 2488, -4532, 29883, 3229, -615, -67, 306,
        -227, 162, -75, 60, -19, 3, 0, -2,
    ],
    [
        -1, 3, -2, -5, 31, -74, 179, -402, 689, -926, 1272, -1446, 31033, -1446, 1272, -926, 689,
        -402, 179, -74, 31, -5, -2, 3, -1,
    ],
    [
        -2, 0, 3, -19, 60, -75, 162, -227, 306, -67, -615, 3229, 29883, -4532, 2488, -1471, 882,
        -424, 166, -27, 5, 6, -8, 3, -1,
    ],
    [
        0, -2, 10, -34, 65, -84, 52, 9, -266, 1024, -2680, 9036, 26516, -6016, 3021, -1571, 848,
        -365, 107, 10, -16, 17, -8, 3, -1,
    ],
    [
        0, -5, 17, -35, 70, -23, -68, 347, -839, 2062, -4681, 15367, 21472, -5882, 2810, -1352,
        635, -235, 26, 43, -35, 16, -8, 2, 0,
    ],
];
