pub mod gamepad;
pub mod memory_card;

use super::DsrState;
use gamepad::{Button, ButtonState};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub struct Peripheral {
    /// Connected device
    device: Box<dyn DeviceInterface>,
    /// Counter keeping track of the current position in the reply sequence
    seq: u8,
    /// False if the device is done processing the current command
    active: bool,
}

impl Peripheral {
    fn new(device: Box<dyn DeviceInterface>) -> Peripheral {
        Peripheral {
            device,
            seq: 0,
            active: false,
        }
    }

    /// Called when the "select" line goes low.
    pub fn select(&mut self) {
        // Prepare for incoming command
        self.active = true;
        self.seq = 0;

        self.device.select();
    }

    /// The 1st return value is the response byte. The 2nd return value contains the state of the
    /// DSR pulse to notify the controller that more data can be read. If the device wants to
    /// complete the transaction it'll return DsrState::Idle
    pub fn exchange_byte(&mut self, cmd: u8) -> (u8, DsrState) {
        if !self.active {
            return (0xff, DsrState::Idle);
        }

        let (resp, dsr_state) = self.device.handle_command(self.seq, cmd);

        // If we're not asserting DSR it either means that we've encountered an error or that we
        // have nothing else to reply. In either case we won't be handling any more command bytes
        // in this transaction.
        self.active = dsr_state != DsrState::Idle;

        self.seq += 1;

        (resp, dsr_state)
    }

    /// Return a reference to the connected device
    pub fn device(&self) -> &dyn DeviceInterface {
        &*self.device
    }

    /// Return a mutable reference to the connected device
    pub fn device_mut(&mut self) -> &mut dyn DeviceInterface {
        &mut *self.device
    }

    /// Change the connected device, returning the old one (will return an instance of
    /// DisconnectedDevice if there was no previously connected device)
    pub fn connect_device(
        &mut self,
        mut device: Box<dyn DeviceInterface>,
    ) -> Box<dyn DeviceInterface> {
        std::mem::swap(&mut self.device, &mut device);

        self.device.connected();

        device
    }

    /// Disconnect the device and return it. Returns an instance of DisconnectedDevice if nothing
    /// was connected
    pub fn disconnect_device(&mut self) -> Box<dyn DeviceInterface> {
        self.connect_device(Box::new(DisconnectedDevice))
    }
}

#[derive(Serialize, Deserialize)]
struct SerializedPeripheral {
    seq: u8,
    active: bool,
}

impl Serialize for Peripheral {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // We don't serialize the device, because it could potentially change independently of the
        // savestate (like changing the controller type). At worst it could mean a glitchy
        let s = SerializedPeripheral {
            seq: self.seq,
            active: self.active,
        };

        s.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Peripheral {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = SerializedPeripheral::deserialize(deserializer)?;

        let mut peripheral = Peripheral::new(Box::new(DisconnectedDevice));

        peripheral.seq = s.seq;
        peripheral.active = s.active;

        Ok(peripheral)
    }
}

/// Trait used to abstract away the various device types.
///
/// This can be used to implement both controllers and memory cards. Obviously the methods that are
/// irrelevant for the concrete device should be left unimplemented (no sense getting the
/// `write_counter` of a DualShock or setting the `axis_state` of a MemoryCard.
pub trait DeviceInterface {
    /// Human-readable description of the device
    fn description(&self) -> String;

    /// Called every time the device is selected (i.e. the "/select" signal goes low)
    fn select(&mut self) {}

    /// Handle a command byte sent by the console. `seq` is the byte position in the current
    /// command starting with `1` since byte `0` is expected to always be `0x01` when addressing a
    /// controller and is handled at the top level.
    ///
    /// Returns a pair `(response, dsr)`. If DSR is false the subsequent command bytes will be
    /// ignored for the current transaction.
    fn handle_command(&mut self, seq: u8, cmd: u8) -> (u8, DsrState);

    /// Set the state of individual buttons
    fn set_button_state(&mut self, _button: Button, _state: ButtonState) {}

    /// Set the state of the axis. Each pair is `(x, y)`.
    fn set_axis_state(&mut self, _left: (i16, i16), _right: (i16, i16)) {}

    /// Get rumble state. The first u8 is the big motor in the left handle, the 2nd is the small
    /// motor in the right handle.
    fn get_rumble(&self) -> (u8, u8) {
        (0, 0)
    }

    /// Dump the entirety of the device's flash (if it exists). Probably only useful for Memory
    /// Cards.
    fn get_memory(&self) -> Option<&[u8; memory_card::FLASH_SIZE]> {
        None
    }

    /// Returns the value of a counter that's incremented every time the memory card's flash is
    /// written (unless the write didn't change the flash contents, in which case it's ignored).
    /// Can be used to check if the contents of the memory card should be written to disk.
    fn write_counter(&self) -> u32 {
        0
    }

    /// Called when the device is connected to a console
    fn connected(&mut self) {}

    /// Called once per frame
    fn new_frame(&mut self) {}
}

/// Dummy profile emulating an empty pad or memory card slot
pub struct DisconnectedDevice;

impl DeviceInterface for DisconnectedDevice {
    fn description(&self) -> String {
        "Disconnected".to_string()
    }

    fn handle_command(&mut self, _: u8, _: u8) -> (u8, DsrState) {
        // The bus is open, no response
        (0xff, DsrState::Idle)
    }
}

pub fn disconnected_gamepad() -> Peripheral {
    Peripheral::new(Box::new(DisconnectedDevice))
}

pub fn disconnected_memory_card() -> Peripheral {
    Peripheral::new(Box::new(DisconnectedDevice))
}
