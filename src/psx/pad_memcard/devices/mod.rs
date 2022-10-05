pub mod gamepad;
pub mod memory_card;

use super::DsrState;
use gamepad::{Button, ButtonState, GamePad};
use memory_card::MemoryCardInterface;
use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, SerializeStruct, Serializer};

pub struct Peripheral<T: DeviceInterface + ?Sized> {
    /// Connected device
    device: Box<T>,
    /// Counter keeping track of the current position in the reply sequence
    seq: u8,
    /// False if the device is done processing the current command
    active: bool,
}

impl<T: DeviceInterface + ?Sized> Peripheral<T> {
    fn new(device: Box<T>) -> Peripheral<T> {
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
    pub fn device(&self) -> &T {
        &*self.device
    }

    /// Return a mutable reference to the connected device
    pub fn device_mut(&mut self) -> &mut T {
        &mut *self.device
    }

    /// Change the connected device
    pub fn connect_device(&mut self, device: Box<T>) {
        self.device = device
    }
}

impl<T: DeviceInterface + ?Sized> Serialize for Peripheral<T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // We don't serialize the device, because it could potentially change independently of the
        // savestate (like changing the controller type). At worst it could mean a glitchy
        let mut state = serializer.serialize_struct("Peripheral", 2)?;
        state.serialize_field("seq", &self.seq)?;
        state.serialize_field("active", &self.active)?;
        state.end()
    }
}

impl<'de, T: DeviceInterface + ?Sized> Deserialize<'de> for Peripheral<T> {
    fn deserialize<D>(_deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        todo!()
    }
}

/// Trait used to abstract away the various controller types.
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

impl GamePad for DisconnectedDevice {
    fn set_button_state(&mut self, _: Button, _: ButtonState) {}
}

impl MemoryCardInterface for DisconnectedDevice {
    fn get_memory(&self) -> Option<&[u8; memory_card::FLASH_SIZE]> {
        None
    }

    fn write_counter(&self) -> u32 {
        0
    }
}

pub fn disconnected_gamepad() -> Peripheral<dyn GamePad> {
    Peripheral::new(Box::new(DisconnectedDevice))
}

pub fn disconnected_memory_card() -> Peripheral<dyn MemoryCardInterface> {
    Peripheral::new(Box::new(DisconnectedDevice))
}
