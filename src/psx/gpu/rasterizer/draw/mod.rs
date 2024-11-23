mod fixed_point;

#[cfg(test)]
mod tests;

use std::sync::mpsc;

use super::{Command, CommandBuffer, Frame, RasterizerOption};
use crate::psx::gpu::commands::{vram_access_dimensions, Shaded};
use crate::psx::gpu::commands::{NoShading, Position, Transparent};
use crate::psx::gpu::commands::{NoTexture, Opaque, ShadingMode, TextureBlending, TextureRaw};
use crate::psx::gpu::commands::{TextureMode, TransparencyMode};
use crate::VRamDisplayMode;

use crate::psx::gpu::{DisplayMode, DrawMode, MaskSettings, TextureWindow, TransparencyFunction};
use fixed_point::{FpCoord, FpVar};
use serde::de::{self, Deserialize, Deserializer, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeTuple, Serializer};
use std::cmp::{max, min};
use std::fmt;
use std::marker::PhantomData;

#[derive(serde::Serialize, serde::Deserialize, Debug)]
enum State {
    /// We're waiting for the next command
    WaitingForCommand,
    /// We're uploading data to the VRAM.
    VRamStore(VRamStore),
    /// We're in the middle of a polyline. The u8 is the opcode for this line, then we store the
    /// position of the end of the last drawn segment (i.e. the start of the next segment)
    PolyLine(u8, Vertex),
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Rasterizer {
    vram: VRam,
    state: State,
    /// Frame currently being drawn
    #[serde(skip)]
    cur_frame: Frame,
    /// Left edge of the clipping area
    clip_x_min: i32,
    /// Top edge of the clipping area
    clip_y_min: i32,
    /// Right edge of the clipping area
    clip_x_max: i32,
    /// Bottom edge of the clipping area
    clip_y_max: i32,
    /// Horizontal drawing offset
    draw_offset_x: i32,
    /// Vertical drawing offset
    draw_offset_y: i32,
    /// Mask bit settings
    mask_settings: MaskSettings,
    /// Texture mapping + caching
    tex_mapper: TextureMapper,
    /// If true we output the entire contents of the VRAM instead of just the visible portion
    #[serde(skip)]
    vram_display_mode: VRamDisplayMode,
    /// Number of the first line displayed on the screen
    display_line_start: u16,
    /// Number of the first line *not* displayed on the screen
    display_line_end: u16,
    /// Number of the first column displayed on the screen
    display_column_start: u16,
    /// Number of the first column *not* displayed on the screen
    display_column_end: u16,
    /// Current value of the display mode
    display_mode: DisplayMode,
    /// First column of the display area in VRAM
    display_vram_x_start: u16,
    /// First line of the display area in VRAM
    display_vram_y_start: u16,
    /// True if the display is disabled,
    display_off: bool,
    /// True to draw opaque pixel as semi-transparent
    force_transparency: bool,
    /// Dithering tables, used for dithering, 8-to-5bit color component truncation and saturation.
    ///
    /// Here's the explanation layer by layer:
    ///
    /// * `[_; 4]`: x % 4 to select the right position in the dithering pattern based on the vram
    ///   position.
    /// * `[_; 4]`: y % 4 to select the right position in the dithering pattern based on the vram
    ///   position.
    /// * `[_; 0x200]`: input value, from 0x000 to 0x1ff. Values above 0xff are saturated to 0xff
    #[serde(with = "serialize_dither_table")]
    dither_table: [[[u8; 0x200]; 4]; 4],
    /// True if dithering is currently enabled
    dither_enabled: bool,
    /// If true we force disable dithering, regardless of the draw mode. Should probably only be
    /// used when `draw_24bpp` is also true otherwise you'll get a lot of banding on shaded areas.
    dithering_force_disable: bool,
    /// If true we don't truncate the values drawn to the framebuffer to 15bit RGB555 like the real
    /// hardware but instead keep the full 24bit color depth. If this is true
    /// `dithering_force_disable` should probably also be true since it doesn't make a lot of sense
    /// to dither from 24bits to 24 bits...
    draw_24bpp: bool,
    /// True if we're interlaced and display the bottom field
    display_bottom_field: bool,
    /// Draw the outline of triangles and quads
    draw_wireframe: bool,
    /// If false we don't draw triangles or quads
    draw_polygons: bool,
}

impl Rasterizer {
    pub fn new() -> Rasterizer {
        Rasterizer {
            vram: VRam::with_upscale_shift(0),
            state: State::WaitingForCommand,
            cur_frame: Frame::new(0, 0),
            clip_x_min: 0,
            clip_y_min: 0,
            clip_x_max: 0,
            clip_y_max: 0,
            draw_offset_x: 0,
            draw_offset_y: 0,
            mask_settings: MaskSettings::new(),
            tex_mapper: TextureMapper::new(),
            vram_display_mode: VRamDisplayMode::Native,
            display_line_start: 0x10,
            display_line_end: 0x100,
            display_column_start: 0x200,
            display_column_end: 0xc00,
            display_mode: DisplayMode::new(),
            display_vram_x_start: 0,
            display_vram_y_start: 0,
            display_off: true,
            force_transparency: false,
            dither_table: [[[0; 0x200]; 4]; 4],
            dither_enabled: false,
            dithering_force_disable: false,
            draw_24bpp: false,
            display_bottom_field: false,
            draw_wireframe: false,
            draw_polygons: true,
        }
    }

    /// Attempt to load a rasterizer from the given state. Returns None in case of error.
    pub fn from_serialized(buf: &[u8]) -> Option<Rasterizer> {
        use serde::Deserialize;

        let fbr = match flexbuffers::Reader::get_root(buf) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to load rasterizer state: {}", e);
                return None;
            }
        };

        match Rasterizer::deserialize(fbr) {
            Ok(r) => Some(r),
            Err(e) => {
                error!("Failed to load rasterizer state: {}", e);
                None
            }
        }
    }

    pub fn run(
        &mut self,
        command_channel: mpsc::Receiver<CommandBuffer>,
        frame_channel: mpsc::Sender<Frame>,
        serialization_channel: mpsc::Sender<Vec<u8>>,
    ) {
        self.rebuild_dither_table();
        self.new_frame();

        loop {
            let commands = command_channel.recv().unwrap();

            let mut command_i = commands.iter();

            while let Some(cmd) = command_i.next() {
                match cmd {
                    Command::Gp0(v) => {
                        match self.state {
                            State::WaitingForCommand => {
                                let opcode = v >> 24;
                                let h = &GP0_COMMANDS[opcode as usize];
                                // The longest possible draw command is 12 word long (shaded and
                                // textured quad)
                                let mut params = [0; 12];

                                params[0] = *v;

                                let len = h.len as usize;

                                for i in 1..len {
                                    // The main GPU code is supposed to send us complete draw
                                    // commands so it should be safe to expect the right number of
                                    // parameters here.
                                    match command_i.next() {
                                        Some(Command::Gp0(v)) => params[i] = *v,
                                        other => panic!("Expected GP0 command, got {:?}", other),
                                    }
                                }

                                if opcode != 0xc0 {
                                    (h.handler)(self, &params[..len]);
                                } else {
                                    // VRAM load
                                    cmd_vram_load(self, &params[..len], &frame_channel);
                                }
                            }
                            State::VRamStore(ref mut store) => {
                                let p0 = Pixel::from_mbgr1555(*v as u16);
                                let p1 = Pixel::from_mbgr1555((*v >> 16) as u16);

                                for &p in [p0, p1].iter() {
                                    let (x, y) = store.target_vram_offset();

                                    let target = self.vram.native_pixel(x, y);
                                    if self.mask_settings.can_draw_to(target) {
                                        self.vram.set_native_pixel(
                                            x,
                                            y,
                                            self.mask_settings.mask(p),
                                        );
                                    }

                                    if store.next().is_none() {
                                        // End of store
                                        self.state = State::WaitingForCommand;
                                        break;
                                    }
                                }
                            }
                            State::PolyLine(opcode, _) => {
                                if *v & 0xf000_f000 == 0x5000_5000 {
                                    // End-of-line marker
                                    self.state = State::WaitingForCommand;
                                } else {
                                    // We have a new segment. The GPU code is supposed to send us
                                    // one full vertex at a time so we should have enough in the
                                    // buffer to continue unconditionally
                                    let mut params = [0; 2];
                                    let is_shaded = (opcode & 0x10) != 0;
                                    let len = 1 + (is_shaded as usize);

                                    params[0] = *v;
                                    if is_shaded {
                                        params[1] = match command_i.next() {
                                            Some(Command::Gp0(v)) => *v,
                                            other => {
                                                panic!("Expected GP0 command, got {:?}", other)
                                            }
                                        };
                                    }

                                    let h = &GP0_COMMANDS[opcode as usize];
                                    (h.handler)(self, &params[..len]);
                                }
                            }
                        }
                    }
                    Command::Gp1(v) => self.gp1(*v),
                    Command::Quit => return,
                    // XXX draw one line at a time
                    Command::EndOfLine(l) => self.finish_line(*l),
                    Command::EndOfFrame => {
                        let frame = self.new_frame();
                        frame_channel.send(frame).unwrap();
                    }
                    Command::FieldChanged(f) => self.display_bottom_field = *f,
                    Command::Option(opt) => self.set_option(*opt),
                    Command::Serialize => {
                        use serde::Serialize;

                        // If there are other pending commands they would be lost by the
                        // serialization process
                        assert!(command_i.next().is_none());

                        let mut fb = flexbuffers::FlexbufferSerializer::new();
                        self.serialize(&mut fb).unwrap();

                        serialization_channel.send(fb.take_buffer()).unwrap();
                    }
                }
            }
        }
    }

    /// Returns `false` if the GPU config forbids writing to this line because it's currently
    /// displayed (currently only useful for interlaced output)
    pub fn can_draw_to_line(&self, y: i32) -> bool {
        if self.tex_mapper.draw_mode.draw_to_display_area() {
            // We can draw to display, no worries
            return true;
        }

        if !self.display_mode.is_true_interlaced() {
            // XXX We only implement the test for interlaced output for now, since that's the most
            // common situation where this leads to visual glitches
            return true;
        }

        // XXX This is how mednafen does it so it's probably safe enough but in practice this is
        // probably very wrong: we should probably still be able to draw to these lines if the X is
        // outside of the display. We should also be able to draw to these lines if they're below
        // or above the display area. In practice interlaced is uncommon enough that it's probably
        // good enough.
        let y_is_bottom = ((y + self.display_vram_y_start as i32) & 1) != 0;

        y_is_bottom != self.display_bottom_field
    }

    pub fn set_option(&mut self, opt: RasterizerOption) {
        match opt {
            RasterizerOption::VRamDisplayMode(v) => self.vram_display_mode = v,
            RasterizerOption::ForceTransparency(v) => self.force_transparency = v,
            RasterizerOption::Draw24Bpp(v) => {
                if v != self.draw_24bpp {
                    self.draw_24bpp = v;
                    self.rebuild_dither_table();
                }
            }
            RasterizerOption::DitherForceDisable(v) => {
                self.dithering_force_disable = v;
                self.maybe_rebuild_dither_table();
            }
            RasterizerOption::Wireframe(v) => self.draw_wireframe = v,
            RasterizerOption::DrawPolygons(v) => self.draw_polygons = v,
            RasterizerOption::UpscaleShift(v) => self.set_upscale_shift(v),
        }
    }

    pub fn set_upscale_shift(&mut self, upscale_shift: u8) {
        if self.vram.upscale_shift == upscale_shift {
            return;
        }

        self.clip_x_min >>= self.vram.upscale_shift;
        self.clip_y_min >>= self.vram.upscale_shift;
        self.clip_x_max >>= self.vram.upscale_shift;
        self.clip_x_max >>= self.vram.upscale_shift;

        self.clip_x_min <<= upscale_shift;
        self.clip_y_min <<= upscale_shift;
        self.clip_x_max <<= upscale_shift;
        self.clip_x_max <<= upscale_shift;

        // The clip is inclusive, so we need to offset when upscaling
        self.clip_x_max += (1 << self.vram.upscale_shift) - 1;
        self.clip_y_max += (1 << self.vram.upscale_shift) - 1;

        let mut vram = VRam::with_upscale_shift(upscale_shift);

        for y in 0..512 {
            for x in 0..1024 {
                vram.set_native_pixel(x, y, self.vram.native_pixel(x, y));
            }
        }

        self.vram = vram;
    }

    /// Called when we should output a line to the output buffer
    pub fn finish_line(&mut self, line: u16) {
        if self.vram_display_mode != VRamDisplayMode::Native {
            // We're just going to dump the full VRAM, nothing to do
            return;
        }

        if self.display_off {
            // Output only black pixels
            return;
        }

        if line < self.display_line_start || line >= self.display_line_end {
            // Video is not active
            return;
        }

        let mut frame_y = line - self.display_line_start;
        if self.display_mode.is_true_interlaced() {
            frame_y = (frame_y << 1) | (self.display_bottom_field as u16);
        }

        let vram_y = self.display_vram_y_start + frame_y;

        self.output_line(self.display_vram_x_start, vram_y, frame_y);
    }

    fn output_line(&mut self, x_start: u16, vram_y: u16, frame_y: u16) {
        let x_start = u32::from(x_start) << self.vram.upscale_shift;
        let frame_y = u32::from(frame_y) << self.vram.upscale_shift;
        let vram_y = i32::from(vram_y) << self.vram.upscale_shift;

        if frame_y >= self.cur_frame.height {
            // Out-of-frame. This should only happen if the video mode changed within the current
            // frame, or for the very last line of the bottom field when we're interlaced.
            return;
        }

        let xres = u32::from(self.display_mode.xres()) << self.vram.upscale_shift;

        let width = min(self.cur_frame.width, xres);

        if self.display_mode.output_24bpp() {
            // GPU is in 24bpp mode, we need to do some bitwise magic to recreate the values
            // correctly

            // X position in the framebuffer, in Byte
            let mut fb_x = (x_start * 2) as i32;

            for x in 0..width {
                // We need two consecutive pixels
                let p_x = fb_x >> 1;
                let p1 = self.read_pixel(p_x, vram_y);
                let p2 = self.read_pixel((p_x + 1) & 0x3ff, vram_y);

                let p1 = p1.to_mbgr1555() as u32;
                let p2 = p2.to_mbgr1555() as u32;

                // Reassemble 32bit word
                let mut p = p1 | (p2 << 16);

                // Realign
                p >>= (fb_x & 1) * 8;
                p &= 0xff_ff_ff;

                // Convert from BGR to RGB
                let mut out = p & 0x00_ff_00;
                out |= p >> 16;
                out |= p << 16;

                self.cur_frame.set_pixel(x, frame_y, out);

                fb_x = (fb_x + 3) & 0x7ff;
            }
        } else {
            // GPU outputs pixels "normally", 15bpp native
            for y in 0..(1i32 << self.vram.upscale_shift) {
                for x in 0..width {
                    let p = self.read_pixel((x_start + x) as i32, vram_y + y);
                    self.cur_frame
                        .set_pixel(x, frame_y + (y as u32), p.to_rgb888());
                }
            }
        }
    }

    fn gp1(&mut self, val: u32) {
        let op = val >> 24;

        match op {
            0x00 => self.reset(),
            // Reset command FIFO
            0x01 => (),
            // IRQ1 ack
            0x02 => (),
            0x03 => self.display_off = (val & 1) != 0,
            // DMA direction
            0x04 => (),
            0x05 => {
                // XXX from mednafen: LSB ignored.
                self.display_vram_x_start = (val & 0x3fe) as u16;
                self.display_vram_y_start = ((val >> 10) & 0x1ff) as u16;
            }
            0x06 => {
                self.display_column_start = (val & 0xfff) as u16;
                self.display_column_end = ((val >> 12) & 0xfff) as u16;
            }
            0x07 => {
                self.display_line_start = (val & 0x3ff) as u16;
                self.display_line_end = ((val >> 10) & 0x3ff) as u16;
            }
            0x08 => self.display_mode.set(val & 0xff_ffff),
            // Get info
            0x10 => (),
            _ => warn!("Unimplemented GP1 {:x}", val),
        }
    }

    /// Creates a new, blank frame and returns the previous one
    fn new_frame(&mut self) -> Frame {
        let interlaced = self.display_mode.is_true_interlaced();

        let (width, height) = match self.vram_display_mode {
            VRamDisplayMode::Native => {
                // XXX For now we approximate the dimensions of the visible area of the image.
                // For better accuracy we should be emulating the output video timings more accurately
                // but it's probably not worth it for now.

                let width = self.display_mode.xres();

                let mut height = self.display_line_end - self.display_line_start;
                if interlaced {
                    height *= 2;
                    // Last line of the bottom field isn't drawn
                    height -= 1;
                }

                let w = u32::from(width) << self.vram.upscale_shift;
                let h = u32::from(height) << self.vram.upscale_shift;

                (w, h)
            }
            mode => {
                let (w, h) = mode.max_resolution();
                // If we display the full VRAM we don't output one line at a time in the GPU timing
                // emulation code, we can just copy it fully here. It's not like there's any meaningful
                // concept of "accuracy" in this mode anyway.

                // It's possible for this to fail if `vram_display_mode` was just switched and the
                // current frame doesn't have the proper dimensions. In this case we're going to return
                // a partially drawn leftover frame which is probably not too important.
                if self.cur_frame.width >= u32::from(w) && self.cur_frame.height >= u32::from(h) {
                    for y in 0..h {
                        for x in 0..w {
                            let px_off = usize::from(y) * usize::from(w) + usize::from(x);
                            self.cur_frame.pixels[px_off] = match mode {
                                VRamDisplayMode::Native => unreachable!(),
                                VRamDisplayMode::Full16bpp => {
                                    self.vram.native_pixel(x, y).to_rgb888()
                                }
                                VRamDisplayMode::Full8bpp => {
                                    let p = self.vram.native_pixel(x >> 1, y).vram_bytes();

                                    let g = p[usize::from(x & 1)];

                                    Pixel::from_rgb(g, g, g).0
                                }
                                VRamDisplayMode::Full4bpp => {
                                    let p = self.vram.native_pixel(x >> 2, y).to_mbgr1555();
                                    let n = [
                                        (p & 0xf) as u8,
                                        ((p >> 4) & 0xf) as u8,
                                        ((p >> 8) & 0xf) as u8,
                                        ((p >> 12) & 0xf) as u8,
                                    ];

                                    let g = n[usize::from(x & 3)];
                                    let g = g | g << 4;

                                    Pixel::from_rgb(g, g, g).0
                                }
                            };
                        }
                    }
                }

                (u32::from(w), u32::from(h))
            }
        };

        if width == self.cur_frame.width && height == self.cur_frame.height {
            self.cur_frame.clone()
        } else {
            // Resolution changed, create a whole new frame
            let mut new_frame = Frame::new(width, height);

            ::std::mem::swap(&mut new_frame, &mut self.cur_frame);

            new_frame
        }
    }

    /// Create a new frame with the given `width` and `height` and containing the pixels in the VRAM
    /// region locatied at `left`x`top`. Used to implement VRAM reads
    fn copy_vram_rect(&mut self, left: u16, top: u16, width: u16, height: u16) -> Frame {
        let mut frame = Frame::new(u32::from(width), u32::from(height));

        for y in 0..height {
            let vram_y = (top + y) & 0x1ff;
            for x in 0..width {
                let vram_x = (left + x) & 0x3ff;

                let p = self.vram.native_pixel(vram_x, vram_y);
                // XXX I reuse Frame here for simplicity but since VRAM loads are 16 bits we waste
                // half the storage here.
                frame.set_pixel(u32::from(x), u32::from(y), u32::from(p.to_mbgr1555()));
            }
        }

        frame
    }

    /// Rebuild `dither_tables` based on the various dithering and color depth settings
    fn rebuild_dither_table(&mut self) {
        // When dithering is enabled DITHER_OFFSETS[x % 4][y % 4] is added to the 8bit value before
        // truncation to 5 bits
        const DITHER_OFFSETS: [[i16; 4]; 4] = [
            [-4, 0, -3, 1],
            [2, -2, 3, -1],
            [-3, 1, -4, 0],
            [3, -1, 2, -2],
        ];

        self.dither_enabled = self.dither_enable();

        for x in 0..4 {
            for y in 0..4 {
                for input_value in 0..0x200 {
                    let mut out = input_value as i16;

                    if self.dither_enabled {
                        out += DITHER_OFFSETS[x][y];
                    }

                    // Saturate to 8bits
                    let mut out = if out < 0 {
                        0
                    } else if out > 0xff {
                        0xff
                    } else {
                        out as u8
                    };

                    if !self.draw_24bpp {
                        // 8-to-5 bit truncation. Since we always output 24 bits per pixel we just
                        // replace the LSBs with the MSBs (this ways blacks remain black and whites
                        // remain white and we effectively lose 3 significant bits)
                        out &= 0xf8;
                        out |= out >> 5;
                    }

                    self.dither_table[x][y][input_value] = out;
                }
            }
        }
    }

    /// Rebuild dithering tables if we need to activate/deactivate the dithering
    fn maybe_rebuild_dither_table(&mut self) {
        if self.dither_enabled != self.dither_enable() {
            self.rebuild_dither_table();
        }
    }

    /// Returns true if dithering should be active
    fn dither_enable(&self) -> bool {
        self.tex_mapper.draw_mode.dither_enable() && !self.dithering_force_disable
    }

    fn reset(&mut self) {
        self.clip_x_min = 0;
        self.clip_y_min = 0;
        self.clip_x_max = 0;
        self.clip_y_max = 0;
        self.draw_offset_x = 0;
        self.draw_offset_y = 0;
        self.draw_offset_x = 0;
        self.draw_offset_y = 0;
        self.tex_mapper.reset();
        self.mask_settings.set(0);
        self.display_line_start = 0x10;
        self.display_line_end = 0x100;
        self.display_column_start = 0x200;
        self.display_column_end = 0xc00;
        self.display_mode.set(0);
        self.display_vram_x_start = 0;
        self.display_vram_y_start = 0;

        self.maybe_rebuild_dither_table();
    }

    fn read_pixel(&self, x: i32, y: i32) -> Pixel {
        debug_assert!(
            (0..(1024 << self.vram.upscale_shift)).contains(&x),
            "x out of bounds ({})",
            x
        );
        debug_assert!(
            (0..(1024 << self.vram.upscale_shift)).contains(&y),
            "y out of bounds ({})",
            y
        );

        let y = (y & ((0x200 << self.vram.upscale_shift) - 1)) as u32;
        let x = x as u32;

        self.vram.pixel(x, y)
    }

    fn draw_pixel<Transparency, Texture>(&mut self, x: i32, y: i32, mut color: Pixel)
    where
        Transparency: TransparencyMode,
        Texture: TextureMode,
    {
        debug_assert!(
            (0..(1024 << self.vram.upscale_shift)).contains(&x),
            "x out of bounds ({})",
            x
        );
        debug_assert!(
            (0..(1024 << self.vram.upscale_shift)).contains(&y),
            "y out of bounds ({})",
            y
        );

        // Apparently the PlayStation GPU supports 2MB VRAM (1024x1024, used in some arcade
        // machines apparently) but the bottom half isn't installed so it wraps around.
        let y = (y & ((0x200 << self.vram.upscale_shift) - 1)) as u32;
        let x = x as u32;

        let bg_pixel = self.vram.pixel(x, y);

        if !self.mask_settings.can_draw_to(bg_pixel) {
            // Masked
            return;
        }

        // If the draw command is semi-transparent and the texture mask bit is set, this is a
        // transparent pixel. If the draw command is not textured all pixels are transparent.
        let is_transparent =
            Transparency::is_transparent() && (!Texture::is_textured() || color.mask());

        if is_transparent {
            let mode = self.tex_mapper.draw_mode.transparency_mode();

            // XXX if we wanted to be extra-accurate we might want to truncate the color here to
            // get accurate result in 15bpp. It's unlikely to make a significant difference
            // however.
            color.apply_transparency(bg_pixel, mode);

            if Texture::is_textured() {
                // XXX Not entirely sure about this.
                color.set_mask();
            }
        } else if self.force_transparency {
            color.apply_transparency(bg_pixel, TransparencyFunction::Average);
        }

        color = self.mask_settings.mask(color);

        self.vram.set_pixel(x, y, color);
    }

    fn draw_triangle<Transparency, Texture, Shading>(&mut self, mut vertices: [Vertex; 3])
    where
        Transparency: TransparencyMode,
        Texture: TextureMode,
        Shading: ShadingMode,
    {
        // Order the vertices by y
        vertices.sort_by(|a, b| a.position.y.cmp(&b.position.y));

        let a = &vertices[0];
        let b = &vertices[1];
        let c = &vertices[2];

        // We need to draw split the triangle in two sub-triangles. Consider the following
        // triangle:
        //
        //    A
        //    +
        //    |\
        //    | \
        //    |  \
        //  H +   + B <-- Need to cut horizontally here.
        //    |  /
        //    | /
        //    |/
        //    +
        //    C
        //
        // Note that since we order A, B and C by Y coordinate it's possible for B to be on either
        // side of the triangle (see `ac_is_left` below)
        //
        // In order to draw it simply we need to break it into two sub-triangles by splitting the
        // full triangle with an horizontal line at B.
        //
        // To make matters more complicated the sub-triangle draw order (and whether they're draw
        // top-to-bottom or bottom-to-top) depends on the coordinates of the vertices and the order
        // in which they're received by the GPU (see how core_vertex is determined below).
        //
        // Of course in some situations we'll end up with "flat" triangles, where one edge is
        // perfectly horizontal and A.x == B.x or C.x == B.x, in which case one of these
        // sub-triangles will effectively have 0 height.

        let y_min = a.position.y;
        let y_max = c.position.y;

        if y_max - y_min >= (512 << self.vram.upscale_shift) {
            // Triangle is too tall, give up
            return;
        }

        if y_max < self.clip_y_min || y_min > self.clip_y_max {
            // The triangle is fully above or below the clip area, we don't have anything to draw
            return;
        }

        // Find the left side of the bounding box and the index of the core vertex. The core vertex
        // is the one we'll start drawing from.
        let core_vertex = vertices
            .iter()
            .min_by(|v0, v1| {
                // Here's the trick: the "core" vertex is the leftmost one. If two vertices are
                // lined up vertically on the left they'll both be equally leftmost, in this case
                // we take the one that comes *last* in the command.
                v0.position
                    .x
                    .cmp(&v1.position.x)
                    .then_with(|| v1.index.cmp(&v0.index))
            })
            .unwrap();

        let x_min = core_vertex.position.x;

        let x_max = vertices.iter().map(|v| v.position.x).max().unwrap();

        if x_max - x_min >= (1024 << self.vram.upscale_shift) {
            // Triangle is too large, give up
            return;
        }

        if x_max < self.clip_x_min || x_min > self.clip_x_max {
            // The triangle is fully to the left or right of the draw area, we don't have anything
            // to draw
            return;
        }

        let xproduct = cross_product(a.position, b.position, c.position);

        if xproduct == 0 {
            // All three vertices are aligned, the triangle is perfectly flat and we have nothing
            // to draw
            return;
        }

        let deltas = RasterVarDeltas::new::<Texture, Shading>(xproduct, &vertices);
        // Initialize the variables with the core vertex values, then move to 0, 0. This way we'll
        // then be able to interpolate the value of the variables for any absolute coordinates
        let mut vars = RasterVars::new::<Texture>(core_vertex);
        vars.translate_by::<Texture, Shading>(&deltas, -core_vertex.x(), -core_vertex.y());

        // True if AC is the left edge and AB + BC are the right edges, false if it's the other way
        // around
        let ac_is_left = xproduct > 0;

        let a_x = a.position.x;
        let b_x = b.position.x;
        let c_x = c.position.x;

        let a_y = a.position.y;
        let b_y = b.position.y;
        let c_y = c.position.y;

        // Slope of AC. We've already checked that the triangle had non-0 screen height, so we know
        // that this can't be a division by 0
        let ac_dxdy = FpCoord::new_dxdy(c_x - a_x, c_y - a_y);

        // Slope of AB
        let ab_dxdy = if a_y != b_y {
            FpCoord::new_dxdy(b_x - a_x, b_y - a_y)
        } else {
            // AB is horizontal, we won't have to use this variable
            FpCoord::new(0)
        };

        // Slope of BC
        let bc_dxdy = if b_y != c_y {
            FpCoord::new_dxdy(c_x - b_x, c_y - b_y)
        } else {
            // BC is horizontal, we won't have to use this variable
            FpCoord::new(0)
        };

        let a_fpx = FpCoord::new_saturated(a_x);
        let b_fpx = FpCoord::new_saturated(b_x);
        let c_fpx = FpCoord::new_saturated(c_x);
        // Coordinate of the point on AC that has the same y as B
        let h_fpx = a_fpx + ac_dxdy * (b_y - a_y);

        // The draw order depends on the core_vertex.
        if core_vertex.index == a.index {
            // We draw AB then BC

            if a_y != b_y {
                // Draw AB
                let (left_dxdy, right_dxdy) = if ac_is_left {
                    (ac_dxdy, ab_dxdy)
                } else {
                    (ab_dxdy, ac_dxdy)
                };

                let rc = RasterCoords {
                    start_y: a_y,
                    end_y: b_y,
                    left_x: a_fpx,
                    right_x: a_fpx,
                    left_dxdy,
                    right_dxdy,
                };

                self.rasterize::<Transparency, Texture, Shading>(
                    rc,
                    &vars,
                    &deltas,
                    RasterDir::Down,
                );
            }

            if b_y != c_y {
                // Draw BC
                let (left_x, left_dxdy, right_x, right_dxdy) = if ac_is_left {
                    (h_fpx, ac_dxdy, b_fpx, bc_dxdy)
                } else {
                    (b_fpx, bc_dxdy, h_fpx, ac_dxdy)
                };

                let rc = RasterCoords {
                    start_y: b_y,
                    end_y: c_y,
                    left_x,
                    right_x,
                    left_dxdy,
                    right_dxdy,
                };

                self.rasterize::<Transparency, Texture, Shading>(
                    rc,
                    &vars,
                    &deltas,
                    RasterDir::Down,
                );
            }
        } else {
            // Core vertex is B or C

            if b_y != c_y {
                if core_vertex.index == b.index {
                    // Draw BC
                    let (left_x, left_dxdy, right_x, right_dxdy) = if ac_is_left {
                        (h_fpx, ac_dxdy, b_fpx, bc_dxdy)
                    } else {
                        (b_fpx, bc_dxdy, h_fpx, ac_dxdy)
                    };

                    let rc = RasterCoords {
                        start_y: b_y,
                        end_y: c_y,
                        left_x,
                        right_x,
                        left_dxdy,
                        right_dxdy,
                    };

                    self.rasterize::<Transparency, Texture, Shading>(
                        rc,
                        &vars,
                        &deltas,
                        RasterDir::Down,
                    );
                } else {
                    // Core vertex is C. Draw CB.
                    let (left_dxdy, right_dxdy) = if ac_is_left {
                        (ac_dxdy, bc_dxdy)
                    } else {
                        (bc_dxdy, ac_dxdy)
                    };

                    let rc = RasterCoords {
                        start_y: c_y,
                        end_y: b_y,
                        left_x: c_fpx,
                        right_x: c_fpx,
                        left_dxdy,
                        right_dxdy,
                    };

                    self.rasterize::<Transparency, Texture, Shading>(
                        rc,
                        &vars,
                        &deltas,
                        RasterDir::Up,
                    );
                }
            }

            // If the core vertex is B or C we always end up by drawing BA
            if a_y != b_y {
                let (left_x, left_dxdy, right_x, right_dxdy) = if ac_is_left {
                    (h_fpx, ac_dxdy, b_fpx, ab_dxdy)
                } else {
                    (b_fpx, ab_dxdy, h_fpx, ac_dxdy)
                };

                let rc = RasterCoords {
                    start_y: b_y,
                    end_y: a_y,
                    left_x,
                    right_x,
                    left_dxdy,
                    right_dxdy,
                };

                self.rasterize::<Transparency, Texture, Shading>(rc, &vars, &deltas, RasterDir::Up);
            }
        }
    }

    fn rasterize<Transparency, Texture, Shading>(
        &mut self,
        rc: RasterCoords,
        vars: &RasterVars,
        deltas: &RasterVarDeltas,
        dir: RasterDir,
    ) where
        Transparency: TransparencyMode,
        Texture: TextureMode,
        Shading: ShadingMode,
    {
        let mut y = rc.start_y;
        let mut left_x = rc.left_x;
        let mut right_x = rc.right_x;

        if dir == RasterDir::Up {
            while y != rc.end_y {
                // We move first, then we draw.
                y -= 1;
                left_x -= rc.left_dxdy;
                right_x -= rc.right_dxdy;

                if y < self.clip_y_min {
                    // We left the drawing area
                    break;
                }

                if y <= self.clip_y_max {
                    self.rasterize_scanline::<Transparency, Texture, Shading>(
                        y,
                        left_x.truncate(),
                        right_x.truncate(),
                        vars.clone(),
                        deltas,
                    );
                }
            }
        } else {
            while y != rc.end_y {
                if y > self.clip_y_max {
                    // We left the drawing area
                    break;
                }

                if y >= self.clip_y_min {
                    self.rasterize_scanline::<Transparency, Texture, Shading>(
                        y,
                        left_x.truncate(),
                        right_x.truncate(),
                        vars.clone(),
                        deltas,
                    );
                }

                y += 1;
                left_x += rc.left_dxdy;
                right_x += rc.right_dxdy;
            }
        }
    }

    /// Rasterize one line from a triangle
    fn rasterize_scanline<Transparency, Texture, Shading>(
        &mut self,
        y: i32,
        left_x: i32,
        right_x: i32,
        mut vars: RasterVars,
        deltas: &RasterVarDeltas,
    ) where
        Transparency: TransparencyMode,
        Texture: TextureMode,
        Shading: ShadingMode,
    {
        let start_x = max(left_x, self.clip_x_min);
        let end_x = min(right_x, self.clip_x_max + 1);

        if !self.can_draw_to_line(y) {
            return;
        }

        if start_x >= end_x {
            // Line is either 0-length or clipped
            return;
        }

        // We "move" the variables to the start of the line
        vars.translate_by::<Texture, Shading>(deltas, start_x, y);

        for x in start_x..end_x {
            if Texture::is_textured() {
                let texel = self.get_texel(vars.u(), vars.v());
                // If the pixel is equal to 0 (including mask bit) then we don't draw it
                if !texel.is_nul() {
                    if Texture::is_raw_texture() {
                        // No need to worry about truncation here since textures are always 555
                        // anyway
                        self.draw_pixel::<Transparency, Texture>(x, y, texel);
                    } else {
                        // Texture blending: the final color is a combination of the texel and
                        // the computed gouraud color
                        let blend = self.blend_and_dither(x, y, texel, vars.color());
                        self.draw_pixel::<Transparency, Texture>(x, y, blend);
                    }
                }
            } else {
                // No texture
                let (mut r, mut g, mut b) = vars.color_components();

                if Shading::is_shaded() {
                    r = self.dither(x, y, r as u32);
                    g = self.dither(x, y, g as u32);
                    b = self.dither(x, y, b as u32);
                }

                let color = Pixel::from_rgb(r, g, b);

                self.draw_pixel::<Transparency, Texture>(x, y, color);
            }
            vars.translate_right::<Texture, Shading>(deltas);
        }
    }

    fn draw_rect<Transparency, Texture>(&mut self, origin: Vertex, width: i32, height: i32)
    where
        Transparency: TransparencyMode,
        Texture: TextureMode,
    {
        let mut u_start = origin.u;
        let mut v = origin.v;

        let (u_inc, v_inc) = if Texture::is_textured() {
            // Per-No$ these bits aren't supposed to function in early PSX models. If that's true
            // they probably aren't used in many games.
            let flip_x = self.tex_mapper.draw_mode.flip_rect_x();
            let flip_y = self.tex_mapper.draw_mode.flip_rect_y();

            let u_inc = if flip_x {
                // XXX Taken from Mednafen, not sure what this does
                u_start |= 1;
                -1
            } else {
                1
            };

            let v_inc = if flip_y { -1 } else { 1 };

            (u_inc, v_inc)
        } else {
            (0, 0)
        };

        /* We always draw rects at native res */
        let clip_x_min = self.clip_x_min >> self.vram.upscale_shift;
        let clip_y_min = self.clip_y_min >> self.vram.upscale_shift;
        let clip_x_max = self.clip_x_max >> self.vram.upscale_shift;
        let clip_y_max = self.clip_y_max >> self.vram.upscale_shift;

        let mut x_start = origin.x();
        let x_end = min(x_start + width, clip_x_max + 1);

        let mut y_start = origin.y();
        let y_end = min(y_start + height, clip_y_max + 1);

        if x_start < clip_x_min {
            if Texture::is_textured() {
                let skip = (clip_x_min - x_start) * u_inc;

                u_start = u_start.wrapping_add(skip as u8);
            }
            x_start = clip_x_min;
        }

        if y_start < clip_y_min {
            if Texture::is_textured() {
                let skip = (clip_y_min - y_start) * u_inc;

                v = v.wrapping_add(skip as u8);
            }
            y_start = clip_y_min;
        }

        if x_end <= x_start || y_end <= y_start {
            // Rect is 0-width or completely clipped
            return;
        }

        let mut color = origin.color;

        if !Texture::is_textured() {
            // We're only going to copy this color everywhere, let's truncate it here once and for
            // all
            color = self.truncate_color(color);
        }

        for y in y_start..y_end {
            if !self.can_draw_to_line(y) {
                v = v.wrapping_add(v_inc as u8);
                continue;
            }

            let mut u = u_start;
            for x in x_start..x_end {
                if Texture::is_textured() {
                    let texel = self.get_texel(u, v);
                    // If the pixel is equal to 0 (including mask bit) then we don't draw it
                    if !texel.is_nul() {
                        for y in
                            (y << self.vram.upscale_shift)..((y + 1) << self.vram.upscale_shift)
                        {
                            for x in
                                (x << self.vram.upscale_shift)..((x + 1) << self.vram.upscale_shift)
                            {
                                if Texture::is_raw_texture() {
                                    self.draw_pixel::<Transparency, Texture>(x, y, texel);
                                } else {
                                    // Texture blending: the final color is a combination of the texel and
                                    // the solid color. Rect are never dithered.
                                    let blend = self.blend(texel, origin.color);
                                    self.draw_pixel::<Transparency, Texture>(x, y, blend);
                                }
                            }
                        }
                    }
                } else {
                    // No texture
                    for y in (y << self.vram.upscale_shift)..((y + 1) << self.vram.upscale_shift) {
                        for x in
                            (x << self.vram.upscale_shift)..((x + 1) << self.vram.upscale_shift)
                        {
                            self.draw_pixel::<Transparency, Texture>(x, y, color);
                        }
                    }
                }
                u = u.wrapping_add(u_inc as u8);
            }

            v = v.wrapping_add(v_inc as u8);
        }
    }

    fn draw_line<Transparency, Shading>(&mut self, mut start: Vertex, mut end: Vertex)
    where
        Transparency: TransparencyMode,
        Shading: ShadingMode,
    {
        // Start at the leftmost edge.
        // XXX Apparently if both sides have the same X we start from the end? This is what
        // mednafen does.
        if start.x() >= end.x() {
            ::std::mem::swap(&mut start, &mut end);
        }

        let start_x = start.x();
        let start_y = start.y();
        let end_x = end.x();
        let end_y = end.y();

        let dx = (start_x - end_x).abs();
        let dy = (start_y - end_y).abs();

        let long_edge = max(dx, dy);

        if long_edge == 0 {
            // 0-length line, nothing to do
            return;
        }

        if dx >= (1024 << self.vram.upscale_shift) || dy >= (512 << self.vram.upscale_shift) {
            // Line is too long, ignore
            return;
        }

        let min_x = min(start_x, end_x);
        let max_x = max(start_x, end_x);
        let min_y = min(start_y, end_y);
        let max_y = max(start_y, end_y);

        let clipped = min_y > self.clip_y_max
            || max_y < self.clip_y_min
            || min_x > self.clip_x_max
            || max_x < self.clip_x_min;

        if clipped {
            // The line is completely outside of the clipping area
            return;
        }

        // We're going to follow the long edge one pixel at a time. That means that one of the
        // values below will necessarily be +1 or -1.
        let dx_dt = FpCoord::new_dxdy(end_x - start_x, long_edge);
        let dy_dt = FpCoord::new_dxdy(end_y - start_y, long_edge);

        let dr_dt;
        let dg_dt;
        let db_dt;

        if Shading::is_shaded() {
            dr_dt = FpVar::new(end.red() - start.red()) / long_edge;
            dg_dt = FpVar::new(end.green() - start.green()) / long_edge;
            db_dt = FpVar::new(end.blue() - start.blue()) / long_edge;
        } else {
            dr_dt = FpVar::new(0);
            dg_dt = FpVar::new(0);
            db_dt = FpVar::new(0);
        }

        let mut lx = FpCoord::new_line_x(start_x);
        let mut ly = FpCoord::new_line_y(start_y, end_y < start_y);

        let mut red = FpVar::new_center(start.red());
        let mut green = FpVar::new_center(start.green());
        let mut blue = FpVar::new_center(start.blue());

        for _t in 0..=long_edge {
            let x = lx.truncate() & 0x7ff;
            let y = ly.truncate() & 0x7ff;
            let r = red.truncate();
            let g = green.truncate();
            let b = blue.truncate();

            lx += dx_dt;
            ly += dy_dt;

            if Shading::is_shaded() {
                red += dr_dt;
                green += dg_dt;
                blue += db_dt;
            }

            if !self.can_draw_to_line(y) {
                continue;
            }

            let clipped = y > self.clip_y_max
                || y < self.clip_y_min
                || x > self.clip_x_max
                || x < self.clip_x_min;

            if clipped {
                continue;
            }

            // Lines are *always* dithered, even when not shaded (unlike triangles)
            let r = self.dither(x, y, r as u32);
            let g = self.dither(x, y, g as u32);
            let b = self.dither(x, y, b as u32);

            let color = Pixel::from_rgb(r, g, b);

            self.draw_pixel::<Transparency, NoTexture>(x, y, color);
        }
    }

    fn set_clut(&mut self, clut: u32) {
        self.tex_mapper.set_clut(clut, &self.vram);
    }

    fn get_texel(&mut self, u: u8, v: u8) -> Pixel {
        self.tex_mapper.get_texel(u, v, &self.vram)
    }

    fn blend(&self, texel: Pixel, color: Pixel) -> Pixel {
        // If you look at DITHER_OFFSETS when we build the table you can see that
        // DITHER_OFFSETS[0][1] is equal to 0, therefore even if dithering is enabled this won't
        // actually modify the value of the pixel beyond normal saturation and truncation.
        self.blend_and_dither(0, 1, texel, color)
    }

    /// Perform texture blending and dithering
    fn blend_and_dither(&self, x: i32, y: i32, texel: Pixel, color: Pixel) -> Pixel {
        let t_r = texel.red() as u32;
        let t_g = texel.green() as u32;
        let t_b = texel.blue() as u32;

        let c_r = color.red() as u32;
        let c_g = color.green() as u32;
        let c_b = color.blue() as u32;

        // In order to normalize the value we should be shifting by 8, but texture blending
        // actually doubles the value, hence the - 1.
        let mut r = (t_r * c_r) >> (8 - 1);
        let mut g = (t_g * c_g) >> (8 - 1);
        let mut b = (t_b * c_b) >> (8 - 1);

        // Perform dithering, saturation and 8-to-5 conversion (if enabled)
        r = self.dither(x, y, r) as u32;
        g = self.dither(x, y, g) as u32;
        b = self.dither(x, y, b) as u32;

        let mask = texel.0 & 0xff00_0000;

        Pixel(mask | b | (g << 8) | (r << 16))
    }

    fn dither(&self, x: i32, y: i32, input: u32) -> u8 {
        let x = (x & 3) as usize;
        let y = (y & 3) as usize;
        let input = input as usize;

        self.dither_table[x][y][input]
    }

    /// Apply 8-to-5bit truncation if enabled
    fn truncate_color(&self, color: Pixel) -> Pixel {
        let r = color.red();
        let g = color.green();
        let b = color.blue();
        let mask = color.0 & 0xff00_0000;

        let r = self.truncate_component(r) as u32;
        let g = self.truncate_component(g) as u32;
        let b = self.truncate_component(b) as u32;

        Pixel(mask | b | (g << 8) | (r << 16))
    }

    fn truncate_component(&self, c: u8) -> u8 {
        // If you look at DITHER_OFFSETS when we build the table you can see that
        // DITHER_OFFSETS[0][1] is equal to 0, therefore even if dithering is disabled this won't
        // actually modify the value of the pixel beyond normal saturation and truncation.
        //
        // If draw_24bpp is true this is a nop since the entry in the table will be the same value
        // as the index in the table
        self.dither_table[0][1][c as usize]
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum RasterDir {
    /// We drawing lines from top to bottom
    Down,
    /// We drawing lines from bottom to top
    Up,
}

/// Structure containing the definition of the various coordinates necessary to rasterize a
/// triangle
struct RasterCoords {
    /// Y coordinate of the first line to be drawn
    start_y: i32,
    /// Y coordinate of the first line *not* to be drawn
    end_y: i32,
    /// X coordinate of the left side of the first line to be drawn
    left_x: FpCoord,
    /// X coordinate of the right side of the first line to be drawn
    right_x: FpCoord,
    /// Value added or subtracted to left_x every time we move down or up one line (respectively)
    left_dxdy: FpCoord,
    /// Value added or subtracted to right_x every time we move down or up one line (respectively)
    right_dxdy: FpCoord,
}

/// Structure containing the various delta values for Gouraud shading and texture mapping
struct RasterVarDeltas {
    /// Value added or subtracted to the red component every time we move along the X axis
    drdx: FpVar,
    /// Value added or subtracted to the red component every time we move along the Y axis
    drdy: FpVar,
    /// Value added or subtracted to the green component every time we move along the X axis
    dgdx: FpVar,
    /// Value added or subtracted to the green component every time we move along the Y axis
    dgdy: FpVar,
    /// Value added or subtracted to the blue component every time we move along the X axis
    dbdx: FpVar,
    /// Value added or subtracted to the blue component every time we move along the Y axis
    dbdy: FpVar,

    /// Value added or subtracted to the texture U coordinate every time we move along the X axis
    dudx: FpVar,
    /// Value added or subtracted to the texture U coordinate every time we move along the Y axis
    dudy: FpVar,
    /// Value added or subtracted to the texture V coordinate every time we move along the X axis
    dvdx: FpVar,
    /// Value added or subtracted to the texture V coordinate every time we move along the Y axis
    dvdy: FpVar,
}

impl RasterVarDeltas {
    fn new<Texture, Shading>(xproduct: i32, vertices: &[Vertex; 3]) -> RasterVarDeltas
    where
        Texture: TextureMode,
        Shading: ShadingMode,
    {
        debug_assert!(xproduct != 0);

        let mut d = RasterVarDeltas {
            drdx: FpVar::new(0),
            drdy: FpVar::new(0),
            dgdx: FpVar::new(0),
            dgdy: FpVar::new(0),
            dbdx: FpVar::new(0),
            dbdy: FpVar::new(0),
            dudx: FpVar::new(0),
            dudy: FpVar::new(0),
            dvdx: FpVar::new(0),
            dvdy: FpVar::new(0),
        };

        if Shading::is_shaded() {
            // Compute the gradient deltas for every component along both axes
            d.drdx = Self::compute_delta(xproduct, vertices, |v| v.red(), |v| v.y());
            d.drdy = Self::compute_delta(xproduct, vertices, |v| v.x(), |v| v.red());
            d.dgdx = Self::compute_delta(xproduct, vertices, |v| v.green(), |v| v.y());
            d.dgdy = Self::compute_delta(xproduct, vertices, |v| v.x(), |v| v.green());
            d.dbdx = Self::compute_delta(xproduct, vertices, |v| v.blue(), |v| v.y());
            d.dbdy = Self::compute_delta(xproduct, vertices, |v| v.x(), |v| v.blue());
        }

        if Texture::is_textured() {
            d.dudx = Self::compute_delta(xproduct, vertices, |v| i32::from(v.u), |v| v.y());
            d.dudy = Self::compute_delta(xproduct, vertices, |v| v.x(), |v| i32::from(v.u));
            d.dvdx = Self::compute_delta(xproduct, vertices, |v| i32::from(v.v), |v| v.y());
            d.dvdy = Self::compute_delta(xproduct, vertices, |v| v.x(), |v| i32::from(v.v));
        }

        d
    }

    fn compute_delta<X, Y>(xproduct: i32, vertices: &[Vertex; 3], get_x: X, get_y: Y) -> FpVar
    where
        X: Fn(&Vertex) -> i32,
        Y: Fn(&Vertex) -> i32,
    {
        let xp = cross_product_with(&vertices[0], &vertices[1], &vertices[2], get_x, get_y);

        FpVar::new(xp) / xproduct
    }
}

/// Variables used during rasterization
#[derive(Debug, Clone)]
struct RasterVars {
    /// Red shading component
    red: FpVar,
    /// Green shading component
    green: FpVar,
    /// Blue shading component
    blue: FpVar,
    /// Texture U coordinate
    u: FpVar,
    /// Texture V coordinate
    v: FpVar,
}

impl RasterVars {
    fn new<Texture>(vertex: &Vertex) -> RasterVars
    where
        Texture: TextureMode,
    {
        let (u, v) = if Texture::is_textured() {
            (
                FpVar::new_center(i32::from(vertex.u)),
                FpVar::new_center(i32::from(vertex.v)),
            )
        } else {
            (FpVar::new(0), FpVar::new(0))
        };

        // We set the red/green/blue even if shading is disabled since they'll be used for solid
        // shading as well
        RasterVars {
            red: FpVar::new_center(vertex.red()),
            green: FpVar::new_center(vertex.green()),
            blue: FpVar::new_center(vertex.blue()),
            u,
            v,
        }
    }

    fn translate_by<Texture, Shading>(&mut self, deltas: &RasterVarDeltas, x_off: i32, y_off: i32)
    where
        Texture: TextureMode,
        Shading: ShadingMode,
    {
        if Texture::is_textured() {
            self.u += deltas.dudx * x_off;
            self.u += deltas.dudy * y_off;
            self.v += deltas.dvdx * x_off;
            self.v += deltas.dvdy * y_off;
        }

        if Shading::is_shaded() {
            self.red += deltas.drdx * x_off;
            self.red += deltas.drdy * y_off;
            self.green += deltas.dgdx * x_off;
            self.green += deltas.dgdy * y_off;
            self.blue += deltas.dbdx * x_off;
            self.blue += deltas.dbdy * y_off;
        }
    }

    /// Move var one pixel to the right
    fn translate_right<Texture, Shading>(&mut self, deltas: &RasterVarDeltas)
    where
        Texture: TextureMode,
        Shading: ShadingMode,
    {
        if Texture::is_textured() {
            self.u += deltas.dudx;
            self.v += deltas.dvdx;
        }

        if Shading::is_shaded() {
            self.red += deltas.drdx;
            self.green += deltas.dgdx;
            self.blue += deltas.dbdx;
        }
    }

    fn color(&self) -> Pixel {
        let (r, g, b) = self.color_components();

        Pixel::from_rgb(r, g, b)
    }

    fn color_components(&self) -> (u8, u8, u8) {
        let r = self.red.truncate() as u8;
        let b = self.blue.truncate() as u8;
        let g = self.green.truncate() as u8;

        (r, g, b)
    }

    fn u(&self) -> u8 {
        self.u.truncate() as u8
    }

    fn v(&self) -> u8 {
        self.v.truncate() as u8
    }
}

/// Compute the cross-product of (AB) x (AC) using the provided getters for x and y
fn cross_product_with<X, Y>(a: &Vertex, b: &Vertex, c: &Vertex, get_x: X, get_y: Y) -> i32
where
    X: Fn(&Vertex) -> i32,
    Y: Fn(&Vertex) -> i32,
{
    let a_x = get_x(a);
    let b_x = get_x(b);
    let c_x = get_x(c);
    let a_y = get_y(a);
    let b_y = get_y(b);
    let c_y = get_y(c);

    (b_x - a_x) * (c_y - a_y) - (c_x - a_x) * (b_y - a_y)
}

/// Compute the cross-product of (AB) x (AC)
fn cross_product(a: Position, b: Position, c: Position) -> i32 {
    (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)
}

/// Structure keeping track of the state needed to convert the extrapolated 8bit U/V values of the
/// rasterizer into absolute coordinates in VRAM. The mapping is non-trivial because the PSX GPU
/// uses 256x256 texture pages, coordinate masking and CLUTs of various depths.
#[derive(serde::Serialize, serde::Deserialize)]
struct TextureMapper {
    /// Draw mode configuration
    draw_mode: DrawMode,
    /// Texture window settings
    tex_window: TextureWindow,
    /// AND mask applied to U coordinates
    u_mask: u8,
    /// AND mask applied to V coordinates
    v_mask: u8,
    /// Value added to U coordinates to find the raw (unpaletted) texel value in VRAM. This value
    /// is a texel offset, not a VRAM pixel offset. Texel size can be 4, 8 or 16bits per pixel,
    /// VRAM pixels on the other hand are always 16bits wide (natively)
    u_offset: u16,
    /// Value added to V coordinates to find the raw (unpaletted) texel value in VRAM
    v_offset: u16,
    /// Shift value to convert a number of texels into a number of VRAM pixels. In other words, you
    /// have `(1 << pixel_to_texel_shift)` texels per VRAM pixel.
    pixel_to_texel_shift: u8,
    /// Cache for color LUTs
    #[serde(with = "serde_big_array::BigArray")]
    clut_cache: [Pixel; 0x100],
    /// Key used to see if the CLUT cache should be reloaded (set to !0 when invalidated)
    clut_tag: u32,
    /// Texture cache
    #[serde(with = "serde_big_array::BigArray")]
    texture_cache: [CacheLine; 0x100],
}

impl TextureMapper {
    pub fn new() -> TextureMapper {
        TextureMapper {
            draw_mode: DrawMode::new(0),
            tex_window: TextureWindow::new(),
            u_mask: 0,
            v_mask: 0,
            u_offset: 0,
            v_offset: 0,
            pixel_to_texel_shift: 0,
            clut_cache: [Pixel(0); 0x100],
            clut_tag: !0,
            texture_cache: [CacheLine::new(); 0x100],
        }
    }

    pub fn reset(&mut self) {
        self.draw_mode = DrawMode::new(0);
        self.tex_window.set(0);
        self.update_texture_params();
        self.cache_invalidate();
    }

    pub fn cache_invalidate(&mut self) {
        self.clut_tag = !0;

        for l in self.texture_cache.iter_mut() {
            l.invalidate();
        }
    }

    pub fn set_clut(&mut self, clut: u32, vram: &VRam) {
        let pts = self.pixel_to_texel_shift as u32;
        if pts == 0 {
            // We're in "truecolor" mode, the palette is not in use
            return;
        }

        let clut = (clut >> 16) & 0x7fff;

        // The key is unique for a given clut coord and texture depth
        let tag = clut | (pts << 16);

        if tag == self.clut_tag {
            // Already cached
            return;
        }

        self.clut_tag = tag;

        let clut_x = ((clut & 0x3f) << 4) as u16;
        let clut_y = ((clut >> 6) & 0x1ff) as u16;

        // let clut_off = (clut_y * 1024 + clut_x) as usize;

        // 256 for 8bpp, 16 for 4bpp
        let nentries = 256u16 >> ((pts - 1) * 4);

        // Dino Crisis 2 sets a weird palette when displaying the Superintendent hologram:
        //
        // clut_x = 1008, clut_y = 511, nentries = 256
        //
        // Naturally this goes out of bounds, but I presume that we're supposed to wrap around (not
        // tested on real hardware)
        for i in 0..nentries {
            self.clut_cache[i as usize] = vram.native_pixel((clut_x + i) & 0x3ff, clut_y)
        }
    }

    pub fn set_draw_mode(&mut self, mode: u32) {
        let new_mode = DrawMode::new(mode);

        if new_mode.texture_page_x() != self.draw_mode.texture_page_x()
            || new_mode.texture_page_y() != self.draw_mode.texture_page_y()
            || new_mode.texture_depth() != self.draw_mode.texture_depth()
            || new_mode.texture_disable() != self.draw_mode.texture_disable()
        {
            self.cache_invalidate();
        }

        self.draw_mode = new_mode;
        self.update_texture_params();
    }

    pub fn update_mode_from_poly(&mut self, mode: u32) {
        self.draw_mode.update_from_poly(mode);
        self.update_texture_params();
    }

    pub fn set_tex_window(&mut self, tw: u32) {
        self.tex_window.set(tw);
        self.update_texture_params();
    }

    fn update_texture_params(&mut self) {
        self.pixel_to_texel_shift = self.draw_mode.pixel_to_texel_shift();
        self.u_mask = self.tex_window.u_mask();
        self.v_mask = self.tex_window.v_mask();

        self.u_offset = u16::from(self.tex_window.u_offset());
        self.v_offset = u16::from(self.tex_window.v_offset());

        // Add texture page offset.
        let tp_x = self.draw_mode.texture_page_x();
        let tp_y = self.draw_mode.texture_page_y();

        // `texture_page_x` is in number of VRAM pixels, convert it to a number of texels
        self.u_offset += tp_x << self.pixel_to_texel_shift;
        self.v_offset += tp_y;
    }

    pub fn get_texel(&mut self, u: u8, v: u8, vram: &VRam) -> Pixel {
        let pts = u16::from(self.pixel_to_texel_shift);
        let fb_u = u16::from(u & self.u_mask) + self.u_offset;
        let fb_v = u16::from(v & self.v_mask) + self.v_offset;

        let fb_x = (fb_u >> pts) & 0x3ff;
        let fb_y = fb_v;

        let cache_index = if pts == 2 {
            // 4bpp: texture cache is organized in blocks of 4 x 64 cachelines
            //
            // Each cacheline is 4 16bpp pixel wide (16 4bpp pixels) that means that the total
            // block size is 64x64 texels.
            ((fb_y << 2) | ((fb_x >> 2) & 3)) & 0xff
        } else {
            // 8/16bpp: texture cache is organized in blocks of 8x32 cachelines
            //
            // Each cacheline is 4 16bpp pixel wide (8 8bpp pixels) that means that the total
            // block size is 64x32 texels for 8bpp and 32x64 for 16bpp.
            ((fb_y << 3) | ((fb_x >> 2) & 7)) & 0xff
        };

        let cacheline = &mut self.texture_cache[usize::from(cache_index)];

        let tag = (u32::from(fb_y) * 1024 + u32::from(fb_x)) & !3;

        if cacheline.tag != tag {
            // Need to fetch the cacheline
            cacheline.tag = tag;

            for i in 0..4u16 {
                cacheline.pixels[usize::from(i)] = vram.native_pixel((fb_x & !3) + i, fb_y);
            }
        }

        let raw = cacheline.pixels[usize::from(fb_x & 3)];

        if pts == 0 {
            // True color mode, we return the texture value as-is
            return raw;
        }

        // We're using a 4 or 8bpp paletted texture
        let raw = raw.to_mbgr1555();

        // XXX should we cache some of this in `update_texture_params`?

        // 4 for 4bpp, 8 for 8bpp
        let bits_per_texel = 16 >> pts;
        // 3 for 4bpp, 1 for 8bpp
        let u_mask = pts + (pts >> 1);
        // 2 for 4bpp, 3 for 8bpp
        let u_mul = 4 - pts;
        // 0, 4, 8 or 12 for 4bpp, 0 or 8 for 8bpp
        let u_shift = (fb_u & u_mask) << u_mul;
        // 0xf for 4bpp, 0xff for 8bpp
        let key_mask = (1 << bits_per_texel) - 1;

        let clut_key = (raw >> u_shift) & key_mask;

        self.clut_cache[clut_key as usize]
    }
}

/// Generic representation of a Pixel, used to represent both 24bit 888RGB colors and 1555xRGB VRAM
/// pixels. Internal representation is a single `u32` containing the values as 8888xRGB, meaning
/// that it can normally be passed straight to the frontend without conversion
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq)]
pub struct Pixel(pub u32);

impl Pixel {
    pub fn black() -> Pixel {
        Pixel(0)
    }

    /// For texels this method returns true if the pixel is all zeroes (which means that the pixel
    /// shouldn't be drawn)
    pub fn is_nul(self) -> bool {
        self.0 == 0
    }

    pub fn from_mbgr1555(mbgr: u16) -> Pixel {
        let r = (mbgr & 0x1f) as u32;
        let g = ((mbgr >> 5) & 0x1f) as u32;
        let b = ((mbgr >> 10) & 0x1f) as u32;
        let m = ((mbgr >> 15) & 1) as u32;

        // We want to extend to RGB888 so we copy the 3 MSBs to the LSBs (this way black remains
        // black and white remains white)
        let r = (r << 3) | (r >> 2);
        let g = (g << 3) | (g >> 2);
        let b = (b << 3) | (b >> 2);

        Pixel(b | (g << 8) | (r << 16) | (m << 24))
    }

    pub fn from_rgb(r: u8, g: u8, b: u8) -> Pixel {
        let r = r as u32;
        let g = g as u32;
        let b = b as u32;

        Pixel(b | (g << 8) | (r << 16))
    }

    fn from_command(cmd: u32) -> Pixel {
        let r = cmd & 0xff;
        let g = (cmd >> 8) & 0xff;
        let b = (cmd >> 16) & 0xff;

        Pixel(b | (g << 8) | (r << 16))
    }

    pub fn to_rgb888(self) -> u32 {
        self.0 & 0xff_ff_ff
    }

    pub fn to_mbgr1555(self) -> u16 {
        let m = self.mask() as u16;
        let r = (self.red() >> 3) as u16;
        let g = (self.green() >> 3) as u16;
        let b = (self.blue() >> 3) as u16;

        (m << 15) | (b << 10) | (g << 5) | r
    }

    pub fn red(self) -> u8 {
        (self.0 >> 16) as u8
    }

    pub fn green(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub fn blue(self) -> u8 {
        (self.0 & 0xff) as u8
    }

    pub fn mask(self) -> bool {
        (self.0 >> 24) != 0
    }

    pub fn set_mask(&mut self) {
        self.0 |= 1 << 24;
    }

    // Returns the 16bpp color as two bytes)
    pub fn vram_bytes(self) -> [u8; 2] {
        self.to_mbgr1555().to_le_bytes()
    }

    /// Blend `self` (the foreground pixel) and `bg_pixel` using the provided transparency mode
    fn apply_transparency(&mut self, bg_pixel: Pixel, mode: TransparencyFunction) {
        // XXX this is a very naive, and probably very slow implementation. We could probably do
        // that processing using the whole pixel value at once using some clever carry handling.
        let f_r = (self.0 >> 16) & 0xff;
        let f_g = (self.0 >> 8) & 0xff;
        let f_b = self.0 & 0xff;

        let b_r = (bg_pixel.0 >> 16) & 0xff;
        let b_g = (bg_pixel.0 >> 8) & 0xff;
        let b_b = bg_pixel.0 & 0xff;

        let o_r;
        let o_g;
        let o_b;

        match mode {
            TransparencyFunction::Average => {
                o_r = (f_r + b_r) >> 1;
                o_g = (f_g + b_g) >> 1;
                o_b = (f_b + b_b) >> 1;
            }
            TransparencyFunction::Add => {
                let s_r = f_r + b_r;
                let s_g = f_g + b_g;
                let s_b = f_b + b_b;

                o_r = if s_r > 0xff { 0xff } else { s_r };
                o_g = if s_g > 0xff { 0xff } else { s_g };
                o_b = if s_b > 0xff { 0xff } else { s_b };
            }
            TransparencyFunction::Sub => {
                let s_r = b_r.wrapping_sub(f_r);
                let s_g = b_g.wrapping_sub(f_g);
                let s_b = b_b.wrapping_sub(f_b);

                o_r = if s_r > 0xff { 0 } else { s_r };
                o_g = if s_g > 0xff { 0 } else { s_g };
                o_b = if s_b > 0xff { 0 } else { s_b };
            }
            TransparencyFunction::QuarterAdd => {
                let f_r = f_r >> 2;
                let f_g = f_g >> 2;
                let f_b = f_b >> 2;

                let s_r = f_r + b_r;
                let s_g = f_g + b_g;
                let s_b = f_b + b_b;

                o_r = if s_r > 0xff { 0xff } else { s_r };
                o_g = if s_g > 0xff { 0xff } else { s_g };
                o_b = if s_b > 0xff { 0xff } else { s_b };
            }
        };

        self.0 = (o_r << 16) | (o_g << 8) | o_b;
    }
}

impl fmt::Display for Pixel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "#{:02x}{:02x}{:02x}",
            self.red(),
            self.green(),
            self.blue()
        )
    }
}

impl fmt::Debug for Pixel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

/// Description of a vertex with position, texture and shading (depending on the command)
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct Vertex {
    position: Position,
    color: Pixel,
    /// Texture u coordinate, relative to the current texture page
    u: u8,
    /// Texture v coordinate, relative to the current texture page
    v: u8,
    /// The order in which the vertices are received is sometimes important, so we keep track of
    /// the index in the original command here.
    index: u8,
}

impl Vertex {
    fn new(index: u8) -> Vertex {
        Vertex {
            position: Position::new(0, 0),
            color: Pixel::from_command(0),
            u: 0,
            v: 0,
            index,
        }
    }

    fn set_position(&mut self, p: u32) {
        self.position = Position::from_command(p);
    }

    fn set_texture_uv(&mut self, uv: u32) {
        self.u = uv as u8;
        self.v = (uv >> 8) as u8;
    }

    fn x(&self) -> i32 {
        self.position.x
    }

    fn y(&self) -> i32 {
        self.position.y
    }

    fn red(&self) -> i32 {
        self.color.red() as i32
    }

    fn green(&self) -> i32 {
        self.color.green() as i32
    }

    fn blue(&self) -> i32 {
        self.color.blue() as i32
    }
}

/// Extend a signed value on `n` bit to an i32
fn extend_to_i32(val: u32, n: usize) -> i32 {
    let shift = 32 - n;

    ((val << shift) as i32) >> shift
}

/// Description of the various GP0 commands
pub struct CommandHandler {
    pub handler: fn(&mut Rasterizer, params: &[u32]),
    /// Actual length of the command, in number of FIFO words
    pub len: u8,
}

fn cmd_handle_poly_quad<Transparency, Texture, Shading>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
    Shading: ShadingMode,
{
    let mut vertices = [
        Vertex::new(0),
        Vertex::new(1),
        Vertex::new(2),
        Vertex::new(3),
    ];

    let mut index = 0;
    let mut cur_color = Pixel::black();
    let mut clut = 0;
    let mut mode = 0;

    // Load the vertex data from the command
    for (v, vertex) in vertices.iter_mut().enumerate() {
        if v == 0 || Shading::is_shaded() {
            cur_color = Pixel::from_command(params[index]);
            index += 1;
        }

        vertex.color = cur_color;

        vertex.set_position(params[index]);
        index += 1;

        // Add the draw offset
        vertex.position.x += rasterizer.draw_offset_x;
        vertex.position.y += rasterizer.draw_offset_y;

        // XXX Not sure if that's correct, Mednafen does it differently, but it does fix the
        // flickering on FFVIII's Dollet bridge
        vertex.position.x = extend_to_i32(vertex.position.x as u32, 11);
        vertex.position.y = extend_to_i32(vertex.position.y as u32, 11);

        vertex.position.x <<= rasterizer.vram.upscale_shift;
        vertex.position.y <<= rasterizer.vram.upscale_shift;

        if Texture::is_textured() {
            if v == 0 {
                clut = params[index];
            } else if v == 1 {
                mode = params[index];
            }
            vertex.set_texture_uv(params[index]);
            index += 1;
        }
    }

    if Texture::is_textured() {
        // Needs to happen in this order since `mode` can change the CLUT in use
        rasterizer.tex_mapper.update_mode_from_poly(mode);
        rasterizer.set_clut(clut);
    }

    if rasterizer.draw_polygons {
        let triangle = [
            vertices[0].clone(),
            vertices[1].clone(),
            vertices[2].clone(),
        ];
        rasterizer.draw_triangle::<Transparency, Texture, Shading>(triangle);

        let triangle = [
            vertices[1].clone(),
            vertices[2].clone(),
            vertices[3].clone(),
        ];
        rasterizer.draw_triangle::<Transparency, Texture, Shading>(triangle);
    }

    if rasterizer.draw_wireframe {
        // Draw the quad outline
        let color = Pixel::from_rgb(0x00, 0x00, 0xff);

        for v in &mut vertices {
            v.color = color;
        }

        rasterizer.draw_line::<Opaque, NoShading>(vertices[0].clone(), vertices[1].clone());
        rasterizer.draw_line::<Opaque, NoShading>(vertices[1].clone(), vertices[3].clone());
        rasterizer.draw_line::<Opaque, NoShading>(vertices[3].clone(), vertices[2].clone());
        rasterizer.draw_line::<Opaque, NoShading>(vertices[2].clone(), vertices[0].clone());

        // Draw the diagonal in a different color
        let mut diag0 = vertices[1].clone();
        let mut diag1 = vertices[2].clone();

        let color = Pixel::from_rgb(0x00, 0xff, 0xff);
        diag0.color = color;
        diag1.color = color;
        rasterizer.draw_line::<Opaque, NoShading>(diag0, diag1);
    }
}

fn cmd_handle_poly_tri<Transparency, Texture, Shading>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
    Shading: ShadingMode,
{
    let mut vertices = [Vertex::new(0), Vertex::new(1), Vertex::new(2)];

    let mut index = 0;
    let mut cur_color = Pixel::black();

    let mut clut = 0;
    let mut mode = 0;

    // Load the vertex data from the command
    for (v, vertex) in vertices.iter_mut().enumerate() {
        if v == 0 || Shading::is_shaded() {
            cur_color = Pixel::from_command(params[index]);
            index += 1;
        }
        vertex.color = cur_color;

        vertex.set_position(params[index]);
        index += 1;

        // Add the draw offset
        vertex.position.x += rasterizer.draw_offset_x;
        vertex.position.y += rasterizer.draw_offset_y;

        // XXX Not sure if that's correct, Mednafen does it differently, but it does fix the
        // flickering on FFVIII's Dollet bridge
        vertex.position.x = extend_to_i32(vertex.position.x as u32, 11);
        vertex.position.y = extend_to_i32(vertex.position.y as u32, 11);

        vertex.position.x <<= rasterizer.vram.upscale_shift;
        vertex.position.y <<= rasterizer.vram.upscale_shift;

        if Texture::is_textured() {
            if v == 0 {
                clut = params[index];
            } else if v == 1 {
                mode = params[index];
            }
            vertex.set_texture_uv(params[index]);
            index += 1;
        }
    }

    if Texture::is_textured() {
        // Needs to happen in this order since `mode` can change the CLUT in use
        rasterizer.tex_mapper.update_mode_from_poly(mode);
        rasterizer.set_clut(clut);
    }

    if rasterizer.draw_polygons {
        rasterizer.draw_triangle::<Transparency, Texture, Shading>(vertices.clone());
    }

    if rasterizer.draw_wireframe {
        // Draw the triangle's outline
        let color = Pixel::from_rgb(0x00, 0xff, 0x00);

        for v in &mut vertices {
            v.color = color;
        }

        rasterizer.draw_line::<Opaque, NoShading>(vertices[0].clone(), vertices[1].clone());
        rasterizer.draw_line::<Opaque, NoShading>(vertices[1].clone(), vertices[2].clone());
        rasterizer.draw_line::<Opaque, NoShading>(vertices[2].clone(), vertices[0].clone());
    }
}

fn cmd_handle_rect<Transparency, Texture>(
    rasterizer: &mut Rasterizer,
    params: &[u32],
    dimensions: Option<(i32, i32)>,
) where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    let mut origin = Vertex::new(0);
    let mut index = 0;

    origin.color = Pixel::from_command(params[index]);
    index += 1;

    origin.set_position(params[index]);
    index += 1;

    // Add the draw offset
    origin.position.x += rasterizer.draw_offset_x;
    origin.position.y += rasterizer.draw_offset_y;

    // Without this we get some flickering on the bridge in Dollet in FFVIII because the coords
    // become negative. I wonder if that's the best way to do it though.
    origin.position.x = extend_to_i32(origin.position.x as u32, 11);
    origin.position.y = extend_to_i32(origin.position.y as u32, 11);

    if Texture::is_textured() {
        rasterizer.set_clut(params[index]);
        origin.set_texture_uv(params[index]);
        index += 1;
    }

    let (w, h) = match dimensions {
        Some(d) => d,
        None => {
            // Variable dimensions
            let dim = params[index];

            let w = dim & 0x3ff;
            let h = (dim >> 16) & 0x1ff;

            (w as i32, h as i32)
        }
    };

    rasterizer.draw_rect::<Transparency, Texture>(origin, w, h);
}

fn cmd_handle_rect_variable<Transparency, Texture>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    cmd_handle_rect::<Transparency, Texture>(rasterizer, params, None);
}

fn cmd_handle_rect_1x1<Transparency, Texture>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    cmd_handle_rect::<Transparency, Texture>(rasterizer, params, Some((1, 1)));
}

fn cmd_handle_rect_8x8<Transparency, Texture>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    cmd_handle_rect::<Transparency, Texture>(rasterizer, params, Some((8, 8)));
}

fn cmd_handle_rect_16x16<Transparency, Texture>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    cmd_handle_rect::<Transparency, Texture>(rasterizer, params, Some((16, 16)));
}

fn cmd_handle_polyline<Transparency, Shading>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Shading: ShadingMode,
{
    let mut index = 0;

    let (opcode, start_vertex) = match &rasterizer.state {
        // We're in the middle of a polyline, use the previous segment's end as start vertex
        State::PolyLine(o, v) => (*o, v.clone()),
        // New command
        _ => {
            let mut v = Vertex::new(0);
            // Command + color
            let cmd = params[index];
            let opcode = cmd >> 24;
            v.color = Pixel::from_command(cmd);
            index += 1;

            // Start position
            v.set_position(params[index]);
            index += 1;

            v.position.x += rasterizer.draw_offset_x;
            v.position.y += rasterizer.draw_offset_y;
            v.position.x <<= rasterizer.vram.upscale_shift;
            v.position.y <<= rasterizer.vram.upscale_shift;

            (opcode as u8, v)
        }
    };

    let mut end_vertex = Vertex::new(0);

    if Shading::is_shaded() {
        end_vertex.color = Pixel::from_command(params[index]);
        index += 1;
    } else {
        end_vertex.color = start_vertex.color;
    };
    end_vertex.set_position(params[index]);
    end_vertex.position.x += rasterizer.draw_offset_x;
    end_vertex.position.y += rasterizer.draw_offset_y;

    end_vertex.position.x <<= rasterizer.vram.upscale_shift;
    end_vertex.position.y <<= rasterizer.vram.upscale_shift;

    rasterizer.draw_line::<Transparency, Shading>(start_vertex, end_vertex.clone());

    rasterizer.state = State::PolyLine(opcode, end_vertex);
}

fn cmd_handle_line<Transparency, Shading>(rasterizer: &mut Rasterizer, params: &[u32])
where
    Transparency: TransparencyMode,
    Shading: ShadingMode,
{
    let mut index = 0;

    let mut start_vertex = Vertex::new(0);
    // Command + color
    start_vertex.color = Pixel::from_command(params[index]);
    index += 1;

    // Start position
    start_vertex.set_position(params[index]);
    index += 1;
    start_vertex.position.x += rasterizer.draw_offset_x;
    start_vertex.position.y += rasterizer.draw_offset_y;
    start_vertex.position.x <<= rasterizer.vram.upscale_shift;
    start_vertex.position.y <<= rasterizer.vram.upscale_shift;

    let mut end_vertex = Vertex::new(0);

    if Shading::is_shaded() {
        end_vertex.color = Pixel::from_command(params[index]);
        index += 1;
    } else {
        end_vertex.color = start_vertex.color;
    };

    end_vertex.set_position(params[index]);
    end_vertex.position.x += rasterizer.draw_offset_x;
    end_vertex.position.y += rasterizer.draw_offset_y;

    end_vertex.position.x <<= rasterizer.vram.upscale_shift;
    end_vertex.position.y <<= rasterizer.vram.upscale_shift;

    rasterizer.draw_line::<Transparency, Shading>(start_vertex, end_vertex);
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct VRamStore {
    x_min: u16,
    x_max: u16,
    y_max: u16,
    /// Current X coordinate, from x_min to x_max
    x: u16,
    /// Current Y coordinate, from y_min to y_max
    y: u16,
}

impl VRamStore {
    fn new(left: u16, top: u16, width: u16, height: u16) -> VRamStore {
        debug_assert!(width > 0);
        debug_assert!(height > 0);

        VRamStore {
            x_min: left,
            x_max: left + width,
            y_max: top + height,
            x: left,
            y: top,
        }
    }

    fn target_vram_offset(&self) -> (u16, u16) {
        let x = self.x & 0x3ff;
        let y = self.y & 0x1ff;

        (x, y)
    }

    fn next(&mut self) -> Option<()> {
        self.x += 1;

        if self.x == self.x_max {
            self.x = self.x_min;
            self.y += 1;

            if self.y == self.y_max {
                // End of transfer
                return None;
            }
        }

        Some(())
    }
}

fn cmd_vram_copy(rasterizer: &mut Rasterizer, params: &[u32]) {
    let src = params[1];
    let dst = params[2];
    let dim = params[3];

    let src_x = ((src & 0x3ff) as i32) << rasterizer.vram.upscale_shift;
    let src_y = (((src >> 16) & 0x3ff) as i32) << rasterizer.vram.upscale_shift;
    let dst_x = ((dst & 0x3ff) as i32) << rasterizer.vram.upscale_shift;
    let dst_y = (((dst >> 16) & 0x3ff) as i32) << rasterizer.vram.upscale_shift;

    let (width, height) = vram_access_dimensions(dim, false);

    // From mednafen, is it because it's used as a temporary buffer for the copy? Is there a
    // different buffer?
    rasterizer.tex_mapper.cache_invalidate();

    let xmask = (0x400 << rasterizer.vram.upscale_shift) - 1;
    let ymask = (0x200 << rasterizer.vram.upscale_shift) - 1;

    for y in 0..height {
        let sy = (y + src_y) & ymask;
        let ty = (y + dst_y) & ymask;

        for x in (0..width).step_by(128) {
            // The use of a 128px intermediate buffer is taken from mednafen
            // XXX should we scale with the upscale_shift?
            let mut copy_buf: [Pixel; 128] = [Pixel(0); 128];

            let w = std::cmp::min(width - x, 128);

            for dx in 0..w {
                let sx = (src_x + x + dx) & xmask;
                copy_buf[dx as usize] = rasterizer.read_pixel(sx, sy);
            }

            for dx in 0..w {
                let tx = (dst_x + x + dx) & xmask;

                let p = copy_buf[dx as usize];

                // VRAM copy respects mask bit settings
                rasterizer.draw_pixel::<Opaque, NoTexture>(tx, ty, p);
            }
        }
    }
}

fn cmd_vram_store(rasterizer: &mut Rasterizer, params: &[u32]) {
    let pos = params[1];
    let dim = params[2];

    let left = (pos & 0x3ff) as u16;
    let top = ((pos >> 16) & 0x3ff) as u16;

    let (width, height) = vram_access_dimensions(dim, false);

    rasterizer.tex_mapper.cache_invalidate();

    let store = VRamStore::new(left, top, width as u16, height as u16);

    rasterizer.state = State::VRamStore(store);
}

fn cmd_vram_load(rasterizer: &mut Rasterizer, params: &[u32], frame_channel: &mpsc::Sender<Frame>) {
    let pos = params[1];
    let dim = params[2];

    let left = (pos & 0x3ff) as u16;
    let top = ((pos >> 16) & 0x3ff) as u16;

    let (width, height) = vram_access_dimensions(dim, true);

    rasterizer.tex_mapper.cache_invalidate();

    if width == 0 || height == 0 {
        // Nothing to do
        return;
    }

    let frame = rasterizer.copy_vram_rect(left, top, width as u16, height as u16);

    frame_channel.send(frame).unwrap();
}

/// Fill a rectangle with a solid color
fn cmd_fill_rect(rasterizer: &mut Rasterizer, params: &[u32]) {
    let color = Pixel::from_command(params[0]);
    let dst = params[1];
    let dim = params[2];

    let start_x = (dst & 0x3f0) as u16;
    let start_y = ((dst >> 16) & 0x3ff) as u16;

    let width = (((dim & 0x3ff) + 0xf) & !0xf) as u16;
    let height = ((dim >> 16) & 0x1ff) as u16;

    // XXX Pretty sure there's no dithering for this commands
    let color = rasterizer.truncate_color(color);

    for y in 0..height {
        let y_pos = (start_y + y) & 511;

        if !rasterizer.can_draw_to_line(y_pos as i32) {
            continue;
        }

        for x in 0..width {
            // Fill rect is supposed to ignore clip space and mask completely.
            //
            // XXX Probably worth adding a test just in case.
            let x_pos = (start_x + x) & 1023;

            rasterizer.vram.set_native_pixel(x_pos, y_pos, color);
        }
    }
}

fn cmd_tex_window(rasterizer: &mut Rasterizer, params: &[u32]) {
    rasterizer.tex_mapper.set_tex_window(params[0]);
}

fn cmd_clip_top_left(rasterizer: &mut Rasterizer, params: &[u32]) {
    let clip = params[0];

    rasterizer.clip_x_min = ((clip & 0x3ff) as i32) << rasterizer.vram.upscale_shift;
    rasterizer.clip_y_min = (((clip >> 10) & 0x3ff) as i32) << rasterizer.vram.upscale_shift;
}

fn cmd_clip_bot_right(rasterizer: &mut Rasterizer, params: &[u32]) {
    let clip = params[0];

    rasterizer.clip_x_max = ((clip & 0x3ff) as i32) << rasterizer.vram.upscale_shift;
    rasterizer.clip_y_max = (((clip >> 10) & 0x3ff) as i32) << rasterizer.vram.upscale_shift;

    // The clip is inclusive, so we need to offset when upscaling
    rasterizer.clip_x_max += (1 << rasterizer.vram.upscale_shift) - 1;
    rasterizer.clip_y_max += (1 << rasterizer.vram.upscale_shift) - 1;
}

fn cmd_draw_mode(rasterizer: &mut Rasterizer, params: &[u32]) {
    let mode = params[0];

    rasterizer.tex_mapper.set_draw_mode(mode);
    rasterizer.maybe_rebuild_dither_table();
}

fn cmd_draw_offset(rasterizer: &mut Rasterizer, params: &[u32]) {
    let off = params[0];

    let off_x = off & 0x7ff;
    let off_y = (off >> 11) & 0x7ff;

    // Sign-extend
    rasterizer.draw_offset_x = extend_to_i32(off_x, 11);
    rasterizer.draw_offset_y = extend_to_i32(off_y, 11);
}

fn cmd_mask_settings(rasterizer: &mut Rasterizer, params: &[u32]) {
    let mask_settings = params[0] & 0x3f_ffff;

    rasterizer.mask_settings.set(mask_settings);
}

fn cmd_clear_cache(rasterizer: &mut Rasterizer, _params: &[u32]) {
    rasterizer.tex_mapper.cache_invalidate();
}

/// Placeholder function
fn cmd_unimplemented(_rasterizer: &mut Rasterizer, params: &[u32]) {
    warn!("GPU command {:08x}", params[0]);
}

/// LUT for all GP0 commands (indexed by opcode, bits[31:24] of the first command word)
pub static GP0_COMMANDS: [CommandHandler; 0x100] = [
    // 0x00
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_clear_cache,
        len: 1,
    },
    CommandHandler {
        handler: cmd_fill_rect,
        len: 3,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0x10
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0x20
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, NoShading>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, NoShading>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, NoShading>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, NoShading>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, TextureBlending, NoShading>,
        len: 7,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, TextureRaw, NoShading>,
        len: 7,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, TextureBlending, NoShading>,
        len: 7,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, TextureRaw, NoShading>,
        len: 7,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, NoShading>,
        len: 5,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, NoShading>,
        len: 5,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, NoShading>,
        len: 5,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, NoShading>,
        len: 5,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, TextureBlending, NoShading>,
        len: 9,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, TextureRaw, NoShading>,
        len: 9,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, TextureBlending, NoShading>,
        len: 9,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, TextureRaw, NoShading>,
        len: 9,
    },
    // 0x30
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, Shaded>,
        len: 6,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, Shaded>,
        len: 6,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, Shaded>,
        len: 6,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, Shaded>,
        len: 6,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, TextureBlending, Shaded>,
        len: 9,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Opaque, TextureRaw, Shaded>,
        len: 9,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, TextureBlending, Shaded>,
        len: 9,
    },
    CommandHandler {
        handler: cmd_handle_poly_tri::<Transparent, TextureRaw, Shaded>,
        len: 9,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, Shaded>,
        len: 8,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, Shaded>,
        len: 8,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, Shaded>,
        len: 8,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, Shaded>,
        len: 8,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, TextureBlending, Shaded>,
        len: 12,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Opaque, TextureRaw, Shaded>,
        len: 12,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, TextureBlending, Shaded>,
        len: 12,
    },
    CommandHandler {
        handler: cmd_handle_poly_quad::<Transparent, TextureRaw, Shaded>,
        len: 12,
    },
    // 0x40
    CommandHandler {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
    },
    // 0x50
    CommandHandler {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
    },
    // 0x60
    CommandHandler {
        handler: cmd_handle_rect_variable::<Opaque, NoTexture>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_variable::<Opaque, NoTexture>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_variable::<Transparent, NoTexture>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_variable::<Transparent, NoTexture>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_variable::<Opaque, TextureBlending>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_rect_variable::<Opaque, TextureRaw>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_rect_variable::<Transparent, TextureBlending>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_rect_variable::<Transparent, TextureRaw>,
        len: 4,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Opaque, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Opaque, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Transparent, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Transparent, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Opaque, TextureBlending>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Opaque, TextureRaw>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Transparent, TextureBlending>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_1x1::<Transparent, TextureRaw>,
        len: 3,
    },
    // 0x70
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Opaque, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Opaque, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Transparent, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Transparent, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Opaque, TextureBlending>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Opaque, TextureRaw>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Transparent, TextureBlending>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_8x8::<Transparent, TextureRaw>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Opaque, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Opaque, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Transparent, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Transparent, NoTexture>,
        len: 2,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Opaque, TextureBlending>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Opaque, TextureRaw>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Transparent, TextureBlending>,
        len: 3,
    },
    CommandHandler {
        handler: cmd_handle_rect_16x16::<Transparent, TextureRaw>,
        len: 3,
    },
    // 0x80
    CommandHandler {
        handler: cmd_vram_copy,
        len: 4,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0x90
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0xa0
    CommandHandler {
        handler: cmd_vram_store,
        len: 3,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0xb0
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0xc0
    CommandHandler {
        // This one is handled explicitly elsewhere, only `len` matters
        handler: cmd_unimplemented,
        len: 3,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0xd0
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0xe0
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_draw_mode,
        len: 1,
    },
    CommandHandler {
        handler: cmd_tex_window,
        len: 1,
    },
    CommandHandler {
        handler: cmd_clip_top_left,
        len: 1,
    },
    CommandHandler {
        handler: cmd_clip_bot_right,
        len: 1,
    },
    CommandHandler {
        handler: cmd_draw_offset,
        len: 1,
    },
    CommandHandler {
        handler: cmd_mask_settings,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    // 0xf0
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
    CommandHandler {
        handler: cmd_unimplemented,
        len: 1,
    },
];

mod serialize_dither_table {
    use serde::{de::Deserializer, ser::Serializer};

    pub fn serialize<S>(_table: &[[[u8; 0x200]; 4]; 4], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // We don't have to save the dither tables since we can regenerate them
        serializer.serialize_unit()
    }

    pub fn deserialize<'de, D>(_deserializer: D) -> Result<[[[u8; 0x200]; 4]; 4], D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok([[[0; 0x200]; 4]; 4])
    }
}

/// Texture cache line containing 4 pixels
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct CacheLine {
    pixels: [Pixel; 4],
    tag: u32,
}

impl CacheLine {
    const TAG_INVALID: u32 = !0;

    fn new() -> CacheLine {
        CacheLine {
            pixels: [Pixel(0); 4],
            tag: CacheLine::TAG_INVALID,
        }
    }

    fn invalidate(&mut self) {
        self.tag = CacheLine::TAG_INVALID;
    }
}

struct VRam {
    pixels: Vec<Pixel>,
    /// Upscale shift value. 0 for native.
    upscale_shift: u8,
}

impl VRam {
    fn with_upscale_shift(upscale_shift: u8) -> VRam {
        VRam {
            pixels: vec![Pixel::black(); (1024 << upscale_shift) * (512 << upscale_shift)],
            upscale_shift,
        }
    }

    /// Returns the pixel at x, y where x an y are in native 1x coordinates.
    fn native_pixel(&self, x: u16, y: u16) -> Pixel {
        self.pixel(
            u32::from(x) << self.upscale_shift,
            u32::from(y) << self.upscale_shift,
        )
    }

    /// Sets the pixel at x, y where x an y are in native 1x coordinates.
    fn set_native_pixel(&mut self, x: u16, y: u16, p: Pixel) {
        for yo in 0..(1 << self.upscale_shift) {
            for xo in 0..(1 << self.upscale_shift) {
                self.set_pixel(
                    (u32::from(x) << self.upscale_shift) + xo,
                    (u32::from(y) << self.upscale_shift) + yo,
                    p,
                )
            }
        }
    }

    /// Returns the pixel at x, y where x and y are in upscaled coordinates
    fn pixel(&self, x: u32, y: u32) -> Pixel {
        let x = x as usize;
        let y = y as usize;

        self.pixels[(1024 << self.upscale_shift) * y + x]
    }

    /// Sets the pixel at x, y where x and y are in upscaled coordinates
    fn set_pixel(&mut self, x: u32, y: u32, p: Pixel) {
        let x = x as usize;
        let y = y as usize;

        self.pixels[(1024 << self.upscale_shift) * y + x] = p
    }
}

impl Serialize for VRam {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tup = serializer.serialize_tuple(1024 * 512)?;

        for y in 0..512 {
            for x in 0..1024 {
                tup.serialize_element(&self.native_pixel(x, y))?;
            }
        }

        tup.end()
    }
}

impl<'de> Deserialize<'de> for VRam {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VRamVisitor {
            phantom: PhantomData<VRam>,
        }

        impl<'de> Visitor<'de> for VRamVisitor {
            type Value = VRam;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "an array of 1024 * 512 pixels")
            }

            fn visit_seq<S>(self, mut seq: S) -> std::result::Result<VRam, S::Error>
            where
                S: SeqAccess<'de>,
            {
                let mut vram = VRam::with_upscale_shift(0);

                for y in 0..512 {
                    for x in 0..1024 {
                        let p = match seq.next_element()? {
                            Some(p) => p,
                            None => return Err(de::Error::invalid_length(y * 1024 + x, &self)),
                        };

                        vram.set_native_pixel(x as u16, y as u16, p);
                    }
                }

                Ok(vram)
            }
        }

        let visitor = VRamVisitor {
            phantom: PhantomData,
        };
        deserializer.deserialize_tuple(1024 * 512, visitor)
    }
}
