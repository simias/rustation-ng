//! Code for the rasterizer. It runs in a different threads from the rest of the emulator for
//! performance reasons and communicates through a pair of channels (one to receive draw commands,
//! one to send back the finished frames).

mod draw;

pub use draw::Pixel;
use draw::Rasterizer;
use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, SerializeStruct, Serializer};
use std::sync::mpsc;
use std::thread;

enum LastFrame {
    /// We asked the rasterizer to provide a frame and we're waiting for it on the `frame_channel`
    Pending,
    /// The last frame we've received
    Frame(Frame),
}

/// This is the handle used from the main thread to communicate with the rasterizer
pub struct Handle {
    command_buffer: CommandBuffer,
    last_frame: LastFrame,
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
        self.last_frame();

        // Instead of blocking immediately waiting for the frame let's just save the fact that we
        // asked for a frame
        self.last_frame = LastFrame::Pending;
    }

    pub fn set_option(&mut self, opt: RasterizerOption) {
        self.push_command(Command::option(opt));
        self.flush_command_buffer();
    }

    pub fn last_frame(&mut self) -> &Frame {
        match self.last_frame {
            LastFrame::Pending => {
                let frame = self.frame_channel.recv().unwrap();

                self.last_frame = LastFrame::Frame(frame);

                // Recursively call this function because why not
                self.last_frame()
            }
            LastFrame::Frame(ref f) => f,
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

impl Serialize for Handle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Ask the rasterizer to serialize its state
        let cmd = vec![Command::Serialize];

        self.command_channel.send(cmd).unwrap();

        let mut state = serializer.serialize_struct("Handle", 2)?;

        state.serialize_field("command_buffer", &self.command_buffer)?;

        let rs = self.serialization_channel.recv().unwrap();
        state.serialize_field("rasterizer_state", &rs)?;

        state.end()
    }
}

impl<'de> Deserialize<'de> for Handle {
    fn deserialize<D>(_deserializer: D) -> Result<Handle, D::Error>
    where
        D: Deserializer<'de>,
    {
        todo!()
    }
}

/// Starts the rasterizer thread and returns a handle to it
pub fn start() -> Handle {
    let (command_sender, command_receiver) = mpsc::channel();
    let (frame_sender, frame_receiver) = mpsc::channel();
    let (serialization_sender, serialization_receiver) = mpsc::channel();

    let builder = thread::Builder::new()
        .name("RSX GPU".to_string())
        .stack_size(1024 * 1024);

    let handle = builder
        .spawn(move || {
            let mut rasterizer = Rasterizer::new();
            rasterizer.run(command_receiver, frame_sender, serialization_sender);
        })
        .unwrap();

    let dummy_frame = Frame::new(0, 0);

    Handle {
        command_buffer: Vec::new(),
        last_frame: LastFrame::Frame(dummy_frame),
        handle: Some(handle),
        command_channel: command_sender,
        frame_channel: frame_receiver,
        serialization_channel: serialization_receiver,
    }
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
    DisplayFullVRam(bool),
    ForceTransparency(bool),
    Draw24Bpp(bool),
    DitherForceDisable(bool),
    Wireframe(bool),
    DrawPolygons(bool),
}

/// Buffer containing one rendered frame
#[derive(serde::Serialize, serde::Deserialize, Clone)]
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
