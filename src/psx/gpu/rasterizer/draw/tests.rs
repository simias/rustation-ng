//! Rasterizer tests
//!
//! Unless otherwise noted the expected output was generated on a real PlayStation (model
//! SCPH-7502, PAL).

use super::{Command, CommandBuffer, Pixel, Rasterizer};
use std::sync::mpsc;

fn build_rasterizer() -> (
    Rasterizer,
    mpsc::Sender<CommandBuffer>,
    mpsc::Receiver<CommandBuffer>,
) {
    let (command_sender, command_receiver) = mpsc::channel();

    let rasterizer = Rasterizer::new();

    let init_commands = vec![
        // Reset
        Command::Gp1(0x00000000),
        // Set drawing area top left at 0, 0
        Command::Gp0(0xe3000000),
        // Set drawing area bottom right at 256, 256
        Command::Gp0(0xe4000000 | 256 | (256 << 10)),
        // Set drawing offset at 0, 0
        Command::Gp0(0xe5000000),
    ];

    command_sender.send(init_commands).unwrap();

    (rasterizer, command_sender, command_receiver)
}

fn vertex_coord(x: i16, y: i16) -> Command {
    let x = x as u16;
    let y = y as u16;

    Command::Gp0((x as u32) | ((y as u32) << 16))
}

fn bgr_px(bgr: u32) -> Pixel {
    Pixel::from_command(bgr)
}

fn mbgr_px(mbgr: u16) -> Pixel {
    Pixel::from_mbgr1555(mbgr)
}

fn check_rasterizer(rasterizer: &Rasterizer, expected: &[&[Pixel]]) {
    for (y, line) in expected.iter().enumerate() {
        for (x, &color) in line.iter().enumerate() {
            let p = rasterizer.vram[y * 1024 + x].to_mbgr1555();
            let color = color.to_mbgr1555();

            assert_eq!(
                color, p,
                "VRAM {}x{}: expected 0x{:x} got 0x{:x}",
                x, y, color, p
            );
        }
    }
}

#[test]
fn quad_rect_solid_opaque() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        // Draw a red quad
        Command::Gp0(0x280000ff),
        vertex_coord(2, 2),
        vertex_coord(2, 6),
        vertex_coord(4, 2),
        vertex_coord(4, 6),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x],
        &[x, x, x, x, x, x],
        &[x, x, r, r, x, x],
        &[x, x, r, r, x, x],
        &[x, x, r, r, x, x],
        &[x, x, r, r, x, x],
        &[x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

/*
 * Triangle rasterization tests
 */

#[test]
fn triangle_solid_opaque_pyramid_up() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(5, 2),
        vertex_coord(2, 5),
        vertex_coord(8, 5),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x],
        &[x, x, x, x, r, r, x, x],
        &[x, x, x, r, r, r, r, x],
        &[x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_pyramid_down() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(5, 5),
        vertex_coord(2, 2),
        vertex_coord(8, 2),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, r, r, r, r, r, r, x],
        &[x, x, x, r, r, r, r, x, x],
        &[x, x, x, x, r, r, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_flat_up() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(5, 1),
        vertex_coord(2, 6),
        vertex_coord(9, 6),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, r, x, x, x, x],
        &[x, x, x, x, r, r, r, x, x, x],
        &[x, x, x, x, r, r, r, r, x, x],
        &[x, x, x, r, r, r, r, r, r, x],
        &[x, x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_flat_down() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(5, 6),
        vertex_coord(2, 1),
        vertex_coord(9, 1),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, r, r, r, r, r, r, r, x],
        &[x, x, x, r, r, r, r, r, r, x],
        &[x, x, x, x, r, r, r, r, x, x],
        &[x, x, x, x, r, r, r, x, x, x],
        &[x, x, x, x, x, r, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_flat_right() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(6, 5),
        vertex_coord(1, 2),
        vertex_coord(1, 9),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x],
        &[x, r, r, x, x, x, x],
        &[x, r, r, r, r, x, x],
        &[x, r, r, r, r, r, x],
        &[x, r, r, r, r, x, x],
        &[x, r, r, r, x, x, x],
        &[x, r, r, x, x, x, x],
        &[x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_flat_left() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(1, 5),
        vertex_coord(6, 2),
        vertex_coord(6, 9),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x],
        &[x, x, x, x, x, r, x],
        &[x, x, x, r, r, r, x],
        &[x, r, r, r, r, r, x],
        &[x, x, x, r, r, r, x],
        &[x, x, x, x, r, r, x],
        &[x, x, x, x, x, r, x],
        &[x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_slant_top_left() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(2, 2),
        vertex_coord(4, 6),
        vertex_coord(9, 6),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, r, x, x, x, x, x],
        &[x, x, x, r, r, r, x, x, x],
        &[x, x, x, x, r, r, r, r, x],
        &[x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_slant_top_right() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(8, 2),
        vertex_coord(1, 6),
        vertex_coord(6, 6),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, r, x],
        &[x, x, x, x, x, r, r, x, x],
        &[x, x, x, r, r, r, r, x, x],
        &[x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_slant_bot_left() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(2, 6),
        vertex_coord(4, 2),
        vertex_coord(9, 2),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, r, r, r, r, r, x],
        &[x, x, x, x, r, r, r, r, x, x],
        &[x, x, x, r, r, r, x, x, x, x],
        &[x, x, x, r, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_slant_bot_right() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(9, 6),
        vertex_coord(2, 2),
        vertex_coord(7, 2),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, r, r, r, r, r, x, x, x],
        &[x, x, x, x, r, r, r, r, x, x],
        &[x, x, x, x, x, x, r, r, x, x],
        &[x, x, x, x, x, x, x, x, r, x],
        &[x, x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_mid_right() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(1, 1),
        vertex_coord(8, 5),
        vertex_coord(3, 8),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
        &[x, x, r, x, x, x, x, x, x],
        &[x, x, r, r, r, x, x, x, x],
        &[x, x, r, r, r, r, r, x, x],
        &[x, x, x, r, r, r, r, r, x],
        &[x, x, x, r, r, r, r, x, x],
        &[x, x, x, r, r, x, x, x, x],
        &[x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

#[test]
fn triangle_solid_opaque_mid_left() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(9, 0),
        vertex_coord(1, 4),
        vertex_coord(6, 9),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x, x, x, x, x],
        &[x, x, x, x, x, x, x, r, r, x],
        &[x, x, x, x, x, r, r, r, r, x],
        &[x, x, x, r, r, r, r, r, x, x],
        &[x, r, r, r, r, r, r, r, x, x],
        &[x, x, r, r, r, r, r, r, x, x],
        &[x, x, x, r, r, r, r, x, x, x],
        &[x, x, x, x, r, r, r, x, x, x],
        &[x, x, x, x, x, r, r, x, x, x],
        &[x, x, x, x, x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

/// Draw a large triangle with a non-trivial edge slope to catch precision errors
#[test]
fn triangle_solid_opaque_big1() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(0, 0),
        vertex_coord(0, 500),
        vertex_coord(125, 500),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, r, x],
        &[r, r, r, r, r, x],
        &[r, r, r, r, r, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

/// Same as big1 but with a small x offset change, enough to change the drawing slightly
#[test]
fn triangle_solid_opaque_big2() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x200000ff),
        vertex_coord(0, 0),
        vertex_coord(0, 500),
        vertex_coord(126, 500),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, x, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, x, x],
        &[r, r, r, r, r, x],
        &[r, r, r, r, r, x],
        &[r, r, r, r, r, x],
        &[r, r, r, r, r, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

/// The PSX only allows to draw triangles up to 512 pixels in height and 1024 pixels in width
#[test]
fn triangle_solid_opaque_draw_limits() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        // First a red triangle with the max possible size
        Command::Gp0(0x200000ff),
        vertex_coord(-10, -1),
        vertex_coord(-10, 510),
        vertex_coord(1013, 510),
        // Then attempt to draw a slightly taller green triangle (shouldn't draw anything)
        Command::Gp0(0x2000ff00),
        vertex_coord(-10, -1),
        vertex_coord(-10, 511),
        vertex_coord(1013, 510),
        // Then attempt to draw a slightly wider blue triangle (shouldn't draw anything)
        Command::Gp0(0x20ff0000),
        vertex_coord(-10, -1),
        vertex_coord(-11, 510),
        vertex_coord(1013, 510),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x],
        &[x, x, x, x, x, x],
        &[x, x, x, x, x, x],
        &[x, x, x, x, x, x],
        &[r, x, x, x, x, x],
        &[r, r, r, x, x, x],
        &[r, r, r, r, r, x],
        &[r, r, r, r, r, r],
    ];

    check_rasterizer(&rasterizer, &expected);
}

/// Two triangles with slightly different coordinates which actually end up drawing the exact
/// same pixels
#[test]
fn triangle_solid_opaque_false_friends() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x2000_00ff),
        vertex_coord(1, 1),
        vertex_coord(1, 6),
        vertex_coord(6, 6),
        // This 2nd triangle has a slightly different shape but it ends up drawing exactly the same
        // pixel pattern
        Command::Gp0(0x2000_00ff),
        vertex_coord(0, 7),
        vertex_coord(1, 12),
        vertex_coord(5, 11),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[x, x, x, x, x, x],
        &[x, x, x, x, x, x],
        &[x, r, x, x, x, x],
        &[x, r, r, x, x, x],
        &[x, r, r, r, x, x],
        &[x, r, r, r, r, x],
        &[x, x, x, x, x, x],
        &[x, x, x, x, x, x],
        &[x, r, x, x, x, x],
        &[x, r, r, x, x, x],
        &[x, r, r, r, x, x],
        &[x, r, r, r, r, x],
        &[x, x, x, x, x, x],
    ];

    check_rasterizer(&rasterizer, &expected);
}

/*
 * Gouraud shading tests
 */

#[test]
fn gouraud_rgb_right() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        Command::Gp0(0x300000ff),
        vertex_coord(1, 0),
        Command::Gp0(0x0000ff00),
        vertex_coord(1, 9),
        Command::Gp0(0x00ff0000),
        vertex_coord(9, 9),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let p = mbgr_px;

    let expected: &[&[Pixel]] = &[
        &[p(0), p(0), p(0), p(0), p(0), p(0), p(0), p(0), p(0), p(0)],
        &[
            p(0),
            p(0x007c),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
        ],
        &[
            p(0),
            p(0x00f8),
            p(0x1078),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
        ],
        &[
            p(0),
            p(0x0155),
            p(0x10d5),
            p(0x2055),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
        ],
        &[
            p(0),
            p(0x01d1),
            p(0x1151),
            p(0x20d1),
            p(0x3051),
            p(0),
            p(0),
            p(0),
            p(0),
            p(0),
        ],
        &[
            p(0),
            p(0x022e),
            p(0x11ae),
            p(0x212e),
            p(0x30ae),
            p(0x402e),
            p(0),
            p(0),
            p(0),
            p(0),
        ],
        &[
            p(0),
            p(0x02aa),
            p(0x122a),
            p(0x21aa),
            p(0x312a),
            p(0x40aa),
            p(0x4c2a),
            p(0),
            p(0),
            p(0),
        ],
        &[
            p(0),
            p(0x0307),
            p(0x1287),
            p(0x2207),
            p(0x3187),
            p(0x4107),
            p(0x4c87),
            p(0x5c07),
            p(0),
            p(0),
        ],
        &[
            p(0),
            p(0x0383),
            p(0x1303),
            p(0x2283),
            p(0x3203),
            p(0x4183),
            p(0x4d03),
            p(0x5c83),
            p(0x6c03),
            p(0),
        ],
        &[p(0), p(0), p(0), p(0), p(0), p(0), p(0), p(0), p(0), p(0)],
    ];

    check_rasterizer(&rasterizer, &expected);
}

/// Test for an overflow issue in Spyro (PAL)
#[test]
fn test_overflow() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        // Clip top-left
        Command::Gp0(0xe3000000),
        // Clip bot-right
        Command::Gp0(0xe4000000 | (0xff << 10) | 0x1ff),
        // Draw offset
        Command::Gp0(0xe5000000),
        // Quad
        Command::Gp0(0x387ecbff),
        Command::Gp0(0x00890145),
        Command::Gp0(0x009c6e62),
        Command::Gp0(0x00910141),
        Command::Gp0(0x0055afff),
        Command::Gp0(0x009d0133),
        Command::Gp0(0x00a57670),
        Command::Gp0(0x00a4012b),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);
}

/// Test for a broken triangle in PSX's intro when FpCoord::epsilon() is set to 1.
#[test]
fn test_bad_draw_psx_logo() {
    let (mut rasterizer, command_channel, command_receiver) = build_rasterizer();
    let (frame_sender, _frame_receiver) = mpsc::channel();
    let (serialization_sender, _serialization_receiver) = mpsc::channel();

    let commands = vec![
        // Triangle
        Command::Gp0(0x200000ff),
        Command::Gp0(0x00090001),
        Command::Gp0(0x0000000a),
        Command::Gp0(0x00040023),
        Command::Quit,
    ];

    command_channel.send(commands).unwrap();

    rasterizer.run(command_receiver, frame_sender, serialization_sender);

    let x = Pixel::black();
    let r = bgr_px(0x0000ff);

    let expected: &[&[Pixel]] = &[
        &[
            x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, x, x, x, x, x, x, x, r, r, r, r, r, r, r, r, x, x, x, x, x, x, x, x, x, x, x, x,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, x, x, x, x, x, x, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, x, x, x, x, x, x,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, x, x, x, x, x, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, x, x, x, x, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r,
            r, r, r, r, r, r, x, x, x, x, x,
        ],
        &[
            x, x, x, x, x, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, x, x, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, r, x, x, x, x, x, x, x,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, x, r, r, r, r, r, r, r, r, r, r, r, r, x, x, x, x, x, x, x, x, x, x, x, x, x, x,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, r, r, r, r, r, r, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
        &[
            x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x, x,
            x, x, x, x, x, x, x, x, x, x, x,
        ],
    ];

    check_rasterizer(&rasterizer, &expected);
}
