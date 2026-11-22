//! Bluetooth's part in Quick Share: no files go over it, but phones use it
//! to wake each other up. A phone about to share sends a short Bluetooth
//! advert (service 0xFE2C); devices nearby then make sure they're seen on
//! the network. And a device about to send makes the same advert, so phones
//! nearby (set to be seen) show up on the network to be sent to.

use std::sync::atomic::{AtomicU64, Ordering};

use windows::Devices::Bluetooth::Advertisement::*;
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::DataWriter;

/// The advert's service data, as phones send it when they start sharing.
const SERVICE_DATA: [u8; 24] =
    [0xFC, 0x12, 0x8E, 0x01, 0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xBF, 0x2D, 0x5B, 0xA0, 0xE1, 0xD8, 0x75, 0x24, 0xCA, 0];

/// Quick Share's Bluetooth service (0xFE2C) as a full UUID.
fn service_uuid() -> windows::core::GUID {
    windows::core::GUID::from_u128(0x0000FE2C_0000_1000_8000_00805F9B34FB)
}

/// Our advert, while it's on: dropped, it stops.
pub struct Beacon(BluetoothLEAdvertisementPublisher);

/// Starts the "about to share" advert (to wake phones nearby). None without
/// Bluetooth, or if Windows won't send it.
pub fn beacon() -> Option<Beacon> {
    let publisher = BluetoothLEAdvertisementPublisher::new().ok()?;
    let ad = publisher.Advertisement().ok()?;
    let writer = DataWriter::new().ok()?;
    // Service data, 16-bit UUID (0x16): the UUID little-endian, then the data.
    writer.WriteBytes(&[0x2C, 0xFE]).ok()?;
    writer.WriteBytes(&SERVICE_DATA).ok()?;
    let section = BluetoothLEAdvertisementDataSection::new().ok()?;
    section.SetDataType(0x16).ok()?;
    section.SetData(&writer.DetachBuffer().ok()?).ok()?;
    ad.DataSections().ok()?.Append(&section).ok()?;
    match publisher.Start() {
        Ok(()) => Some(Beacon(publisher)),
        Err(e) => {
            crate::config::log(&format!("quickshare: Bluetooth advert refused: {e}"));
            None
        }
    }
}

impl Drop for Beacon {
    fn drop(&mut self) {
        let _ = self.0.Stop();
    }
}

/// Watching for phones about to share: dropped, it stops.
pub struct Watch(BluetoothLEAdvertisementWatcher);

static LAST: AtomicU64 = AtomicU64::new(0);

/// Calls `heard` when a phone nearby starts sharing (at most every 20 seconds).
pub fn watch(heard: impl Fn() + Send + Sync + 'static) -> Option<Watch> {
    let watcher = BluetoothLEAdvertisementWatcher::new().ok()?;
    watcher.SetScanningMode(BluetoothLEScanningMode::Passive).ok()?;
    let filter = watcher.AdvertisementFilter().ok()?;
    filter.Advertisement().ok()?.ServiceUuids().ok()?.Append(service_uuid()).ok()?;
    watcher
        .Received(&TypedEventHandler::new(move |_, _| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            if now.saturating_sub(LAST.load(Ordering::Relaxed)) >= 20 {
                LAST.store(now, Ordering::Relaxed);
                heard();
            }
            Ok(())
        }))
        .ok()?;
    watcher.Start().ok()?;
    Some(Watch(watcher))
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.0.Stop();
    }
}
