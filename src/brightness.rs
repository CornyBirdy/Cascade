//! Brightness changes on laptop panels, via a blocking WMI event query.
//! On desktops with external monitors the query fails and the thread exits.

use windows::core::{w, Result, BSTR};
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::{VariantClear, VARIANT};
use windows::Win32::System::Wmi::*;

use crate::msg;

pub fn spawn() {
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = watch();
    });
}

unsafe fn watch() -> Result<()> {
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
    let events = services.ExecNotificationQuery(
        &BSTR::from("WQL"),
        &BSTR::from("SELECT * FROM WmiMonitorBrightnessEvent"),
        WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_FORWARD_ONLY,
        None,
    )?;
    loop {
        let mut row = [None::<IWbemClassObject>; 1];
        let mut returned = 0u32;
        events.Next(WBEM_INFINITE.0, &mut row, &mut returned).ok()?;
        let Some(obj) = row[0].take() else { continue };
        let mut v = VARIANT::default();
        if obj.Get(w!("Brightness"), 0, &mut v, None, None).is_ok() {
            let level = v.Anonymous.Anonymous.Anonymous.bVal;
            let _ = VariantClear(&mut v);
            msg::post(msg::BRIGHTNESS, level.min(100) as usize, 0);
        }
    }
}
