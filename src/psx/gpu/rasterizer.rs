//! Code for the rasterizer. It runs in a different threads from the rest of the emulator for
//! performance reasons and communicates through a pair of channels (one to receive draw commands,
//! one to send back the finished frames).

mod draw;

pub use draw::Pixel;
use draw::Rasterizer;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::mpsc;
use std::thread;

/// This is the handle used from the main thread to communicate with the rasterizer
pub struct Handle {
    command_buffer: CommandBuffer,
    frame_pending: bool,
    handle: Option<thread::JoinHandle<()>>,
    command_channel: mpsc::Sender<CommandBuffer>,
    frame_channel: mpsc::Receiver<Frame>,
    serialization_channel: mpsc::Receiver<Vec<u8>>,
}

impl Handle {
    pub fn push_command(&mut self, c: Command) {
        self.command_buffer.push(c);
    }

    /// Send the command buffer to the rasterizer thread
    pub fn flush_command_buffer(&mut self) {
        if self.command_buffer.is_empty() {
            return;
        }

        let mut commands = CommandBuffer::new();

        ::std::mem::swap(&mut commands, &mut self.command_buffer);

        self.command_channel.send(commands).unwrap();
    }

    /// Notify the rasterizer that a line has been fully displayed on the TV output
    pub fn end_of_line(&mut self, line: u16) {
        self.push_command(Command::EndOfLine(line));

        // Flush after each line
        self.flush_command_buffer();
    }

    /// When displaying interlaced video this is called when the displayed field changed
    pub fn field_change(&mut self, bottom_field: bool) {
        // This should be called just after the end of the line, so there shouldn't be any need to
        // flush
        debug_assert!(self.command_buffer.is_empty());

        self.push_command(Command::FieldChanged(bottom_field));
    }

    /// Notify the rasterizer that the current frame is done drawing and should be returned through
    /// the frame channel
    pub fn end_of_frame(&mut self) {
        self.push_command(Command::EndOfFrame);
        self.flush_command_buffer();

        // Make sure we were not already waiting for a frame
        self.take_frame();

        // Instead of blocking immediately waiting for the frame let's just save the fact that we
        // asked for a frame
        self.frame_pending = true;
    }

    pub fn set_option(&mut self, opt: RasterizerOption) {
        self.push_command(Command::option(opt));
        self.flush_command_buffer();
    }

    pub fn take_frame(&mut self) -> Option<Frame> {
        if self.frame_pending {
            self.frame_pending = false;
            Some(self.frame_channel.recv().unwrap())
        } else {
            None
        }
    }

    /// Must be called after a VRAM load command has been sent to the rasterizer and before
    /// finishing the current frame to receive the loaded pixels
    pub fn receive_vram_load(&mut self) -> Frame {
        self.frame_channel.recv().unwrap()
    }

    pub fn push_gp0(&mut self, gp0: u32) {
        self.push_command(Command::Gp0(gp0));
    }

    pub fn push_gp1(&mut self, gp1: u32) {
        self.push_command(Command::Gp1(gp1));
    }
}

impl ::std::ops::Drop for Handle {
    fn drop(&mut self) {
        self.command_buffer.clear();
        self.push_command(Command::Quit);
        self.flush_command_buffer();

        if let Some(t) = self.handle.take() {
            t.join().unwrap();
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SerializedHandle {
    command_buffer: CommandBuffer,
    rasterizer_state: Vec<u8>,
}

impl Serialize for Handle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Ask the rasterizer to serialize its state
        let cmd = vec![Command::Serialize];

        self.command_channel.send(cmd).unwrap();

        let command_buffer = self.command_buffer.clone();
        let rasterizer_state = self.serialization_channel.recv().unwrap();

        let s = SerializedHandle {
            command_buffer,
            rasterizer_state,
        };

        s.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Handle {
    fn deserialize<D>(deserializer: D) -> Result<Handle, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        let s = SerializedHandle::deserialize(deserializer)?;

        let rasterizer = match Rasterizer::from_serialized(&s.rasterizer_state) {
            Some(r) => r,
            None => {
                return Err(de::Error::invalid_value(
                    de::Unexpected::Bytes(&s.rasterizer_state),
                    &"invalid or corrupted rasterizer state",
                ));
            }
        };

        Ok(start_from_state(s.command_buffer, rasterizer))
    }
}

pub fn start_from_state(command_buffer: CommandBuffer, mut rasterizer: Rasterizer) -> Handle {
    let (command_sender, command_receiver) = mpsc::channel();
    let (frame_sender, frame_receiver) = mpsc::channel();
    let (serialization_sender, serialization_receiver) = mpsc::channel();

    let builder = thread::Builder::new()
        .name("RSX GPU".to_string())
        .stack_size(1024 * 1024);

    let handle = builder
        .spawn(move || {
            rasterizer.run(command_receiver, frame_sender, serialization_sender);
        })
        .unwrap();

    Handle {
        command_buffer,
        handle: Some(handle),
        frame_pending: false,
        command_channel: command_sender,
        frame_channel: frame_receiver,
        serialization_channel: serialization_receiver,
    }
}

/// Starts a new rasterizer thread and returns a handle to it
pub fn start() -> Handle {
    start_from_state(Vec::new(), Rasterizer::new())
}

type CommandBuffer = Vec<Command>;

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// GP0 register command
    Gp0(u32),
    /// GP1 register command
    Gp1(u32),
    /// Terminate rasterization
    Quit,
    /// Finalize current line
    EndOfLine(u16),
    /// Field changed. Contains `true` if we're displaying the bottom field, `false` otherwise
    FieldChanged(bool),
    /// Finalize frame and return it through `frame_channel`
    EndOfFrame,
    /// Option setting
    Option(RasterizerOption),
    /// We want to serialize the state of the rasterizer
    Serialize,
}

impl Command {
    fn option(opt: RasterizerOption) -> Command {
        Command::Option(opt)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, PartialEq, Eq, Debug)]
pub enum RasterizerOption {
    VRamDisplayMode(crate::VRamDisplayMode),
    ForceTransparency(bool),
    Draw24Bpp(bool),
    DitherForceDisable(bool),
    Wireframe(bool),
    DrawPolygons(bool),
    UpscaleShift(u8),
}

/// Buffer containing one rendered frame
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct Frame {
    /// Frame pixels in xRGB 8888 format. Its size must always be *exactly* `width * height`.
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
}

impl Frame {
    fn new(width: u32, height: u32) -> Frame {
        let npixels = width * height;

        Frame {
            pixels: vec![0; npixels as usize],
            width,
            height,
        }
    }

    fn set_pixel(&mut self, x: u32, y: u32, p: u32) {
        debug_assert!(x < self.width);
        debug_assert!(y < self.height);

        let x = x as usize;
        let y = y as usize;

        self.pixels[(y * self.width as usize) + x] = p;
    }
}
