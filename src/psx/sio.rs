use super::{Addressable, Psx};

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Sio {
    mode: u16,
    ctrl: u16,
    baud_rate: u16,
}

impl Sio {
    pub fn new() -> Sio {
        Sio {
            mode: 0,
            ctrl: 0,
            baud_rate: 0,
        }
    }
}

pub fn store<T: Addressable>(psx: &mut Psx, off: u32, val: T) {
    let v = val.as_u16();

    match off {
        8 => psx.sio.mode = v,
        10 => psx.sio.ctrl = v,
        14 => psx.sio.baud_rate = v,
        _ => panic!("Write to sio register {} {:04x}", off, v),
    }
}

pub fn load<T: Addressable>(psx: &mut Psx, off: u32) -> T {
    let v = match off {
        8 => psx.sio.mode,
        10 => psx.sio.ctrl,
        14 => psx.sio.baud_rate,
        _ => panic!("sio read {:?} 0x{:x}", T::width(), off),
    };

    T::from_u16(v)
}
