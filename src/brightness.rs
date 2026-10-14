//! Screen brightness.
//!
//! * Watching: laptop panels report changes through a blocking WMI event
//!   query (on desktops it fails and the thread exits), and through Windows'
//!   own brightness power setting, which changes the moment the keys are
//!   pressed while the WMI event can take a while to arrive.
//! * Setting: laptop panels via WMI, external monitors via DDC/CI. Runs on a
//!   worker thread because DDC/CI is slow (tens of ms per monitor).

use std::sync::mpsc::{channel, Sender};

use windows::core::{w, Result, BOOL, BSTR, GUID};
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::System::Com::*;
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VariantClear, VARENUM, VARIANT, VT_I4, VT_UI1};
use windows::Win32::System::Wmi::*;

use crate::msg;

unsafe fn connect() -> Result<IWbemServices> {
    let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
    let services = locator.ConnectServer(
        &BSTR::from("ROOT\\WMI"),
        &BSTR::new(),
        &BSTR::new(),
        &BSTR::new(),
        0,
        &BSTR::new(),
        None,
    )?;
    CoSetProxyBlanket(
        &services,
        RPC_C_AUTHN_WINNT,
        RPC_C_AUTHZ_NONE,
        None,
        RPC_C_AUTHN_LEVEL_CALL,
        RPC_C_IMP_LEVEL_IMPERSONATE,
        None,
        EOAC_NONE,
    )?;
    Ok(services)
}

// ------------------------------------------------------------- watching --

pub fn spawn_watcher() {
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = watch();
    });
}

unsafe fn watch() -> Result<()> {
    let services = connect()?;
    let events = services.ExecNotificationQuery(
        &BSTR::from("WQL"),
        &BSTR::from("SELECT * FROM WmiMonitorBrightnessEvent"),
        WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_FORWARD_ONLY,
        None,
    )?;
    loop {
        let mut row = [None::<IWbemClassObject>; 1];
        let mut returned = 0u32;
        events.Next(WBEM_INFINITE, &mut row, &mut returned).ok()?;
        let Some(obj) = row[0].take() else { continue };
        let mut v = VARIANT::default();
        if obj.Get(w!("Brightness"), 0, &mut v, None, None).is_ok() {
            let level = v.Anonymous.Anonymous.Anonymous.bVal;
            let _ = VariantClear(&mut v);
            msg::post(msg::BRIGHTNESS, level.min(100) as usize, 0);
        }
    }
}

/// GUID_VIDEO_CURRENT_MONITOR_BRIGHTNESS: the current brightness of the
/// built-in screen, as a percentage.
const CURRENT_BRIGHTNESS: GUID = GUID::from_u128(0x8ffee2c6_2d01_46be_adb9_398addc5b4ff);
const PBT_POWERSETTINGCHANGE: u32 = 0x8013;

/// Asks Windows to tell `hwnd` (with `WM_POWERBROADCAST`) whenever the
/// brightness setting changes. Read the level with `power_setting_level`.
pub fn listen(hwnd: HWND) {
    use windows::Win32::System::Power::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let _ = RegisterPowerSettingNotification(HANDLE(hwnd.0), &CURRENT_BRIGHTNESS, DEVICE_NOTIFY_WINDOW_HANDLE);
    }
}

/// The brightness in a `WM_POWERBROADCAST`, if that is what it is about.
///
/// # Safety
/// `wp` and `lp` must be those of a `WM_POWERBROADCAST` message.
pub unsafe fn power_setting_level(wp: WPARAM, lp: LPARAM) -> Option<u32> {
    use windows::Win32::System::Power::POWERBROADCAST_SETTING;
    if wp.0 as u32 != PBT_POWERSETTINGCHANGE || lp.0 == 0 {
        return None;
    }
    let s = &*(lp.0 as *const POWERBROADCAST_SETTING);
    if s.PowerSetting != CURRENT_BRIGHTNESS || s.DataLength < 4 {
        return None;
    }
    let level = std::ptr::read_unaligned(s.Data.as_ptr() as *const u32);
    Some(level.min(100))
}

// -------------------------------------------------------------- setting --

/// Starts the setter. It first reports the current level (or that brightness
/// can't be controlled) with `msg::BRIGHTNESS_LEVEL`.
pub fn spawn_setter() -> Sender<u32> {
    let (tx, rx) = channel::<u32>();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let wmi = connect().ok();
        let current = wmi.as_ref().and_then(|s| wmi_get(s)).or_else(|| ddc_get());
        msg::post(msg::BRIGHTNESS_LEVEL, current.unwrap_or(0) as usize, current.is_some() as isize);
        while let Ok(mut level) = rx.recv() {
            // A slider drag queues many values; only the latest matters.
            while let Ok(newer) = rx.try_recv() {
                level = newer;
            }
            let level = level.min(100);
            if let Some(s) = &wmi {
                wmi_set(s, level);
            }
            ddc_set(level);
        }
    });
    tx
}

unsafe fn wmi_get(services: &IWbemServices) -> Option<u32> {
    let rows = services
        .ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from("SELECT CurrentBrightness FROM WmiMonitorBrightness"),
            WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_FORWARD_ONLY,
            None,
        )
        .ok()?;
    let mut row = [None::<IWbemClassObject>; 1];
    let mut returned = 0u32;
    rows.Next(WBEM_INFINITE, &mut row, &mut returned).ok().ok()?;
    let obj = row[0].take()?;
    let mut v = VARIANT::default();
    obj.Get(w!("CurrentBrightness"), 0, &mut v, None, None).ok()?;
    let level = v.Anonymous.Anonymous.Anonymous.bVal as u32;
    let _ = VariantClear(&mut v);
    Some(level)
}

unsafe fn wmi_set(services: &IWbemServices, level: u32) {
    let mut class: Option<IWbemClassObject> = None;
    if services
        .GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut class), None)
        .is_err()
    {
        return;
    }
    let Some(class) = class else { return };
    let mut input_def: Option<IWbemClassObject> = None;
    if class.GetMethod(w!("WmiSetBrightness"), 0, &mut input_def, std::ptr::null_mut()).is_err() {
        return;
    }
    let Some(input_def) = input_def else { return };
    let Ok(rows) = services.ExecQuery(
        &BSTR::from("WQL"),
        &BSTR::from("SELECT * FROM WmiMonitorBrightnessMethods"),
        WBEM_GENERIC_FLAG_TYPE(0),
        None,
    ) else {
        return;
    };
    loop {
        let mut row = [None::<IWbemClassObject>; 1];
        let mut returned = 0u32;
        if rows.Next(WBEM_INFINITE, &mut row, &mut returned).is_err() || returned == 0 {
            break;
        }
        let Some(obj) = row[0].take() else { break };
        let mut path = VARIANT::default();
        if obj.Get(w!("__RELPATH"), 0, &mut path, None, None).is_err() {
            continue;
        }
        let relpath = BSTR::from(path.Anonymous.Anonymous.Anonymous.bstrVal.to_string());
        let _ = VariantClear(&mut path);
        let Ok(input) = input_def.SpawnInstance(0) else { continue };
        let mut b = VARIANT::default();
        (*b.Anonymous.Anonymous).vt = VARENUM(VT_UI1.0);
        (*b.Anonymous.Anonymous).Anonymous.bVal = level as u8;
        let mut t = VARIANT::default();
        (*t.Anonymous.Anonymous).vt = VARENUM(VT_I4.0);
        (*t.Anonymous.Anonymous).Anonymous.lVal = 0;
        let _ = input.Put(w!("Brightness"), 0, &b, 0);
        let _ = input.Put(w!("Timeout"), 0, &t, 0);
        let _ = services.ExecMethod(
            &relpath,
            &BSTR::from("WmiSetBrightness"),
            WBEM_GENERIC_FLAG_TYPE(0),
            None,
            Some(&input),
            None,
            None,
        );
    }
}

/// Runs `f` on every physical monitor that answers DDC/CI.
unsafe fn for_each_physical(mut f: impl FnMut(&PHYSICAL_MONITOR)) {
    unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        (*(out.0 as *mut Vec<HMONITOR>)).push(h);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut handles as *mut _ as isize));
    for h in handles {
        let mut n = 0u32;
        if GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n).is_err() || n == 0 {
            continue;
        }
        let mut list = vec![PHYSICAL_MONITOR::default(); n as usize];
        if GetPhysicalMonitorsFromHMONITOR(h, &mut list).is_ok() {
            for m in &list {
                f(m);
            }
            let _ = DestroyPhysicalMonitors(&list);
        }
    }
}

unsafe fn ddc_get() -> Option<u32> {
    let mut found = None;
    for_each_physical(|m| {
        if found.is_some() {
            return;
        }
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        if GetMonitorBrightness(m.hPhysicalMonitor, &mut min, &mut cur, &mut max) != 0 && max > min {
            found = Some((cur.saturating_sub(min)) * 100 / (max - min));
        }
    });
    found
}

unsafe fn ddc_set(level: u32) {
    for_each_physical(|m| {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        if GetMonitorBrightness(m.hPhysicalMonitor, &mut min, &mut cur, &mut max) != 0 && max > min {
            let _ = SetMonitorBrightness(m.hPhysicalMonitor, min + (max - min) * level / 100);
        }
    });
}
