use super::{DeviceInterface, DsrState};

/// Digital buttons on a PlayStation controller. The value assigned to each button is the bit
/// position in the 16bit word returned in the serial protocol
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Select = 0,
    L3 = 1,
    R3 = 2,
    Start = 3,
    DUp = 4,
    DRight = 5,
    DDown = 6,
    DLeft = 7,
    L2 = 8,
    R2 = 9,
    L1 = 10,
    R1 = 11,
    Triangle = 12,
    Circle = 13,
    Cross = 14,
    Square = 15,
    Analog = 0xff,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonState {
    Pressed,
    Released,
}

impl ButtonState {
    fn is_pressed(self) -> bool {
        self == ButtonState::Pressed
    }
}

/// SCPH-1080: Digital gamepad.
///
/// Full state is only two bytes since we only need one bit per button.
pub struct DigitalPad(u16);

impl DigitalPad {
    pub fn new() -> DigitalPad {
        DigitalPad(0xffff)
    }
}

impl DeviceInterface for DigitalPad {
    fn description(&self) -> String {
        "PlayStation Digital Controller (SCPH-1080)".to_string()
    }

    fn handle_command(&mut self, seq: u8, cmd: u8) -> (u8, DsrState) {
        let (resp, send_dsr) = match seq {
            // First byte should be 0x01 if the command targets the controller
            0 => (0xff, (cmd == 0x01)),
            // Digital gamepad only supports command 0x42: read buttons.
            //
            // Response 0x41: we're a digital PSX controller
            1 => (0x41, (cmd == 0x42)),
            // From then on the command byte is ignored.
            //
            // Response 0x5a: 2nd controller ID byte
            2 => (0x5a, true),
            // First button state byte: direction cross, start and select.
            3 => (self.0 as u8, true),
            // 2nd button state byte: shoulder buttons and "shape" buttons. We don't assert DSR for
            // the last byte.
            4 => ((self.0 >> 8) as u8, false),
            _ => unreachable!(),
        };

        let dsr_state = if send_dsr {
            // The actual length of the pulse seems to vary between 90 and 100 cycles depending on
            // the controller.
            //
            // Note that the 440 cycle delay is *not* from the moment the RX not empty goes up in the
            // controller, because it seems that there's a ~`baud_divider` delay between the moment
            // the controller handles the command and the moment RX not empty is asserted.
            DsrState::Pending(360, 90)
        } else {
            DsrState::Idle
        };

        (resp, dsr_state)
    }

    fn set_button_state(&mut self, button: Button, state: ButtonState) {
        if button == Button::Analog {
            // No analog button on the digital pad
            return;
        }

        let s = self.0;

        let mask = 1 << (button as usize);

        self.0 = match state {
            ButtonState::Pressed => s & !mask,
            ButtonState::Released => s | mask,
        };

        // Digital pads don't support L3/R3, so those bits are always set to 1
        self.0 |= 0x6;
    }
}

/// SCPH-1200: DualShock controller
pub struct DualShock {
    /// State of the digital buttons
    buttons: u16,
    /// State of the analog selection button
    analog_pressed: bool,
    /// State of the left stick. These are the values returned by the pad on the serial link:
    /// 0x00 all the way to one side, 0xff all the way to the other, 0x80 when the stick is
    /// centered.
    left_stick: (u8, u8),
    /// State of the right stick
    right_stick: (u8, u8),
    /// Calibration factor for the left stick
    left_stick_radius: f32,
    /// Calibration factor for the right stick
    right_stick_radius: f32,
    /// DualShock can optionally deactivate their analog function. This way they behave like
    /// digital gamepads.
    analog_mode: bool,
    /// True if the game locked the analog mode and it can't be changed by pressing the ANALOG
    /// button
    analog_mode_locked: bool,
    /// Special mode activated by using a special command sequence. Changes the controller's
    /// behaviour.
    dualshock_mode: bool,
    /// Type of access for the current command
    access_type: DsAccessType,
    /// If not None it means that the watchdog is active and counts the number of frames since the
    /// last time the controller was active
    watchdog: Option<u8>,
    /// Rumble state for the big motor (left) and small motor (right). 0x00 means off, 0xff means
    /// max. The small motor is always either 0x00 or 0xff.
    rumble: (u8, u8),
    /// Full rumble configuration as set by command 0x4d
    rumble_config: [u8; 6],
    /// Byte offset of the read input command that contain the big and small motor rumble
    /// setting. If rumble is deactivated it's set to (0xff, 0xff).
    rumble_pos: (u8, u8),
    /// Value used in various ways internally by some commands.
    command_internal: u8,
}

impl DualShock {
    pub fn new() -> DualShock {
        DualShock {
            buttons: 0xffff,
            left_stick: (0x80, 0x80),
            right_stick: (0x80, 0x80),
            left_stick_radius: 0.7,
            right_stick_radius: 0.7,
            analog_mode: false,
            analog_mode_locked: false,
            dualshock_mode: false,
            access_type: DsAccessType::ReadInput,
            watchdog: None,
            rumble: (0, 0),
            rumble_config: [0xff; 6],
            rumble_pos: (0xff, 0xff),
            command_internal: 0,
            analog_pressed: false,
        }
    }

    /// Should be called exactly once per frame
    fn run_frame(&mut self) {
        if let Some(ref mut f) = self.watchdog {
            // Watchdog is running.
            *f += 1;

            // On the real hardware the watchdog triggers if we haven't had any activity in about
            // 2.5s. Assuming NTSC framerate that would be about 150 frames.
            if *f > 150 {
                // Reset to digital mode
                info!("Dual Shock watchdog reset to digital mode");

                self.analog_mode = false;
                self.analog_mode_locked = false;
                self.dualshock_mode = false;
                self.rumble = (0, 0);
                self.rumble_config = [0xff; 6];
                self.rumble_pos = (0xff, 0xff);
                self.watchdog = None;
            }
        }
    }

    fn handle_read_input(&mut self, seq: u8, cmd: u8) -> (u8, bool) {
        if self.access_type == DsAccessType::ReadInput {
            if seq == self.rumble_pos.0 {
                // We should be receiving the rumble command for the big motor in the left handle
                self.rumble.0 = cmd;
            }
            if seq == self.rumble_pos.1 {
                // We receive the configuration command for the small rumble motor in the right
                // handle here. It's got only two states, on or off, driven by the bit 1
                let small_motor_on = (cmd & 1) != 0;

                self.rumble.1 = if small_motor_on { 0xff } else { 0x00 };
            }
        }

        match seq {
            // First button state byte: direction cross, start and select.
            3 => {
                let mut response = self.buttons as u8;

                if !self.analog_mode && !self.dualshock_mode {
                    // No L3/R3 in non-analog mode (tested on real hardware)
                    response |= 0x6;
                }

                (response, true)
            }
            // 2nd button state byte: shoulder buttons and "shape" buttons
            4 => {
                // We don't assert DSR in non-analog mode since that's the end of the transaction,
                // unless Dual Shock mode is activated in which case we always return the analog
                // input
                (
                    (self.buttons >> 8) as u8,
                    self.analog_mode || self.dualshock_mode,
                )
            }
            // Right stick X
            5 => (self.right_stick.0, true),
            // Right stick Y
            6 => (self.right_stick.1, true),
            // Left stick X
            7 => (self.left_stick.0, true),
            // Left stick Y
            8 => (self.left_stick.1, false),
            _ => unreachable!(),
        }
    }

    fn handle_change_mode(&mut self, seq: u8, cmd: u8) -> (u8, bool) {
        // The sequence is almost identical to read_input in normal mode.
        let (mut resp, mut send_dsr) = self.handle_read_input(seq, cmd);

        match seq {
            3 => {
                // This is where the mode change occur. Behaviour: if in normal mode we write 0x01
                // to switch to Dual Shock mode. From Dual Shock mode we write 0x00 to return to
                // normal mode. Any other value is ignored.
                if self.dualshock_mode && cmd == 0x00 {
                    self.dualshock_mode = false;
                } else if !self.dualshock_mode && cmd == 0x01 {
                    self.dualshock_mode = true;
                    /* Watchdog is activated */
                    self.watchdog = Some(0);
                }
            }
            4 => {
                // If this command started in normal mode we don't send the analog state if
                // analog_mode is false
                if self.access_type == DsAccessType::NormalChangeMode && !self.analog_mode {
                    send_dsr = false;
                }
            }
            _ => (),
        }

        if self.access_type == DsAccessType::DsChangeMode && seq >= 3 {
            // When run from Dual Shock mode this command returns the same number of bytes but all
            // input state is set to 0
            resp = 0;
        }

        (resp, send_dsr)
    }

    fn handle_set_analog_mode(&mut self, seq: u8, cmd: u8) -> (u8, bool) {
        match seq {
            // Set analog mode
            3 => {
                // Use the command internal to save whether the mode is correct (1) or incorrect
                // (0)
                self.command_internal = 1;

                match cmd {
                    0 => self.analog_mode = false,
                    1 => self.analog_mode = true,
                    _ => {
                        warn!("Received invalid analog mode {:x}", cmd);
                        self.command_internal = 0;
                    }
                }

                (0x00, true)
            }
            4 => {
                match cmd & 3 {
                    2 => self.analog_mode_locked = false,
                    3 => self.analog_mode_locked = true,
                    _ => warn!("Received invalid analog mode lock {:x}", cmd),
                }
                (0x00, true)
            }
            // 0xff if the mode was invalid, 0x00 otherwise. Invalid lock values don't seem to be
            // reflected in the response.
            5 => (
                if self.command_internal == 0 {
                    0xff
                } else {
                    0x00
                },
                true,
            ),
            6 => (0x00, true),
            7 => (0x00, true),
            8 => (0x00, false),
            _ => unreachable!(),
        }
    }

    /// Returns the current state of the analog mode alongside other unknown values
    fn handle_get_analog_mode(&mut self, seq: u8, _cmd: u8) -> (u8, bool) {
        match seq {
            3 => (0x01, true),
            4 => (0x02, true),
            5 => (self.analog_mode as u8, true),
            6 => (0x02, true),
            7 => (0x01, true),
            8 => (0x00, false),
            _ => unreachable!(),
        }
    }

    /// I think those are simply unused commands in the Dual Shock, I don't think they do anything
    /// and I don't think `cmd` is ever used for anything.
    fn handle_dummy_command(&mut self, seq: u8, _cmd: u8) -> (u8, bool) {
        match seq {
            3..=7 => (0x00, true),
            8 => (0x00, false),
            _ => unreachable!(),
        }
    }

    fn handle_mystery_46(&mut self, seq: u8, cmd: u8) -> (u8, bool) {
        match seq {
            3 => {
                // Mystery option for the mystery command
                self.command_internal = cmd;
                (0x00, true)
            }
            4 => (0x00, true),
            n => match self.command_internal {
                0x00 => match n {
                    5 => (0x01, true),
                    6 => (0x02, true),
                    7 => (0x00, true),
                    8 => (0x0a, false),
                    _ => unreachable!(),
                },
                0x01 => match n {
                    5 => (0x01, true),
                    6 => (0x01, true),
                    7 => (0x01, true),
                    8 => (0x14, false),
                    _ => unreachable!(),
                },
                _ => match n {
                    5 => (0x00, true),
                    6 => (0x00, true),
                    7 => (0x00, true),
                    8 => (0x00, false),
                    _ => unreachable!(),
                },
            },
        }
    }

    fn handle_mystery_47(&mut self, seq: u8, _cmd: u8) -> (u8, bool) {
        match seq {
            3 => (0x00, true),
            4 => (0x00, true),
            5 => (0x02, true),
            6 => (0x00, true),
            7 => (0x01, true),
            8 => (0x00, false),
            _ => unreachable!(),
        }
    }

    fn handle_mystery_48(&mut self, seq: u8, _cmd: u8) -> (u8, bool) {
        match seq {
            3 => (0x00, true),
            4 => (0x00, true),
            5 => (0x00, true),
            6 => (0x00, true),
            7 => (0x01, true),
            8 => (0x00, false),
            _ => unreachable!(),
        }
    }

    fn handle_mystery_4c(&mut self, seq: u8, cmd: u8) -> (u8, bool) {
        match seq {
            3 => {
                // Mystery option for the mystery command
                self.command_internal = cmd;
                (0x00, true)
            }
            4 | 5 => (0x00, true),
            6 => match self.command_internal {
                0x00 => (0x04, true),
                0x01 => (0x07, true),
                _ => (0x00, true),
            },
            7 => (0x00, true),
            8 => (0x00, false),
            _ => unreachable!(),
        }
    }

    fn handle_rumble_config(&mut self, seq: u8, cmd: u8) -> (u8, bool) {
        let response = match seq {
            2 => 0x5a,
            3..=8 => {
                // Get new rumble config byte, return the old one
                let index = (seq - 3) as usize;

                let old = self.rumble_config[index];
                self.rumble_config[index] = cmd;
                old
            }
            _ => unreachable!(),
        };

        let dsr_active = if seq < 8 {
            true
        } else {
            // Last byte received, check config
            self.rumble_pos = match self.rumble_config {
                // Standard command to deactivate the rumble. I've checked on real hardware that
                // sending this command does *not* stop the motors if they're currently active.
                [0xff, 0xff, 0xff, 0xff, 0xff, 0xff] => (0xff, 0xff),
                // Standard command to activate the rumble: we receive the commands for the small
                // and big motor on bytes 3 and 4 respectively
                [0x00, 0x01, 0xff, 0xff, 0xff, 0xff] => (4, 3),
                // Command used by FFVIII: same as above but one byte later
                [0xff, 0x00, 0x01, 0xff, 0xff, 0xff] => (5, 4),
                _ => {
                    error!("Unsupported rumble config {:x?}", self.rumble_config);
                    // XXX There are many, many possible configurations for this command. You can
                    // unlock only one engine, swap their config, make them share the config etc...
                    // Since we don't know what this configuration does, we disable rumble
                    (0xff, 0xff)
                }
            };

            false
        };

        (response, dsr_active)
    }
}

impl DeviceInterface for DualShock {
    fn description(&self) -> String {
        "PlayStation DualShock Analog Controller (SCPH-1200)".to_string()
    }

    fn select(&mut self) {
        // Watchdog is reset every time the select signal goes down, even if the controller is not
        // the target. I assume that the logic is that the controller should return to the default
        // digital mode if the console is reset since the BIOS boot sequence is fairly long.
        if self.watchdog.is_some() {
            // Reset watchdog
            self.watchdog = Some(0);
        }
    }

    fn handle_command(&mut self, seq: u8, cmd: u8) -> (u8, DsrState) {
        // The following sequence assumes that the controller is in DualShock mode (with the ANALOG
        // LED switched on).
        let (resp, send_dsr) = match seq {
            // First byte should be 0x01 if the command targets the controller
            0 => (0xff, (cmd == 0x01)),
            1 => {
                let response = if self.dualshock_mode {
                    0xf3
                } else if self.analog_mode {
                    // Response 0x73: we're a DualShock
                    0x73
                } else {
                    // Response 0x41: we're a digital PSX controller
                    0x41
                };

                let mut continue_sequence = true;

                self.access_type = if self.dualshock_mode {
                    match cmd {
                        0x40 => DsAccessType::DsDummyCommand,
                        0x41 => DsAccessType::DsDummyCommand,
                        0x42 => DsAccessType::ReadInput,
                        0x43 => DsAccessType::DsChangeMode,
                        0x44 => DsAccessType::DsSetAnalogMode,
                        0x45 => DsAccessType::DsGetAnalogMode,
                        0x46 => DsAccessType::DsMystery46,
                        0x47 => DsAccessType::DsMystery47,
                        0x48 => DsAccessType::DsMystery48,
                        0x49 => DsAccessType::DsDummyCommand,
                        0x4a => DsAccessType::DsDummyCommand,
                        0x4b => DsAccessType::DsDummyCommand,
                        0x4c => DsAccessType::DsMystery4c,
                        0x4d => DsAccessType::DsRumbleConfig,
                        0x4e => DsAccessType::DsDummyCommand,
                        0x4f => DsAccessType::DsDummyCommand,
                        _ => {
                            warn!("Unhandled DualShock command {:x}", cmd);
                            continue_sequence = false;
                            DsAccessType::ReadInput
                        }
                    }
                } else {
                    // "Normal" mode
                    match cmd {
                        0x42 => DsAccessType::ReadInput,
                        0x43 => DsAccessType::NormalChangeMode,
                        _ => {
                            warn!("Unhandled normal command {:x}", cmd);
                            continue_sequence = false;
                            DsAccessType::ReadInput
                        }
                    }
                };

                (response, continue_sequence)
            }
            2 => {
                // In my tests the controller doesn't like it when this byte is not 0, but *only*
                // if we're in DualShock mode, otherwise it seems to be ignored. ReadInput (0x42)
                // also seems to ignore this byte in either modes
                if self.access_type != DsAccessType::ReadInput && self.dualshock_mode && cmd != 0 {
                    (0x5a, false)
                } else {
                    (0x5a, true)
                }
            }
            // After that the sequence changes depending on the access_type
            n => match self.access_type {
                DsAccessType::ReadInput => self.handle_read_input(n, cmd),
                DsAccessType::NormalChangeMode => self.handle_change_mode(n, cmd),
                DsAccessType::DsChangeMode => self.handle_change_mode(n, cmd),
                DsAccessType::DsSetAnalogMode => self.handle_set_analog_mode(n, cmd),
                DsAccessType::DsGetAnalogMode => self.handle_get_analog_mode(n, cmd),
                DsAccessType::DsDummyCommand => self.handle_dummy_command(n, cmd),
                DsAccessType::DsMystery46 => self.handle_mystery_46(n, cmd),
                DsAccessType::DsMystery47 => self.handle_mystery_47(n, cmd),
                DsAccessType::DsMystery48 => self.handle_mystery_48(n, cmd),
                DsAccessType::DsMystery4c => self.handle_mystery_4c(n, cmd),
                DsAccessType::DsRumbleConfig => self.handle_rumble_config(n, cmd),
            },
        };

        let dsr_state = if send_dsr {
            DsrState::Pending(360, 64)
        } else {
            DsrState::Idle
        };

        (resp, dsr_state)
    }

    fn set_button_state(&mut self, button: Button, state: ButtonState) {
        if button == Button::Analog {
            let was_pressed = self.analog_pressed;

            self.analog_pressed = state.is_pressed();

            if !self.analog_mode_locked && !was_pressed && self.analog_pressed {
                // Analog button was just pressed and the mode isn't locked, toggle analog mode
                self.analog_mode = !self.analog_mode;
            }

            return;
        }

        let s = self.buttons;

        let mask = 1 << (button as usize);

        self.buttons = match state {
            ButtonState::Pressed => s & !mask,
            ButtonState::Released => s | mask,
        };
    }

    fn set_axis_state(&mut self, left: (i16, i16), right: (i16, i16)) {
        // Here's how the calibration works: at the start left_stick_radius and
        // right_stick_radius are set to a small-ish value. Every time we receive controller
        // axis input from the frontend we compute the current distance between the center and
        // the current position. If it's greater than the corresponding *_stick_radius we
        // update it. This way we should quickly get a good estimate of the effective stick radius
        // of the controller we're using (at least once the user bothers to reach the full range of
        // the stick).
        fn radius(pos: (i16, i16)) -> f32 {
            let x = (pos.0 as f32) / (i16::max_value() as f32);
            let y = (pos.1 as f32) / (i16::max_value() as f32);

            (x * x + y * y).sqrt()
        }

        let left_radius = radius(left);
        if left_radius > self.left_stick_radius {
            self.left_stick_radius = left_radius;
        }

        let right_radius = radius(right);
        if right_radius > self.right_stick_radius {
            self.right_stick_radius = right_radius;
        }

        // This represents the maximal value the DualShock sticks can reach, where 1.0 would be the
        // maximum value along the X or Y axis. On my DualShock I can *almost* reach the corner of
        // the square (I read x = 0x00, y = 0x05)
        const DUALSHOCK_ANALOG_RADIUS: f32 = 1.387;

        // Now that we know both the real and emulated radii we can scale the values we've received
        // to compensate
        let l_scaling = DUALSHOCK_ANALOG_RADIUS / self.left_stick_radius;
        let r_scaling = DUALSHOCK_ANALOG_RADIUS / self.right_stick_radius;

        fn scale(v: i16, scaling: f32) -> u8 {
            let mut v = f32::from(v) * scaling;

            // The frontend sends us 16bits of resolution per axis but the PSX only uses 8, so we
            // scale down
            v /= 0x100 as f32;

            // The pad returns 0x80 at 0 so we need to offset
            v += 0x80 as f32;

            let v = v as i32;

            if v > 0xff {
                0xffu8
            } else if v < 0 {
                0
            } else {
                v as u8
            }
        }

        self.left_stick = (scale(left.0, l_scaling), scale(left.1, l_scaling));
        self.right_stick = (scale(right.0, r_scaling), scale(right.1, r_scaling));

        // XXX assume that this function is called exactly once per frame
        self.run_frame();
    }

    fn get_rumble(&self) -> (u8, u8) {
        self.rumble
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum DsAccessType {
    ReadInput,
    /// Change mode while we're in normal mode
    NormalChangeMode,
    /// Change mode while we're in Dual Shock mode
    DsChangeMode,
    /// Force analog mode and lock or unlock it
    DsSetAnalogMode,
    /// Returns the current state of the analog mode
    DsGetAnalogMode,
    /// Dummy DualShock command command
    DsDummyCommand,
    /// Unknown command 0x46
    DsMystery46,
    /// Unknown command 0x47
    DsMystery47,
    /// Unknown command 0x48
    DsMystery48,
    /// Unknown command 0x4c
    DsMystery4c,
    /// Rumble configuration command (doesn't actually start the rumble, just enables it)
    DsRumbleConfig,
}
