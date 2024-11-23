//! Implementation of the various GP0 commands.

use super::{CycleCount, DrawMode, Psx, State};
use std::cmp::max;

/// Description of the various GP0 commands
pub struct Command {
    /// Callback function to actually perform the command. If the command is a draw command it only
    /// takes care of the timings and maintaining the façade, the drawing takes place in an other
    /// thread.
    pub handler: fn(&mut Psx),
    /// Actual length of the command, in number of FIFO words
    pub len: u8,
    /// This is the amount of overhead bytes accepted on the FIFO when this command is the next to
    /// be executed (see `try_write_command` for more details). These values are taken from
    /// mednafen, I'm not sure how they've been established. I assume it has something to do with
    /// the amount of time taken by these instructions to process?
    pub fifo_len: u8,
    /// Marker for commands that aren't executed with normal PSX timings.
    pub out_of_band: bool,
}

impl Command {
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn fifo_len(&self) -> usize {
        self.fifo_len as usize
    }
}

pub trait TransparencyMode {
    fn is_transparent() -> bool;
}

pub struct Transparent;

impl TransparencyMode for Transparent {
    fn is_transparent() -> bool {
        true
    }
}

pub struct Opaque;

impl TransparencyMode for Opaque {
    fn is_transparent() -> bool {
        false
    }
}

pub trait TextureMode {
    fn is_textured() -> bool;
    fn is_raw_texture() -> bool;
}

pub struct NoTexture;

impl TextureMode for NoTexture {
    fn is_textured() -> bool {
        false
    }

    fn is_raw_texture() -> bool {
        false
    }
}

pub struct TextureBlending;

impl TextureMode for TextureBlending {
    fn is_textured() -> bool {
        true
    }

    fn is_raw_texture() -> bool {
        false
    }
}

pub struct TextureRaw;

impl TextureMode for TextureRaw {
    fn is_textured() -> bool {
        true
    }

    fn is_raw_texture() -> bool {
        true
    }
}

pub trait ShadingMode {
    fn is_shaded() -> bool;
}

pub struct NoShading;

impl ShadingMode for NoShading {
    fn is_shaded() -> bool {
        false
    }
}

pub struct Shaded;

impl ShadingMode for Shaded {
    fn is_shaded() -> bool {
        true
    }
}

/// A vertex's coordinates
#[derive(serde::Serialize, serde::Deserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

impl Position {
    pub fn new(x: i32, y: i32) -> Position {
        Position { x, y }
    }

    pub fn from_command(c: u32) -> Position {
        let x = c;
        let y = c >> 16;

        // XXX The coordinates each take 16bits and are signed, however mednafen extends the sign
        // starting on bit 11, not 15.
        let x = extend_to_i32(x, 11);
        let y = extend_to_i32(y, 11);
        Position::new(x, y)
    }
}

/// Extend a signed value on `n` bit to an i32
fn extend_to_i32(val: u32, n: usize) -> i32 {
    let shift = 32 - n;

    ((val << shift) as i32) >> shift
}

/// Compute the cross-product of (AB) x (AC)
fn cross_product(a: Position, b: Position, c: Position) -> i32 {
    (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)
}

/// Triangles taller than this value are rejected by the PSX GPU
const TRIANGLE_MAX_HEIGHT: i32 = 511;
/// Triangles wider than this value are rejected by the PSX GPU
const TRIANGLE_MAX_WIDTH: i32 = 1023;

/// Computes triangle-drawing timings
fn triangle_draw_time<Transparency, Texture, Shading>(
    psx: &mut Psx,
    mut coords: [Position; 3],
) -> CycleCount
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
    Shading: ShadingMode,
{
    let mut draw_time = 0;

    // First compute the constant draw time for this command
    draw_time += if Shading::is_shaded() {
        if Texture::is_textured() {
            // Shaded and textured
            150 * 3
        } else {
            // Shaded
            96 * 3
        }
    } else if Texture::is_textured() {
        // Textured
        60 * 3
    } else {
        0
    };

    // Order coords by x coordinates
    coords.sort_by(|a, b| a.x.cmp(&b.x));

    let min_x = coords[0].x;
    let max_x = coords[2].x;

    // This is the triangle's width on screen, in pixels (not accounting for any clipping). This is
    // *not* the geometric base width of the triangle.
    let triangle_width = max_x - min_x;

    if triangle_width == 0 || triangle_width > TRIANGLE_MAX_WIDTH {
        return draw_time;
    }

    // Order coords by y coordinates
    coords.sort_by(|a, b| a.y.cmp(&b.y));

    let min_y = coords[0].y;
    let max_y = coords[2].y;

    // This is the triangle's height on screen, in pixels (not accounting for any clipping). This
    // is *not* the geometric height of the triangle.
    let triangle_height = max_y - min_y;

    if triangle_height == 0 || triangle_height > TRIANGLE_MAX_HEIGHT {
        return draw_time;
    }

    // Compute the cross product
    let cross = cross_product(coords[0], coords[1], coords[2]);

    // If the cross-product is 0 it means that the 3 vertices lie on a line and the triangle is
    // completely flat, so we have nothing to draw.
    if cross == 0 {
        return draw_time;
    }

    // The area of the triangle is half the magnitude of the cross-product.
    let triangle_area = cross.abs() / 2;

    // True if the triangle clips with at least one side of the drawing area
    let clip = min_x < psx.gpu.clip_x_min
        || max_x > psx.gpu.clip_x_max
        || min_y < psx.gpu.clip_y_min
        || max_y > psx.gpu.clip_y_max;

    let npixels = if clip {
        // If the triangle clips the draw area we can't use its area to guess the number of pixels
        // for this polygon since parts of it won't be drawn. Figuring out the exact area of the
        // displayed part quickly is not trivial (or at least, I can't think of a trivial way).
        // There are many possible scenarios to consider.
        //
        // For the time being I'm going to use a very inaccurate first approach: I consider that
        // we can approximate the ratio of the total area of the triangle over the non-clipped area
        // by using the ratio of the total area of the triangle's bounding box over the clipped
        // bounding box.
        //
        // This is of course very inaccurate in the general case, however it does have the
        // interesting property that it gives the correct result for rectangular quads (since
        // obviously in this case the bounding box of the quad *is* the quad itself). Overall I
        // hope that the proportion of clipped triangles will be low enough to make the error in
        // the estimation negligible.
        let bb_total_width = triangle_width;
        let bb_total_height = triangle_height;

        let mut bb_clip_width = bb_total_width;
        let mut bb_clip_height = bb_total_height;

        // Clip the bounding box
        if min_x < psx.gpu.clip_x_min {
            bb_clip_width -= psx.gpu.clip_x_min - min_x;
        }
        if max_x > psx.gpu.clip_x_max {
            bb_clip_width -= max_x - psx.gpu.clip_x_max;
        }
        if min_y < psx.gpu.clip_y_min {
            bb_clip_height -= psx.gpu.clip_y_min - min_y;
        }
        if max_y > psx.gpu.clip_y_max {
            bb_clip_height -= max_y - psx.gpu.clip_y_max;
        }

        if bb_clip_width <= 0 || bb_clip_height <= 0 {
            // The bounding box is entirely outside of the clip area, the pixel won't be drawn at
            // all
            //
            // XXX Is it really rejected instantly or does the GPU still iterate over every line,
            // drawing nothing?
            return draw_time;
        }

        // Use 64bits to avoid overflows
        let bb_total_area = i64::from(bb_total_width * bb_total_height);
        let bb_clip_area = i64::from(bb_clip_width * bb_clip_height);

        let clip_area_approx = (i64::from(triangle_area) * bb_clip_area) / bb_total_area;

        clip_area_approx as i32
    } else {
        // If the triangle doesn't clip the draw area we can consider that, typically, the
        // number of pixels is roughly the area of the triangle. It's not a perfect match
        // because a rasterized triangle will have at least one "ragged" edge with bits of
        // pixels poking out or in. We can hope that the discrepancies will even out over the
        // course of a frame.
        //
        // In particular for any quad in the shape of a parallelogram the symmetry of the two
        // contained triangles should make that the sum of both areas should be very close to
        // the sum of the pixels, with one half being under-evaluated and the other
        // over-evaluated.
        triangle_area
    };

    draw_time += if Texture::is_textured() || Shading::is_shaded() {
        // For texture and shading we have a base cost of about 2 cycles per pixel (like for
        // shading) *but* we also have the additional costs of the texture fetch which depends on
        // the GPU cache state.
        //
        // XXX Can we estimate the texture cache performance without emulating every pixel draw?
        // Maybe some heuristic keeping track of which texture has been used last to see if cache
        // hits are likely?
        npixels * 2
    } else if Transparency::is_transparent() || psx.gpu.mask_settings.check_mask_bit() {
        // If we need to read the VRAM for transparency or mask checking we use about 1.5 cycles
        // per pixel
        (npixels * 3) / 2
    } else {
        // For plain solid triangles we have about 1 cycle per pixel
        npixels
    };

    // XXX Handle !draw_to_display_area for non-interlaced output
    if !psx.gpu.draw_mode.draw_to_display_area() && psx.gpu.display_mode.is_true_interlaced() {
        // We'll only draw every other line
        draw_time /= 2;
    }

    draw_time
}

fn cmd_handle_poly_tri<Transparency, Texture, Shading>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
    Shading: ShadingMode,
{
    let mut coords = [
        Position::new(0, 0),
        Position::new(0, 0),
        Position::new(0, 0),
    ];

    // Load the triangle coordinates. Since we only care about timings here we don't need to load
    // the texture coordinates or anything.
    for (v, coord) in coords.iter_mut().enumerate() {
        if v == 0 || Shading::is_shaded() {
            // Pop the shading color (also takes care of the command word if we're not shaded)
            psx.gpu.command_pop_to_rasterizer();
        }

        let cmd = psx.gpu.command_pop_to_rasterizer();
        *coord = Position::from_command(cmd);

        // Add the draw offset
        coord.x += psx.gpu.draw_offset_x;
        coord.y += psx.gpu.draw_offset_y;

        if Texture::is_textured() {
            // Pop texture coordinates
            let tex = psx.gpu.command_pop_to_rasterizer();
            if v == 1 {
                // For polygons the 2nd texel contains the new texture page config
                psx.gpu.draw_mode.update_from_poly(tex);
            }
        }
    }

    // Compute the time needed to draw the triangle
    let draw_time = triangle_draw_time::<Transparency, Texture, Shading>(psx, coords);
    psx.gpu.draw_time(64 + 18 + draw_time);
}

fn cmd_handle_poly_quad<Transparency, Texture, Shading>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
    Shading: ShadingMode,
{
    // Quads are effectively just an optimization to reduce the length of the draw command,
    // internally they're just drawn as two triangles.

    let mut coords = [
        Position::new(0, 0),
        Position::new(0, 0),
        Position::new(0, 0),
        Position::new(0, 0),
    ];

    // Load the triangle coordinates. Since we only care about timings here we don't need to load
    // the texture coordinates or anything.
    //
    // XXX For now I read all for vertices at once to make the code simpler but I'm fairly sure
    // that it's inaccurate, the GPU probably only reads the last vertex once the first triangle
    // has been drawn.
    for (v, coord) in coords.iter_mut().enumerate() {
        if v == 0 || Shading::is_shaded() {
            // Pop the shading color (also takes care of the command word if we're not shaded)
            psx.gpu.command_pop_to_rasterizer();
        }

        let cmd = psx.gpu.command_pop_to_rasterizer();
        *coord = Position::from_command(cmd);

        // Add the draw offset
        coord.x += psx.gpu.draw_offset_x;
        coord.y += psx.gpu.draw_offset_y;

        if Texture::is_textured() {
            // Pop texture coordinates
            let tex = psx.gpu.command_pop_to_rasterizer();
            if v == 1 {
                // For polygons the 2nd texel contains the new texture page config
                psx.gpu.draw_mode.update_from_poly(tex);
            }
        }
    }

    let triangle_1 = [coords[0], coords[1], coords[2]];
    let triangle_2 = [coords[1], coords[2], coords[3]];

    // Compute the time needed to draw the first triangle
    let draw_time = triangle_draw_time::<Transparency, Texture, Shading>(psx, triangle_1);
    psx.gpu.draw_time(64 + 18 + draw_time);

    // Compute the time needed to draw the second triangle and save it for later
    let draw_time = triangle_draw_time::<Transparency, Texture, Shading>(psx, triangle_2);

    psx.gpu.state = State::InQuad(28 + 18 + draw_time);
}

fn rect_draw_time<Transparency, Texture>(
    psx: &mut Psx,
    dimensions: Option<(i32, i32)>,
) -> CycleCount
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    // Pop the command/color
    psx.gpu.command_pop_to_rasterizer();

    let mut top_left_corner = Position::from_command(psx.gpu.command_pop_to_rasterizer());
    // Add the draw offset
    top_left_corner.x += psx.gpu.draw_offset_x;
    top_left_corner.y += psx.gpu.draw_offset_y;

    if Texture::is_textured() {
        // Pop texture coordinates
        psx.gpu.command_pop_to_rasterizer();
    }

    let (mut width, mut height) = match dimensions {
        Some(d) => d,
        None => {
            // Variable dimensions
            let dim = psx.gpu.command_pop_to_rasterizer();
            let w = dim & 0x3ff;
            let h = (dim >> 16) & 0x1ff;

            (w as i32, h as i32)
        }
    };

    let min_x = top_left_corner.x;
    let min_y = top_left_corner.y;
    let max_x = top_left_corner.x + width;
    let max_y = top_left_corner.y + height;

    if min_x < psx.gpu.clip_x_min {
        width -= psx.gpu.clip_x_min - min_x;
    }
    if max_x > psx.gpu.clip_x_max {
        width -= max_x - psx.gpu.clip_x_max;
    }
    if min_y < psx.gpu.clip_y_min {
        height -= psx.gpu.clip_y_min - min_y;
    }
    if max_y > psx.gpu.clip_y_max {
        height -= max_y - psx.gpu.clip_y_max;
    }

    if width <= 0 || height <= 0 {
        // XXX are 0-size rects really rejected immediately even if one dimension in non-0?
        return 0;
    }

    let mut draw_time = width * height;

    if Transparency::is_transparent() || psx.gpu.mask_settings.check_mask_bit() {
        // If we need to read the VRAM for transparency or mask checking we use about 1.5 cycles
        // per pixel.
        // XXX here mednafen rounds up the dimensions to the next multiple of two, but not for the
        // "base" time above. Not sure what's up with that.
        draw_time = (draw_time * 3) / 2
    };

    // XXX Handle !draw_to_display_area for non-interlaced output
    if !psx.gpu.draw_mode.draw_to_display_area() && psx.gpu.display_mode.is_true_interlaced() {
        // We'll only draw every other line
        draw_time /= 2;
    }

    draw_time
}

fn cmd_handle_rect_variable<Transparency, Texture>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    let draw_time = rect_draw_time::<Transparency, Texture>(psx, None);
    psx.gpu.draw_time(draw_time);
}

fn cmd_handle_rect_1x1<Transparency, Texture>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    let draw_time = rect_draw_time::<Transparency, Texture>(psx, Some((1, 1)));
    psx.gpu.draw_time(draw_time);
}

fn cmd_handle_rect_8x8<Transparency, Texture>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    let draw_time = rect_draw_time::<Transparency, Texture>(psx, Some((8, 8)));
    psx.gpu.draw_time(draw_time);
}

fn cmd_handle_rect_16x16<Transparency, Texture>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Texture: TextureMode,
{
    let draw_time = rect_draw_time::<Transparency, Texture>(psx, Some((16, 16)));
    psx.gpu.draw_time(draw_time);
}

/// Computes line-drawing timings
fn line_draw_time<Transparency, Shading>(
    _psx: &mut Psx,
    start: Position,
    end: Position,
) -> CycleCount
where
    Transparency: TransparencyMode,
    Shading: ShadingMode,
{
    let mut draw_time = 16;

    let w = (start.x - end.x).abs();
    let h = (start.y - end.y).abs();

    if w >= 1024 || h >= 512 {
        // Line is too long, ignore
        return draw_time;
    }

    // XXX from mednafen. Doesn't change depending on the transparency and shading mode which I
    // find odd.
    draw_time += max(w, h) * 2;

    draw_time
}

fn cmd_handle_line<Transparency, Shading>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Shading: ShadingMode,
{
    // Pop command + color
    psx.gpu.command_pop_to_rasterizer();
    // Start position
    let pos = psx.gpu.command_pop_to_rasterizer();
    let start_pos = Position::from_command(pos);

    if Shading::is_shaded() {
        // Pop end color
        psx.gpu.command_pop_to_rasterizer();
    }

    // Pop end position
    let pos = psx.gpu.command_pop_to_rasterizer();
    let end_pos = Position::from_command(pos);

    let draw_time = line_draw_time::<Transparency, Shading>(psx, start_pos, end_pos);
    psx.gpu.draw_time(draw_time);
}

fn cmd_handle_polyline<Transparency, Shading>(psx: &mut Psx)
where
    Transparency: TransparencyMode,
    Shading: ShadingMode,
{
    let (opcode, start_pos) = match psx.gpu.state {
        // We're in the middle of a polyline, use the previous segment's end as start position
        State::PolyLine(o, p) => (o, p),
        // New command
        _ => {
            // Pop command + color
            let cmd = psx.gpu.command_pop_to_rasterizer();
            let opcode = cmd >> 24;
            // Pop start position
            let pos = psx.gpu.command_pop_to_rasterizer();
            (opcode as u8, Position::from_command(pos))
        }
    };

    if Shading::is_shaded() {
        // Pop end color
        psx.gpu.command_pop_to_rasterizer();
    }

    // Pop end position
    let pos = psx.gpu.command_pop_to_rasterizer();
    let end_pos = Position::from_command(pos);

    let draw_time = line_draw_time::<Transparency, Shading>(psx, start_pos, end_pos);
    psx.gpu.draw_time(draw_time);

    // Update state for the next segment
    psx.gpu.state = State::PolyLine(opcode, end_pos);
}

/// Parses the command word for a VRAM store, load or copy and returns the dimensions of the target
/// rectangle
pub fn vram_access_dimensions(dim: u32, is_load: bool) -> (i32, i32) {
    // Width is in GPU pixels, i.e. 16bits per pixel
    let mut width = dim & 0x3ff;
    let mut height = (dim >> 16) & 0x1ff;

    // XXX recheck this, a comment in mednafen says that the results for VRAM load are inconsistent
    if width == 0 {
        width = 1024;
    }

    // XXX not sure about this difference, taken from mednafen
    if is_load {
        if height > 0x200 {
            height &= 0x1ff;
        }
    } else if height == 0 {
        height = 512;
    }

    (width as i32, height as i32)
}

/// Parses the command word for a VRAM store or load and returns the number of words about to be
/// read/written
fn vram_access_length_words(dim: u32, is_load: bool) -> i32 {
    let (width, height) = vram_access_dimensions(dim, is_load);

    // Total number of words to complete the transfer. Since every pixel is 16bit and we transfer
    // 32bits at a time we need to round up
    (width * height + 1) / 2
}

fn cmd_vram_copy(psx: &mut Psx) {
    // Pop command
    psx.gpu.command_pop_to_rasterizer();
    // Source position in VRAM
    psx.gpu.command_pop_to_rasterizer();
    // Target position in VRAM
    psx.gpu.command_pop_to_rasterizer();
    // Dimensions
    let dim = psx.gpu.command_pop_to_rasterizer();

    let (width, height) = vram_access_dimensions(dim, false);

    let duration = width * height * 2;
    psx.gpu.draw_time(duration as CycleCount);
}

fn cmd_vram_store(psx: &mut Psx) {
    // Pop command
    psx.gpu.command_pop_to_rasterizer();
    // Position in VRAM
    psx.gpu.command_pop_to_rasterizer();
    // Dimensions
    let dim = psx.gpu.command_pop_to_rasterizer();

    let nwords = vram_access_length_words(dim, false);
    psx.gpu.state = State::VRamStore(nwords as u32);
}

fn cmd_vram_load(psx: &mut Psx) {
    // Pop command
    psx.gpu.command_pop_to_rasterizer();
    // Position in VRAM
    psx.gpu.command_pop_to_rasterizer();
    // Dimensions
    let dim = psx.gpu.command_pop_to_rasterizer();

    let nwords = vram_access_length_words(dim, true);
    if nwords > 0 {
        // Flush the command to the rasterizer immediately since it'll have to send us the buffer
        // back
        psx.gpu.rasterizer.flush_command_buffer();

        // Let's not block waiting for the reply right now, instead set the frame to None so that
        // we can hopefully wait a little bit longer before we have to stall for the other thread
        psx.gpu.state = State::VRamLoad(None);
    }
}

fn cmd_draw_mode(psx: &mut Psx) {
    let mode = psx.gpu.command_pop_to_rasterizer();

    psx.gpu.draw_mode = DrawMode::new(mode);
}

fn cmd_tex_window(psx: &mut Psx) {
    let tw = psx.gpu.command_pop_to_rasterizer();
    psx.gpu.tex_window.set(tw);
}

fn cmd_clip_top_left(psx: &mut Psx) {
    let clip = psx.gpu.command_pop_to_rasterizer();

    psx.gpu.clip_x_min = (clip & 0x3ff) as i32;
    psx.gpu.clip_y_min = ((clip >> 10) & 0x3ff) as i32;
}

fn cmd_clip_bot_right(psx: &mut Psx) {
    let clip = psx.gpu.command_pop_to_rasterizer();

    psx.gpu.clip_x_max = (clip & 0x3ff) as i32;
    psx.gpu.clip_y_max = ((clip >> 10) & 0x3ff) as i32;
}

fn cmd_draw_offset(psx: &mut Psx) {
    let off = psx.gpu.command_pop_to_rasterizer();

    let off_x = off & 0x7ff;
    let off_y = (off >> 11) & 0x7ff;

    // Sign-extend
    psx.gpu.draw_offset_x = extend_to_i32(off_x, 11);
    psx.gpu.draw_offset_y = extend_to_i32(off_y, 11);
}

fn cmd_mask_settings(psx: &mut Psx) {
    let mask_settings = psx.gpu.command_pop_to_rasterizer() & 0x3f_ffff;

    psx.gpu.mask_settings.set(mask_settings)
}

fn cmd_clear_cache(psx: &mut Psx) {
    // XXX for now we don't implement the GPU cache timings
    psx.gpu.command_pop_to_rasterizer();
}

/// Fill a rectangle with a solid color
fn cmd_fill_rect(psx: &mut Psx) {
    let _color = psx.gpu.command_pop_to_rasterizer();
    let _dst = psx.gpu.command_pop_to_rasterizer();
    let dim = psx.gpu.command_pop_to_rasterizer();

    // Rounding equation taken from Mednafen
    let width = ((dim & 0x3ff) + 0xf) & !0xf;
    let height = (dim >> 16) & 0x1ff;

    let line_delay = (width >> 3) + 9;
    let delay = height * line_delay;

    psx.gpu.draw_time(46 + delay as CycleCount);
}

/// Does nothing, but with style
fn cmd_nop(psx: &mut Psx) {
    // Pop the FIFO.
    //
    // We don't call command_pop_to_rasterizer since it's pointless to send nops to the rasterizer
    psx.gpu.command_fifo.pop();
}

/// Placeholder function
fn cmd_unimplemented(psx: &mut Psx) {
    warn!("GPU command {:08x}", psx.gpu.command_fifo.pop());
}

/// LUT for all GP0 commands (indexed by opcode, bits[31:24] of the first command word)
pub static GP0_COMMANDS: [Command; 0x100] = [
    // 0x00
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_clear_cache,
        len: 1,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_fill_rect,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    // 0x10
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_nop,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0x20
    Command {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, NoShading>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, NoShading>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, NoShading>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, NoShading>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Opaque, TextureBlending, NoShading>,
        len: 7,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Opaque, TextureRaw, NoShading>,
        len: 7,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, TextureBlending, NoShading>,
        len: 7,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, TextureRaw, NoShading>,
        len: 7,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, NoShading>,
        len: 5,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, NoShading>,
        len: 5,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, NoShading>,
        len: 5,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, NoShading>,
        len: 5,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, TextureBlending, NoShading>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, TextureRaw, NoShading>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, TextureBlending, NoShading>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, TextureRaw, NoShading>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0x30
    Command {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, Shaded>,
        len: 6,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Opaque, NoTexture, Shaded>,
        len: 6,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, Shaded>,
        len: 6,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, NoTexture, Shaded>,
        len: 6,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Opaque, TextureBlending, Shaded>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Opaque, TextureRaw, Shaded>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, TextureBlending, Shaded>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_tri::<Transparent, TextureRaw, Shaded>,
        len: 9,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, Shaded>,
        len: 8,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, NoTexture, Shaded>,
        len: 8,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, Shaded>,
        len: 8,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, NoTexture, Shaded>,
        len: 8,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, TextureBlending, Shaded>,
        len: 12,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Opaque, TextureRaw, Shaded>,
        len: 12,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, TextureBlending, Shaded>,
        len: 12,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_poly_quad::<Transparent, TextureRaw, Shaded>,
        len: 12,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0x40
    Command {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, NoShading>,
        len: 3,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0x50
    Command {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_line::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Opaque, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_polyline::<Transparent, Shaded>,
        len: 4,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0x60
    Command {
        handler: cmd_handle_rect_variable::<Opaque, NoTexture>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_variable::<Opaque, NoTexture>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_variable::<Transparent, NoTexture>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_variable::<Transparent, NoTexture>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_variable::<Opaque, TextureBlending>,
        len: 4,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_variable::<Opaque, TextureRaw>,
        len: 4,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_variable::<Transparent, TextureBlending>,
        len: 4,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_variable::<Transparent, TextureRaw>,
        len: 4,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Opaque, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Opaque, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Transparent, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Transparent, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Opaque, TextureBlending>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Opaque, TextureRaw>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Transparent, TextureBlending>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_1x1::<Transparent, TextureRaw>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    // 0x70
    Command {
        handler: cmd_handle_rect_8x8::<Opaque, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_8x8::<Opaque, NoTexture>,
        len: 2,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_8x8::<Transparent, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_8x8::<Transparent, NoTexture>,
        len: 2,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_8x8::<Opaque, TextureBlending>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_8x8::<Opaque, TextureRaw>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_8x8::<Transparent, TextureBlending>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_8x8::<Transparent, TextureRaw>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Opaque, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Opaque, NoTexture>,
        len: 2,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Transparent, NoTexture>,
        len: 2,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Transparent, NoTexture>,
        len: 2,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Opaque, TextureBlending>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Opaque, TextureRaw>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Transparent, TextureBlending>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    Command {
        handler: cmd_handle_rect_16x16::<Transparent, TextureRaw>,
        len: 3,
        fifo_len: 3,
        out_of_band: false,
    },
    // 0x80
    Command {
        handler: cmd_vram_copy,
        len: 4,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0x90
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0xa0
    Command {
        handler: cmd_vram_store,
        len: 3,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0xb0
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0xc0
    Command {
        handler: cmd_vram_load,
        len: 3,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0xd0
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0xe0
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_draw_mode,
        len: 1,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_tex_window,
        len: 1,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_clip_top_left,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_clip_bot_right,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_draw_offset,
        len: 1,
        fifo_len: 1,
        out_of_band: true,
    },
    Command {
        handler: cmd_mask_settings,
        len: 1,
        fifo_len: 2,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    // 0xf0
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
    Command {
        handler: cmd_unimplemented,
        len: 1,
        fifo_len: 1,
        out_of_band: false,
    },
];

#[test]
fn check_poly_callbacks() {
    use crate::psx::{bios, cd, gpu, Psx};

    let dummy_bios = bios::Bios::new_dummy();
    let mut dummy_psx = Psx::new_with_bios(
        None,
        dummy_bios,
        gpu::VideoStandard::Pal,
        [0; cd::CDC_ROM_SIZE],
    )
    .unwrap();

    for op in 0x20..=0x3f {
        let cmd = &GP0_COMMANDS[op];

        if cmd.handler as *const fn() == cmd_unimplemented as *const fn() {
            // Not yet implemented
            continue;
        }

        let is_textured = op & 4 != 0;
        let is_shaded = op & 0x10 != 0;
        let is_quad = op & 8 != 0;

        let num_vertex = if is_quad { 4 } else { 3 };
        let vertex_len = 1 + is_textured as u8 + is_shaded as u8;

        let total_len = num_vertex * vertex_len + (!is_shaded) as u8;

        assert_eq!(cmd.len, total_len);
        assert_eq!(cmd.out_of_band, false);
        assert_eq!(cmd.fifo_len, 1);

        dummy_psx.gpu.reset();

        // Fill the FIFO with a dummy command to make sure the handler consumes the right number of
        // words
        for _ in 0..total_len {
            dummy_psx.gpu.command_fifo.push((op << 24) as u32);
        }

        (cmd.handler)(&mut dummy_psx);

        assert_eq!(dummy_psx.gpu.command_fifo.len(), 0);
    }
}
