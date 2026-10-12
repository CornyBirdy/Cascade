//! Wi-Fi and Bluetooth on/off through the Windows radio API. Runs on its own
//! thread, woken only by toggles and the radios' own state-change events.

use std::sync::mpsc::{channel, Receiver, Sender};

use windows::core::{IInspectable, Result};
use windows::Devices::Radios::{Radio, RadioKind, RadioState};
use windows::Foundation::TypedEventHandler;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::msg;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Wifi,
    Bluetooth,
}

pub enum RadioCmd {
    Toggle(Kind),
    Refresh,
}

/// Radio state as the UI sees it. `None` = no such radio.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Radios {
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
}

impl Radios {
    fn pack(self) -> usize {
        let bits = |o: Option<bool>| match o {
            None => 0,
            Some(false) => 1,
            Some(true) => 3,
        };
        bits(self.wifi) | bits(self.bluetooth) << 2
    }

    pub fn unpack(w: usize) -> Self {
        let get = |b: usize| match b & 3 {
            0 => None,
            3 => Some(true),
            _ => Some(false),
        };
        Self { wifi: get(w), bluetooth: get(w >> 2) }
    }
}

pub fn spawn() -> Sender<RadioCmd> {
    let (tx, rx) = channel();
    let events = tx.clone();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = run(rx, events);
    });
    tx
}

fn kind_of(r: &Radio) -> Option<Kind> {
    match r.Kind().ok()? {
        RadioKind::WiFi => Some(Kind::Wifi),
        RadioKind::Bluetooth => Some(Kind::Bluetooth),
        _ => None,
    }
}

fn run(rx: Receiver<RadioCmd>, tx: Sender<RadioCmd>) -> Result<()> {
    let _ = Radio::RequestAccessAsync()?.get();
    let list = Radio::GetRadiosAsync()?.get()?;
    let mut radios = Vec::new();
    for i in 0..list.Size()? {
        let r = list.GetAt(i)?;
        if kind_of(&r).is_some() {
            let t = tx.clone();
            let _ = r.StateChanged(&TypedEventHandler::<Radio, IInspectable>::new(move |_, _| {
                let _ = t.send(RadioCmd::Refresh);
                Ok(())
            }));
            radios.push(r);
        }
    }
    let report = |radios: &[Radio]| {
        let mut s = Radios::default();
        for r in radios {
            let on = r.State().map(|st| st == RadioState::On).unwrap_or(false);
            match kind_of(r) {
                Some(Kind::Wifi) => s.wifi = Some(on || s.wifi == Some(true)),
                Some(Kind::Bluetooth) => s.bluetooth = Some(on || s.bluetooth == Some(true)),
                None => {}
            }
        }
        msg::post(msg::RADIOS, s.pack(), 0);
    };
    report(&radios);
    while let Ok(cmd) = rx.recv() {
        if let RadioCmd::Toggle(kind) = cmd {
            let any_on = radios
                .iter()
                .filter(|r| kind_of(r) == Some(kind))
                .any(|r| r.State().map(|s| s == RadioState::On).unwrap_or(false));
            let target = if any_on { RadioState::Off } else { RadioState::On };
            for r in radios.iter().filter(|r| kind_of(r) == Some(kind)) {
                let _ = r.SetStateAsync(target).and_then(|op| op.get());
            }
        }
        while rx.try_recv().is_ok() {}
        report(&radios);
    }
    Ok(())
}
