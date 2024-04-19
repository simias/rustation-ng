mod commands;
mod fifo;
mod rasterizer;

use super::cpu::CPU_FREQ_HZ;
use super::{irq, sync, timers, AccessWidth, Addressable, CycleCount, Psx};
use commands::{Command, Position};
pub use rasterizer::{Frame, Pixel, RasterizerOption};

const GPUSYNC: sync::SyncToken = sync::SyncToken::Gpu;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Gpu {
    state: State,
    rasterizer: rasterizer::Handle,
    video_standard: VideoStandard,
    /// Current value of the display mode
    display_mode: DisplayMode,
    /// Number of the first line displayed on the screen
    display_line_start: u16,
    /// Number of the first line *not* displayed on the screen
    display_line_end: u16,
    /// Number of the first column displayed on the screen
    display_column_start: u16,
    /// Number of the first column *not* displayed on the screen
    display_column_end: u16,
    /// True when we're between [display_line_start; display_line_end[
    display_active: bool,
    /// First column of the display area in VRAM
    display_vram_x_start: u16,
    /// First line of the display area in VRAM
    display_vram_y_start: u16,
    /// Current value of the draw mode
    draw_mode: DrawMode,
    /// DMA request direction
    dma_direction: DmaDirection,
    /// GP0 command FIFO
    command_fifo: fifo::CommandFifo,
    /// Variable used to simulate the time taken by draw commands. Taken from mednafen. This value
    /// can become negative (we don't start a new draw command if it's negative, but a command
    /// already started won't stop using cycles even if this goes below 0).
    draw_time_budget: CycleCount,
    /// Since the ratio of GPU-to-CPU frequency isn't an integer we can end up with fractional
    /// cycles in `run`. We store them here (this is a fixed point value with 16 fractional bits).
    remaining_fractional_cycles: u16,
    /// True if we're in the current line's HSYNC, false otherwise
    in_hsync: bool,
    /// If we're in the current line's HSYNC then this is the time to the end of line. Otherwise
    /// it's the time until we reach the HSYNC.
    cycles_to_line_event: CycleCount,
    /// Number of the line currently being displayed (from 0 to `lines_per_field`)
    cur_line: u16,
    /// Offset of the currently displayed line in the VRAM (relative to `display_vram_y_start`)
    cur_line_vram_offset: u16,
    /// Actual Y coordinate in VRAM of the currently displayed line
    cur_line_vram_y: u16,
    /// This variable toggles at each new line.
    line_phase: bool,
    /// When interlaced this variable is true when we switch to the bottom field. When progressive
    /// this variable is always false.
    bottom_field: bool,
    /// When interlaced this variable is true when we send the bottom field to the TV
    read_bottom_field: bool,
    /// Total number of lines (including blanking) per field. If the display is progressive there's
    /// only one field per frame
    lines_per_field: u16,
    /// Set to `false` on the beginning of a new line and set to `true` when the line has been
    /// drawn
    frame_drawn: bool,
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
    /// Texture window settings
    tex_window: TextureWindow,
    /// Mask bit settings
    mask_settings: MaskSettings,
    /// True if the display is disabled,
    display_off: bool,
    /// Next word returned by the GPUREAD command
    read_word: u32,
}

impl Gpu {
    pub fn new(video_standard: VideoStandard) -> Gpu {
        let mut gpu = Gpu {
            state: State::Idle,
            rasterizer: rasterizer::start(),
            video_standard,
            display_mode: DisplayMode::new(),
            display_line_start: 0x10,
            display_line_end: 0x100,
            display_column_start: 0x200,
            display_column_end: 0xc00,
            display_active: false,
            display_vram_x_start: 0,
            display_vram_y_start: 0,
            draw_mode: DrawMode::new(0),
            dma_direction: DmaDirection::Off,
            command_fifo: fifo::CommandFifo::new(),
            draw_time_budget: 0,
            remaining_fractional_cycles: 0,
            // This is what Mednafen uses, I have no idea where that comes from. Surely having
            // extremely precise timings so early on doesn't matter? After all the BIOS will resync
            // on VSync anyway.
            cycles_to_line_event: 3412 - 200,
            in_hsync: false,
            cur_line: 0,
            cur_line_vram_offset: 0,
            cur_line_vram_y: 0,
            line_phase: true,
            bottom_field: false,
            read_bottom_field: false,
            lines_per_field: 0,
            frame_drawn: false,
            clip_x_min: 0,
            clip_y_min: 0,
            clip_x_max: 1,
            clip_y_max: 0,
            draw_offset_x: 0,
            draw_offset_y: 0,
            tex_window: TextureWindow::new(),
            mask_settings: MaskSettings::new(),
            display_off: true,
            read_word: 0,
        };

        gpu.refresh_lines_per_field();

        gpu
    }

    pub fn video_standard(&self) -> VideoStandard {
        self.video_standard
    }

    pub fn take_frame(&mut self) -> Option<Frame> {
        // If we were waiting for a VRAM read we must fetch it before attempting to recover a
        // frame, otherwise we'll receive the VRAM read from the rasterizer and think that it's a
        // frame...
        if let State::VRamLoad(None) = self.state {
            let frame = self.rasterizer.receive_vram_load();
            self.state = State::VRamLoad(Some((frame, 0)));
        }

        self.rasterizer.take_frame()
    }

    pub fn set_rasterizer_option(&mut self, opt: RasterizerOption) {
        self.rasterizer.set_option(opt)
    }

    /// Pop a command from the `command_fifo` and return it while also sending it to the rasterizer
    /// as a side effect.
    fn command_pop_to_rasterizer(&mut self) -> u32 {
        let v = self.command_fifo.pop();

        self.rasterizer.push_gp0(v);

        v
    }

    fn reset(&mut self) {
        self.display_line_start = 0x10;
        self.display_line_end = 0x100;
        self.display_column_start = 0x200;
        self.display_column_end = 0xc00;

        self.draw_mode = DrawMode::new(0);
        self.display_mode.set(0);
        self.dma_direction.set(0);

        self.clip_x_min = 0;
        self.clip_y_min = 0;
        self.clip_x_max = 1;
        self.clip_y_max = 0;
        self.draw_offset_x = 0;
        self.draw_offset_y = 0;
        self.tex_window.set(0);
        self.mask_settings.set(0);
        self.display_vram_x_start = 0;
        self.display_vram_y_start = 0;

        self.reset_command_fifo();
    }

    fn reset_command_fifo(&mut self) {
        self.command_fifo.clear();
        self.state = State::Idle;

        if self.draw_time_budget < 0 {
            self.draw_time_budget = 0;
        }
    }

    fn status(&self) -> u32 {
        let mut s = 0;

        s |= self.draw_mode.0 & 0x7ff;

        s |= (self.mask_settings.draw_with_mask_bit() as u32) << 11;
        s |= (self.mask_settings.check_mask_bit() as u32) << 12;

        s |= ((!self.bottom_field) as u32) << 13;

        // TODO: No$ says that bit 14 is `display_mode` bit 7, need to check. Mednafen doesn't set
        // it.

        s |= (self.draw_mode.texture_disable() as u32) << 15;

        s |= ((self.display_mode.0 >> 6) & 1) << 16;
        s |= (self.display_mode.0 & 0x3f) << 17;

        s |= (self.display_off as u32) << 23;
        // TODO: bit 24 - IRQ1 (*not* VSync)

        // XXX This in what mednafen does but it's probably far from accurate. No$ has a more
        // detailed description but I don't know how accurate it is.
        let dma_request = self.dma_direction == DmaDirection::CpuToGp0
            || self.dma_direction == DmaDirection::VRamToCpu;

        s |= (dma_request as u32) << 25;

        s |= (self.is_idle() as u32) << 26;

        if let State::VRamLoad(_) = self.state {
            s |= 1 << 27;
        }

        // TODO: can read bit
        s |= 0 << 27;
        s |= (self.dma_can_write() as u32) << 28;
        s |= (self.dma_direction as u32) << 29;

        let display_line_even_odd = u32::from(self.cur_line_vram_y & 1);
        s |= display_line_even_odd << 31;

        s
    }

    /// Returns true if we're ready to accept DMA commands
    pub fn dma_can_write(&self) -> bool {
        // TODO: return false if in PLINE
        if let State::InQuad(_) = self.state {
            return false;
        }

        if self.command_fifo.is_empty() {
            return true;
        }

        let next_command = self.next_command();

        if let State::VRamStore(_) = self.state {
            return false;
        }

        if let State::VRamLoad(_) = self.state {
            return false;
        }

        // XXX this is taken from mednafen but I don't quite understand why we check `fifo_len`
        // instead of `len` here. Don't we just want to check if the entire command has been
        // received?
        if self.command_fifo.len() >= next_command.fifo_len() {
            // XXX I don't understand why mednafen doesn't use the same condition as in
            // `try_write_command`. Surely it would make sense for the `dma_can_write` bit to
            // simply be a `is_fifo_not_full` bit?
            return false;
        }

        true
    }

    /// Computes the value of the status register's "idle" bit
    fn is_idle(&self) -> bool {
        // TODO: add "InCmd" when we implement it
        self.state.is_idle() && self.draw_time_budget >= 0 && self.command_fifo.is_empty()
    }

    /// Attempt to write `command` to the command FIFO, returns `true` if successful, `false` if
    /// the FIFO overflowed and the command was dropped
    fn try_write_command(&mut self, command: u32) -> bool {
        // This logic was taken from mednafen: normally we should have a `command_fifo` of the same
        // depth as the real PSX (0x10 entries) and reject the command if it's empty. The problem
        // is that this requires very accurate GPU pipeline emulation, if a game really pushes the
        // FIFO to the limit and we end up slightly slower than normal we'll drop some data that we
        // shouldn't. As a workaround we use a deeper FIFO and we allow one extra command in it.
        let cur_fifo_len = self.command_fifo.len();

        if cur_fifo_len >= PSX_COMMAND_FIFO_DEPTH {
            // We have more data than a real PSX would allow but if it fits with the next command
            // removed we allow it anyway
            let next_command = self.next_command();

            let fifo_max = PSX_COMMAND_FIFO_DEPTH + next_command.fifo_len();

            if cur_fifo_len >= fifo_max {
                // Nope, the FIFO is still too full, drop the command
                warn!("GPU FIFO full, dropping 0x{:x}", command);
                return false;
            }
        }

        self.command_fifo.push(command);
        true
    }

    /// Peek into the command FIFO and retrieve the next Command to be executed. It's a mistake to
    /// call this method if the FIFO is empty.
    fn next_command(&self) -> &'static Command {
        debug_assert!(!self.command_fifo.is_empty());

        let opcode = self.command_fifo.peek() >> 24;

        &commands::GP0_COMMANDS[opcode as usize]
    }

    fn add_draw_time(&mut self, elapsed_cpu_cycles: CycleCount) {
        // No idea what's the rationale behind this cycle twiddling, it's copied from mednafen
        self.draw_time_budget += elapsed_cpu_cycles << 1;

        if self.draw_time_budget > 256 {
            self.draw_time_budget = 256;
        }
    }

    /// Returns the number of GPU cycles elapsed while the CPU ran `cpu_cyles`. Any fractional
    /// leftover cycle will be stored in `remaining_fractional_cycles`
    fn tick(&mut self, cpu_cycles: CycleCount) -> CycleCount {
        let clock_ratio = match self.video_standard {
            VideoStandard::Ntsc => GPU_CYCLES_PER_CPU_CYCLES_NTSC,
            VideoStandard::Pal => GPU_CYCLES_PER_CPU_CYCLES_PAL,
        };

        let mut gpu_cycles = u64::from(self.remaining_fractional_cycles);
        gpu_cycles += (cpu_cycles as u64) * clock_ratio;

        // FRACTIONAL_FACTOR should be a power of two so that the following modulo/division
        // optimize as simple bitops
        self.remaining_fractional_cycles = (gpu_cycles % FRACTIONAL_FACTOR) as u16;

        (gpu_cycles / FRACTIONAL_FACTOR) as CycleCount
    }

    /// Returns the total length of a line (including horizontal blanking)
    fn line_length(&self) -> u16 {
        match self.display_mode.standard() {
            // I'm not really sure what justifies this `line_phase` business but that's what
            // mednafen does. Maybe the real value is close to 3412.5 and therefore we have close
            // to one full cycle added every other cycle?
            VideoStandard::Ntsc => 3412 + self.line_phase as u16,
            VideoStandard::Pal => 3405,
        }
    }

    /// Refresh the value of `lines_per_field` based on the display mode
    fn refresh_lines_per_field(&mut self) {
        self.lines_per_field = if self.display_mode.is_interlaced() {
            let l = match self.display_mode.standard() {
                VideoStandard::Ntsc => 263,
                VideoStandard::Pal => 313,
            };

            l - self.bottom_field as u16
        } else {
            match self.display_mode.standard() {
                VideoStandard::Ntsc => 263,
                VideoStandard::Pal => 314,
            }
        };
    }

    /// Called when we switch to a new line
    fn new_line(&mut self) {
        self.line_phase = !self.line_phase;

        self.cur_line = (self.cur_line + 1) % self.lines_per_field;
    }

    /// Called when we're about to finish the current field
    fn new_field(&mut self) {
        self.bottom_field = if self.display_mode.is_interlaced() {
            !self.bottom_field
        } else {
            false
        };
    }

    /// Consume the `draw_time_budget`
    fn draw_time(&mut self, time: CycleCount) {
        self.draw_time_budget -= time;
    }

    /// Return various GPU state information in the GPUREAD register
    fn gp1_get_info(&mut self, val: u32) {
        // XXX what happens if we're in the middle of a framebuffer read?
        let v = match val & 0xf {
            2 => self.tex_window.0,
            3 => {
                let top = self.clip_y_min as u32;
                let left = self.clip_x_min as u32;

                left | (top << 10)
            }
            4 => {
                let bottom = self.clip_y_max as u32;
                let right = (self.clip_x_max - 1) as u32;

                right | (bottom << 10)
            }
            5 => {
                let x = (self.draw_offset_x as u32) & 0x7ff;
                let y = (self.draw_offset_y as u32) & 0x7ff;

                x | (y << 11)
            }
            // GPU version. Seems to always be 2?
            7 => 2,
            _ => unimplemented!("Unsupported GP1 info command {:08x}", val),
        };

        self.read_word = v;
    }
}

pub fn run(psx: &mut Psx) {
    let elapsed = sync::resync(psx, GPUSYNC);

    psx.gpu.add_draw_time(elapsed);

    process_commands(psx);

    let mut elapsed_gpu_cycles = psx.gpu.tick(elapsed);

    timers::run_gpu_clocks(psx, elapsed_gpu_cycles);

    while elapsed_gpu_cycles >= psx.gpu.cycles_to_line_event {
        elapsed_gpu_cycles -= psx.gpu.cycles_to_line_event;

        // We either reached hsync or left it
        psx.gpu.in_hsync = !psx.gpu.in_hsync;

        timers::set_in_hsync(psx, psx.gpu.in_hsync);

        if psx.gpu.in_hsync {
            // We don't have anything special to do here when we reach the hsync, we only handle
            // this case specially to synchronize the timer code.
            psx.gpu.cycles_to_line_event = HSYNC_LEN_CYCLES;
        } else {
            // We reached the EOL
            handle_eol(psx);
        }
    }

    psx.gpu.cycles_to_line_event -= elapsed_gpu_cycles;

    // New we need to program the next sync at `cycles_to_line_event`. Where it gets tricky is that
    // we program sync events based on the CPU clock, so we need to do the conversion
    let mut delta = psx.gpu.cycles_to_line_event as u64 * FRACTIONAL_FACTOR;
    // Don't forget the fractional cycle we have leftover
    delta -= u64::from(psx.gpu.remaining_fractional_cycles);

    // Finally divide by the frequency factor, rounding *up* (we want to be called when the event
    // has occurred, not just before). Remember that in order to divide `x` by `y` rounding up you
    // need to do `(x + y - 1) / y`.
    let clock_ratio = match psx.gpu.video_standard {
        VideoStandard::Ntsc => GPU_CYCLES_PER_CPU_CYCLES_NTSC,
        VideoStandard::Pal => GPU_CYCLES_PER_CPU_CYCLES_PAL,
    };

    delta = (delta + clock_ratio - 1) / clock_ratio;

    if delta < 1 {
        delta = 1;
    } else if delta > 128 {
        // I believe that mednafen does that in order to trigger a call to `process_commands` very
        // often and keep the GPU working. We could relax this when we don't care for very accurate
        // timings
        delta = 128;
    }

    sync::next_event(psx, GPUSYNC, delta as CycleCount);
}

pub fn dma_can_write(psx: &mut Psx) -> bool {
    run(psx);
    psx.gpu.dma_can_write()
}

/// Called when we reach the end of line (just after the HSYNC)
fn handle_eol(psx: &mut Psx) {
    let mut eof = false;

    psx.gpu.rasterizer.end_of_line(psx.gpu.cur_line);

    psx.gpu.new_line();

    // Next line event will be when we reach the hsync
    psx.gpu.cycles_to_line_event = CycleCount::from(psx.gpu.line_length()) - HSYNC_LEN_CYCLES;
    let cur_line = psx.gpu.cur_line;

    // Taken from mednafen but I'm not sure if that's necessary or even useful. Normally we'll draw
    // the frame when we reach `display_line_end` below. If we miss that we have the code running
    // on the last field line below that'll always catch it. So what's the point of checking once
    // again here? Especially since at this point the line number seems fairly arbitrary. I guess
    // it can trigger early if `display_line_end` is set to some silly value but is it really worth
    // a special case?
    if !psx.gpu.frame_drawn {
        let draw_line = match psx.gpu.video_standard {
            VideoStandard::Ntsc => 256,
            VideoStandard::Pal => 308,
        };

        if cur_line == draw_line {
            // We reached the end of active video, we can tell the frontend to render the frame
            eof = true;
        }
    }

    let is_first_line = cur_line == 0;
    let is_last_line = cur_line == psx.gpu.lines_per_field - 1;

    if is_last_line {
        if !psx.gpu.frame_drawn {
            // Normally we should've drawn the frame by now but we might have missed it if the
            // video standard changed mid-frame. In this case we can just draw it now in order not
            // to skip the frame entirely and it'll return to normal next frame
            eof = true;
        }

        // I'm not sure why Mednafen changes the field on the last line of the field instead of the
        // first line of the next one.
        psx.gpu.new_field();
    } else if is_first_line {
        debug_assert!(psx.gpu.frame_drawn, "Last frame wasn't drawn!");

        psx.gpu.frame_drawn = false;
        psx.gpu.refresh_lines_per_field();
    }

    if cur_line == psx.gpu.display_line_end && psx.gpu.display_active {
        // We're leaving the active display area.
        psx.gpu.display_active = false;
        psx.gpu.cur_line_vram_offset = 0;

        irq::set_high(psx, irq::Interrupt::VBlank);
        timers::set_in_vsync(psx, true);

        if psx.gpu.display_mode.is_true_interlaced() {
            // Prepare for the next frame, if we're currently sending the bottom field it means
            // that we're going to switch to the top
            psx.gpu.read_bottom_field = !psx.gpu.bottom_field;
            psx.gpu.rasterizer.field_change(psx.gpu.read_bottom_field);
        } else if psx.gpu.read_bottom_field {
            psx.gpu.read_bottom_field = false;
            psx.gpu.rasterizer.field_change(psx.gpu.read_bottom_field);
        }

        if !psx.gpu.frame_drawn {
            // More magic from mednafen. I assume that the logic is to try to refresh the display
            // as early as possible to minimize latency, although I'm not really sure I understand
            // the logic behind putting a minimum line value here. The comment in mednafen mentions
            // Descent(NTSC) which reaches the end of frame at line 236 and Mikagura Shoujo
            // Tanteidan which sets it to 192 during the intro FMV.
            let line_min = match psx.gpu.video_standard {
                VideoStandard::Ntsc => 232,
                VideoStandard::Pal => 260,
            };

            if cur_line >= line_min {
                eof = true;
            }
        }
    }

    if cur_line == psx.gpu.display_line_start && !psx.gpu.display_active {
        // We're entering the active display area
        psx.gpu.display_active = true;
        irq::set_low(psx, irq::Interrupt::VBlank);
        timers::set_in_vsync(psx, false);
    }

    // Figure out which VRAM line is being displayed
    psx.gpu.cur_line_vram_y = psx.gpu.display_vram_y_start;
    psx.gpu.cur_line_vram_y += if psx.gpu.display_mode.is_true_interlaced() {
        let field_off = if psx.gpu.display_active {
            psx.gpu.read_bottom_field as u16
        } else {
            0
        };

        (psx.gpu.cur_line_vram_offset << 1) | field_off
    } else {
        psx.gpu.cur_line_vram_offset
    };
    psx.gpu.cur_line_vram_y %= VRAM_HEIGHT;

    if eof {
        draw_frame(psx);
    }

    if psx.gpu.display_active {
        psx.gpu.cur_line_vram_offset += 1;
    }
}

/// Called when a frame is done rendering and should be displayed
fn draw_frame(psx: &mut Psx) {
    psx.gpu.rasterizer.end_of_frame();
    psx.gpu.frame_drawn = true;
    psx.frame_done = true;
}

pub fn store<T: Addressable>(psx: &mut Psx, off: u32, val: T) {
    if T::width() != AccessWidth::Word {
        panic!("Unhandled GPU store ({:?})", T::width());
    }

    let val = val.as_u32();

    match off {
        0 => gp0(psx, val),
        4 => gp1(psx, val),
        _ => unreachable!(),
    };
}

pub fn load<T: Addressable>(psx: &mut Psx, off: u32) -> T {
    if T::width() != AccessWidth::Word {
        panic!("Unhandled GPU load ({:?})", T::width());
    }

    let v = match off {
        0 => read(psx),
        4 => psx.gpu.status(),
        _ => unreachable!(),
    };

    T::from_u32(v)
}

pub fn dma_store(psx: &mut Psx, val: u32) {
    gp0(psx, val);
}

pub fn dma_load(psx: &mut Psx) -> u32 {
    read(psx)
}

/// Handles loads from GP0
fn read(psx: &mut Psx) -> u32 {
    match psx.gpu.state {
        State::VRamLoad(ref mut vram_load) => {
            let (frame, pos) = match vram_load {
                None => {
                    // We haven't received the frame from the renderer yet, let's do that now
                    let frame = psx.gpu.rasterizer.receive_vram_load();
                    *vram_load = Some((frame, 0));
                    vram_load.as_mut().unwrap()
                }
                Some(ref mut s) => s,
            };

            // If we reach this point we should have at least one pixel left.
            //
            // XXX Watch out: frame pixels are u32 but in this case they're really u16 and the top
            // 16bits is unused. I'm just being lazy and reuse the same structure for frame draws
            // and VRAM reads.
            let p1 = frame.pixels[*pos as usize];
            *pos += 1;

            let to_read = frame.pixels.len() - *pos as usize;

            let p2 = if to_read > 1 {
                let p2 = frame.pixels[*pos as usize];
                *pos += 1;
                p2
            } else if to_read == 1 {
                // Last pixel
                let p2 = frame.pixels[*pos as usize];
                psx.gpu.state = State::Idle;
                p2
            } else {
                // No more pixels
                psx.gpu.state = State::Idle;
                0
            };

            p1 | (p2 << 16)
        }
        _ => {
            // XXX I'm not really sure about this one. Is the read_word normally pushed in the
            // FIFO?  What happens if you send a GP1[0x10] and then immediately attempt to read the
            // framebuffer? Needs more testing
            psx.gpu.read_word
        }
    }
}

/// Handle GP0 commands
fn gp0(psx: &mut Psx, val: u32) {
    if psx.gpu.try_write_command(val) {
        process_commands(psx);
    }
}

/// Handle GP1 commands
fn gp1(psx: &mut Psx, val: u32) {
    psx.gpu.rasterizer.push_gp1(val);

    let op = val >> 24;

    match op {
        0x00 => psx.gpu.reset(),
        0x01 => psx.gpu.reset_command_fifo(),
        // IRQ1 ack
        0x02 => (),
        0x03 => psx.gpu.display_off = (val & 1) != 0,
        0x04 => psx.gpu.dma_direction.set(val & 3),
        0x05 => {
            // XXX from mednafen: LSB ignored.
            psx.gpu.display_vram_x_start = (val & 0x3fe) as u16;
            psx.gpu.display_vram_y_start = ((val >> 10) & 0x1ff) as u16;
        }
        0x06 => {
            psx.gpu.display_column_start = (val & 0xfff) as u16;
            psx.gpu.display_column_end = ((val >> 12) & 0xfff) as u16;
        }
        0x07 => {
            psx.gpu.display_line_start = (val & 0x3ff) as u16;
            psx.gpu.display_line_end = ((val >> 10) & 0x3ff) as u16;
        }
        0x08 => psx.gpu.display_mode.set(val & 0xff_ffff),
        0x10 => psx.gpu.gp1_get_info(val),
        _ => unimplemented!("GP1 0x{:08x}", val),
    }
}

/// Attempt to execute a command from the `command_fifo`
fn process_commands(psx: &mut Psx) {
    let pending_commands = !psx.gpu.command_fifo.is_empty();

    match psx.gpu.state {
        State::Idle => run_next_command(psx),
        State::InQuad(draw_time) => {
            if psx.gpu.draw_time_budget >= 0 {
                // We have finished drawing the first triangle in the quad, we can move to the 2nd
                // one
                psx.gpu.draw_time(draw_time);
                psx.gpu.state = State::Idle;
            }
        }
        State::PolyLine(opcode, _) => {
            if psx.gpu.draw_time_budget >= 0 && pending_commands {
                let next = psx.gpu.command_fifo.peek();
                if next & 0xf000_f000 == 0x5000_5000 {
                    // End-of-line marker
                    psx.gpu.command_pop_to_rasterizer();
                    psx.gpu.state = State::Idle;
                } else {
                    // Shaded polylines require 2 words per segment, solid polylines only 1
                    let is_shaded = (opcode & 0x10) != 0;
                    let words_per_segment = 1 + (is_shaded as usize);

                    if psx.gpu.command_fifo.len() >= words_per_segment {
                        // We can draw the next segment
                        let cmd = &commands::GP0_COMMANDS[opcode as usize];
                        (cmd.handler)(psx);
                    }
                }
            }
        }
        State::VRamStore(ref mut nwords) => {
            if pending_commands {
                let v = psx.gpu.command_fifo.pop();
                psx.gpu.rasterizer.push_gp0(v);
                *nwords -= 1;

                if *nwords == 0 {
                    psx.gpu.state = State::Idle;
                }
            }
        }
        // We don't process commands in VRAM Load mode
        // XXX validate on real hardware
        State::VRamLoad(_) => (),
    }
}

fn run_next_command(psx: &mut Psx) {
    if psx.gpu.command_fifo.is_empty() {
        // We have nothing to do if the FIFO is empty
        return;
    }

    let command = psx.gpu.next_command();

    if command.len() > psx.gpu.command_fifo.len() {
        // We still haven't received the entire command, wait longer
        return;
    }

    // Apparently there are a handful of commands that aren't executed like the rest. Both mednafen
    // and No$ agree on that, however the specifics are unclear. I tag these commands as "out of
    // band" here (mednafen calls them "ss_cmd", not sure what that means).
    //
    // No$ says that these commands "do not take up space in the FIFO" and are "probably executed
    // immediately (even if there're still other commands in the FIFO)". If that's true it means
    // that we should run these commands directly from `gp0` without touching the FIFO.
    //
    // Mednafen on the other hands puts these commands through the FIFO as usual and executes them
    // in order, just with no draw time overhead.
    if psx.gpu.draw_time_budget < 0 && !command.out_of_band {
        // We don't have enough time budget to execute this command
        return;
    }

    if !command.out_of_band {
        psx.gpu.draw_time(2);
    }

    // Invoke the callback to actually implement the command
    (command.handler)(psx);
}

#[derive(serde::Serialize, serde::Deserialize)]
enum State {
    Idle,
    /// We're drawing the first triangle of a quad. The CycleCount is the number of cycles we'll
    /// have to use to draw the 2nd triangle.
    InQuad(CycleCount),
    /// We're in the middle of a polyline. The u8 is the opcode for this line, then we store the
    /// position of the end of the last drawn segment (i.e. the start of the next segment)
    PolyLine(u8, Position),
    /// We're uploading data to the VRAM. The u32 is the number of 32bit words left to transfer.
    VRamStore(u32),
    /// We're downloading data from the VRAM. Since this comes from the rasterizer we set the
    /// Option to `None` while we're waiting for the rasterizer to send us the data, then we store
    /// the received frame when we receive it, alongside a counter that tells us how many bytes
    /// we've left to read
    VRamLoad(Option<(Frame, u32)>),
}

impl State {
    fn is_idle(&self) -> bool {
        matches!(self, State::Idle)
    }
}

/// Wrapper around the Draw Mode register value (set by GP0[0xe1] and polygon draw commands)
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct DrawMode(u32);

impl DrawMode {
    fn new(mode: u32) -> DrawMode {
        DrawMode(mode)
    }

    /// Update from a polygon draw command. When that happens it overwrites the previous value
    /// globally (i.e. the configuration remains set for any subsequent draw commands, not just the
    /// current polygon)
    fn update_from_poly(&mut self, poly_cmd: u32) {
        // XXX bit 11 (texture_disable) can also be set/cleared, but only if the functionality is
        // enabled using GP1[0x09]
        self.0 &= !0x9ff;
        self.0 |= (poly_cmd >> 16) & 0x9ff;
    }

    fn texture_disable(self) -> bool {
        self.0 & (1 << 11) != 0
    }

    /// Coordinate of the left side of the texture page in VRAM
    fn texture_page_x(self) -> u16 {
        let x = (self.0 & 0xf) as u16;

        x << 6
    }

    /// Coordinate of the top side of the texture page in VRAM
    fn texture_page_y(self) -> u16 {
        let y = ((self.0 >> 4) & 1) as u16;

        y << 8
    }

    fn texture_depth(self) -> u8 {
        ((self.0 >> 7) & 3) as u8
    }

    fn pixel_to_texel_shift(self) -> u8 {
        match self.texture_depth() {
            0 => 2, // Paletted 4bpp
            1 => 1, // Paletted 8bpp
            2 => 0, // True Color 1555BGR, 16bits per pixel
            _ => 0, // XXX double-check if 3 is also truecolor.
        }
    }

    fn transparency_mode(self) -> TransparencyFunction {
        match (self.0 >> 5) & 3 {
            0 => TransparencyFunction::Average,
            1 => TransparencyFunction::Add,
            2 => TransparencyFunction::Sub,
            3 => TransparencyFunction::QuarterAdd,
            _ => unreachable!(),
        }
    }

    /// If true rectangle textures should be flipped horizontally
    fn flip_rect_x(self) -> bool {
        self.0 & (1 << 12) != 0
    }

    /// If true rectangle textures should be flipped vertically
    fn flip_rect_y(self) -> bool {
        self.0 & (1 << 13) != 0
    }

    /// Return true if dithering is enabled
    fn dither_enable(self) -> bool {
        self.0 & (1 << 9) != 0
    }

    /// True if the GPU is allowed to draw to the display area
    fn draw_to_display_area(self) -> bool {
        self.0 & (1 << 10) != 0
    }
}

/// The various transparency modes
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum TransparencyFunction {
    /// (Background + Foreground) / 2
    Average,
    /// Background + Foreground
    Add,
    /// Background - Foreground
    Sub,
    /// Background + (Foreground / 4)
    QuarterAdd,
}

/// Wrapper around the Texture Window register value (set by GP0[0xe2])
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct TextureWindow(u32);

impl TextureWindow {
    fn new() -> TextureWindow {
        TextureWindow(0)
    }

    fn set(&mut self, tw: u32) {
        self.0 = tw;
    }

    /// Mask to be ANDed to U coordinates
    fn u_mask(self) -> u8 {
        let m = (self.0 & 0x1f) as u8;

        // 8 pixel steps
        !(m << 3)
    }

    /// Mask to be ANDed to V coordinates
    fn v_mask(self) -> u8 {
        let m = ((self.0 >> 5) & 0x1f) as u8;

        // 8 pixel steps
        !(m << 3)
    }

    /// Offset to be added to U coordinates after applying `u_mask`
    fn u_offset(self) -> u8 {
        let off = ((self.0 >> 10) & 0x1f) as u8;

        (off << 3) & !self.u_mask()
    }

    /// Offset to be added to V coordinates after applying `v_mask`
    fn v_offset(self) -> u8 {
        let off = ((self.0 >> 15) & 0x1f) as u8;

        (off << 3) & !self.v_mask()
    }
}

/// Wrapper around the Display Mode register value (set by GP1[0x08])
#[derive(serde::Serialize, serde::Deserialize, Copy, Clone)]
struct DisplayMode(u32);

impl DisplayMode {
    fn new() -> DisplayMode {
        DisplayMode(0)
    }

    fn set(&mut self, mode: u32) {
        self.0 = mode
    }

    fn standard(self) -> VideoStandard {
        if self.0 & (1 << 3) != 0 {
            VideoStandard::Pal
        } else {
            VideoStandard::Ntsc
        }
    }

    fn is_interlaced(self) -> bool {
        self.0 & (1 << 5) != 0
    }

    /// To actually have the console output in interlaced (having two interlaced fields in VRAM and
    /// the console displays one after the other) it's not enough to set the `is_interlaced` bit,
    /// you also need to set bit 2 in Display Mode to actually tell the console to use two fields
    /// in VRAM. Without it the console sends the same data for the top and bottom fields, which is
    /// fairly useless.
    fn is_true_interlaced(self) -> bool {
        let two_fields = self.0 & (1 << 2) != 0;

        self.is_interlaced() && two_fields
    }

    /// Retrieve the approximate horizontal resolution of the active video. This is an
    /// approximation because it will also depend on the timing configuration of the output (column
    /// start/column end etc...).
    fn xres(self) -> u16 {
        if (self.0 & (1 << 6)) != 0 {
            368
        } else {
            match self.0 & 3 {
                0 => 256,
                1 => 320,
                2 => 512,
                3 => 640,
                _ => unreachable!(),
            }
        }
    }

    /// True if we output 24 bits per pixel
    fn output_24bpp(self) -> bool {
        self.0 & (1 << 4) != 0
    }
}

/// Wrapper around the Mask Setting register value (set by GP0[0xe6])
#[derive(serde::Serialize, serde::Deserialize)]
struct MaskSettings {
    /// Raw register value
    raw: u32,
    /// Pixel value that should be ORed on write
    or_mask: Pixel,
}

impl MaskSettings {
    fn new() -> MaskSettings {
        MaskSettings {
            raw: 0,
            or_mask: Pixel::from_mbgr1555(0),
        }
    }

    fn set(&mut self, v: u32) {
        self.raw = v & 3;

        let p = if self.draw_with_mask_bit() { 0x8000 } else { 0 };

        self.or_mask = Pixel::from_mbgr1555(p);
    }

    fn draw_with_mask_bit(&self) -> bool {
        self.raw & 1 != 0
    }

    fn check_mask_bit(&self) -> bool {
        self.raw & (1 << 1) != 0
    }

    fn can_draw_to(&self, p: Pixel) -> bool {
        if self.check_mask_bit() {
            !p.mask()
        } else {
            true
        }
    }

    fn mask(&self, mut p: Pixel) -> Pixel {
        p.0 |= self.or_mask.0;

        p
    }
}

/// Requested DMA direction.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq)]
enum DmaDirection {
    Off = 0,
    Fifo = 1,
    CpuToGp0 = 2,
    VRamToCpu = 3,
}

impl DmaDirection {
    fn set(&mut self, v: u32) {
        *self = match v & 3 {
            0 => DmaDirection::Off,
            1 => DmaDirection::Fifo,
            2 => DmaDirection::CpuToGp0,
            3 => DmaDirection::VRamToCpu,
            _ => unreachable!(),
        }
    }
}

/// Real PSX command FIFO depth
const PSX_COMMAND_FIFO_DEPTH: usize = 0x10;

/// Emulated command FIFO depth. This is different from `PSX_COMMAND_FIFO_DEPTH` to work around
/// emulation inaccuracies, see `try_write_command` above for more infos. Needs to be a power of
/// two for the FIFO code to work correctly.
const COMMAND_FIFO_DEPTH: usize = 0x20;

/// The are a few hardware differences between PAL and NTSC consoles, in particular the pixelclock
/// runs slightly slower on PAL consoles.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
pub enum VideoStandard {
    Ntsc,
    Pal,
}

/// Total number of lines in the VRAM
const VRAM_HEIGHT: u16 = 512;

/// Duration of the HSYNC in GPU cycles
const HSYNC_LEN_CYCLES: CycleCount = 200;

/// Scaling factor used in fixed point GPU cycle arithmetics
const FRACTIONAL_FACTOR: u64 = 1 << 16;

/// Ratio of GPU_FREQ_NTSC_HZ / CPU_FREQ_HZ multiplied by 0x1000 to use fixed point arithmetics
/// instead of floating point
const GPU_CYCLES_PER_CPU_CYCLES_NTSC: u64 =
    (GPU_FREQ_NTSC_HZ * (FRACTIONAL_FACTOR as f64) / (CPU_FREQ_HZ as f64)) as u64;

/// Ratio of GPU_FREQ_PAL_HZ / CPU_FREQ_HZ multiplied by 0x1000 to use fixed point arithmetics
/// instead of floating point
const GPU_CYCLES_PER_CPU_CYCLES_PAL: u64 =
    (GPU_FREQ_PAL_HZ * (FRACTIONAL_FACTOR as f64) / (CPU_FREQ_HZ as f64)) as u64;

/// GPU frequency for NTSC consoles (Japan + North America)
const GPU_FREQ_NTSC_HZ: f64 = 53_693_181.818;
/// GPU frequency for PAL consoles (Europe)
const GPU_FREQ_PAL_HZ: f64 = 53_203_425.;
