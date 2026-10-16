//! Dropping files on the taskbar: drag a file onto an app's button and it
//! opens with that app. Holding it over an app with windows brings its
//! window up first, the way Windows' taskbar does, so you can drop into it.
//!
//! Each taskbar window is an OLE drop target (`register`); OLE calls it on
//! the UI thread while a drag passes over, and it hands the work to the app
//! (see `ui::drag_over` and `ui::drop_files`).

use windows::core::{implement, Ref, Result};
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

#[implement(IDropTarget)]
struct Target {
    /// Which taskbar it is (0: the main one).
    dock: usize,
}

fn effect(out: *mut DROPEFFECT, accept: bool) {
    if let Some(e) = unsafe { out.as_mut() } {
        *e = if accept { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, _: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        effect(out, crate::ui::drag_over(self.dock, pt.x, pt.y));
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        effect(out, crate::ui::drag_over(self.dock, pt.x, pt.y));
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        crate::ui::drag_leave(self.dock);
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, pt: &POINTL, out: *mut DROPEFFECT) -> Result<()> {
        let files = data.ok().map(files).unwrap_or_default();
        let taken = !files.is_empty() && crate::ui::drop_files(self.dock, pt.x, pt.y, files);
        effect(out, taken);
        Ok(())
    }
}

/// The paths of the files being dropped.
fn files(data: &IDataObject) -> Vec<String> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let mut out = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else { return out };
        let drop = HDROP(medium.u.hGlobal.0);
        let n = DragQueryFileW(drop, u32::MAX, None);
        for i in 0..n.min(64) {
            let mut buf = [0u16; 1024];
            let len = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            if len > 0 {
                out.push(String::from_utf16_lossy(&buf[..len.min(buf.len())]));
            }
        }
        ReleaseStgMedium(&mut medium);
    }
    out
}

/// Makes taskbar window `hwnd` (taskbar `dock`) take dropped files.
pub fn register(hwnd: HWND, dock: usize) {
    let target: IDropTarget = Target { dock }.into();
    unsafe {
        let _ = RegisterDragDrop(hwnd, &target);
    }
}

/// Stops `hwnd` taking dropped files (before it's destroyed).
pub fn revoke(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}
