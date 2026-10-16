//! Reading what's on screen, for refraction (see `lens`).
//!
//! Windows' desktop duplication hands over each screen as the compositor
//! drew it, as a texture on the GPU, and says when it changed. Cascade keeps
//! a copy of the latest frame on the GPU (a copy there costs next to
//! nothing) and reads back only the thin strips round each pane's edge, and
//! only when the screen did change: with nothing moving, reading it is one
//! call that returns at once.
//!
//! A plain GDI screen copy (BitBlt from the screen) does the same job, but
//! each one makes the compositor copy its frame back from the GPU and wait
//! for it, on its own thread: done many times a second, every window on
//! screen loses frames. `lens` only falls back to it for a screen that
//! can't be duplicated (over Remote Desktop, say, or a rotated screen).

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{E_FAIL, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

/// One screen, duplicated.
struct Output {
    /// Where it is on the desktop (screen px).
    rect: RECT,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    /// A copy of the latest frame, kept on the GPU.
    last: Option<ID3D11Texture2D>,
    /// Counts the frames copied into `last`.
    frame: u64,
    /// Where strips are copied to be read back, and its size (grown as needed).
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

#[derive(Default)]
struct Screens {
    /// Every screen that could be duplicated.
    outputs: Vec<Output>,
    /// `outputs` is set up.
    ready: bool,
    /// After a failure, don't set up again before this.
    retry: Option<Instant>,
    /// How long to wait after the next failure.
    backoff: Duration,
}

thread_local! {
    static SCREENS: RefCell<Screens> = RefCell::new(Screens::default());
}

fn intersect(a: RECT, b: RECT) -> Option<RECT> {
    let r = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (r.left < r.right && r.top < r.bottom).then_some(r)
}

fn contains(r: RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

unsafe fn texture(device: &ID3D11Device, w: u32, h: u32, staging: bool) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
        CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
        ..Default::default()
    };
    let mut tex = None;
    device.CreateTexture2D(&desc, None, Some(&mut tex))?;
    tex.ok_or_else(|| E_FAIL.into())
}

/// Duplicates every screen it can, each on the graphics card it's plugged
/// into (duplication only works there).
fn setup() -> Vec<Output> {
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            a += 1;
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                if let Some(d) = duplicate(&adapter, &output) {
                    out.push(d);
                }
            }
        }
    }
    out
}

unsafe fn duplicate(adapter: &IDXGIAdapter1, output: &IDXGIOutput) -> Option<Output> {
    let desc = output.GetDesc().ok()?;
    // A rotated screen's frames come unrotated; leave those to GDI.
    let upright = desc.Rotation == DXGI_MODE_ROTATION_IDENTITY || desc.Rotation == DXGI_MODE_ROTATION_UNSPECIFIED;
    if !desc.AttachedToDesktop.as_bool() || !upright {
        return None;
    }
    let (mut device, mut context) = (None, None);
    D3D11CreateDevice(
        adapter,
        D3D_DRIVER_TYPE_UNKNOWN,
        Default::default(),
        D3D11_CREATE_DEVICE_SINGLETHREADED,
        None,
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
    )
    .ok()?;
    let (device, context): (ID3D11Device, ID3D11DeviceContext) = (device?, context?);
    let dup = output.cast::<IDXGIOutput1>().ok()?.DuplicateOutput(&device).ok()?;
    Some(Output { rect: desc.DesktopCoordinates, device, context, dup, last: None, frame: 0, staging: None })
}

impl Output {
    fn size(&self) -> (u32, u32) {
        ((self.rect.right - self.rect.left) as u32, (self.rect.bottom - self.rect.top) as u32)
    }

    /// Takes in a new frame, if there is one. An error means the
    /// duplication is lost (the screens changed, a secure desktop, ...).
    unsafe fn take(&mut self) -> Result<()> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match self.dup.AcquireNextFrame(0, &mut info, &mut resource) {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(()),
            Err(e) => return Err(e),
        }
        // No frames, only the pointer moved: the picture is the same.
        let result = if info.AccumulatedFrames > 0 || self.last.is_none() { self.keep(resource) } else { Ok(()) };
        let _ = self.dup.ReleaseFrame();
        result
    }

    unsafe fn keep(&mut self, resource: Option<IDXGIResource>) -> Result<()> {
        let frame: ID3D11Texture2D = resource.ok_or(E_FAIL)?.cast()?;
        let (w, h) = self.size();
        if self.last.is_none() {
            self.last = Some(texture(&self.device, w, h, false)?);
        }
        let Some(last) = self.last.as_ref() else { return Ok(()) };
        let all = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
        self.context.CopySubresourceRegion(last, 0, 0, 0, 0, &frame, 0, Some(&all));
        self.frame += 1;
        Ok(())
    }

    /// Copies `parts` (inside `valid`, the part of `area` on this screen)
    /// into `dst`, a top-down BGRA image of `area`.
    unsafe fn read(&mut self, area: RECT, valid: RECT, parts: &[RECT], dst: &mut [u8]) -> Result<()> {
        let last = self.last.as_ref().ok_or(E_FAIL)?;
        let (aw, ah) = ((area.right - area.left) as u32, (area.bottom - area.top) as u32);
        if dst.len() < (aw * ah * 4) as usize {
            return Err(E_FAIL.into());
        }
        if self.staging.as_ref().is_none_or(|&(_, w, h)| w < aw || h < ah) {
            let (w, h) = self.staging.as_ref().map_or((aw, ah), |&(_, w, h)| (w.max(aw), h.max(ah)));
            self.staging = Some((texture(&self.device, w, h, true)?, w, h));
        }
        let Some((staging, _, _)) = self.staging.as_ref() else { return Err(E_FAIL.into()) };
        let parts: Vec<RECT> = parts.iter().filter_map(|&p| intersect(p, valid)).collect();
        for p in &parts {
            let from = D3D11_BOX {
                left: (p.left - self.rect.left) as u32,
                top: (p.top - self.rect.top) as u32,
                front: 0,
                right: (p.right - self.rect.left) as u32,
                bottom: (p.bottom - self.rect.top) as u32,
                back: 1,
            };
            let (x, y) = ((p.left - area.left) as u32, (p.top - area.top) as u32);
            self.context.CopySubresourceRegion(staging, 0, x, y, 0, last, 0, Some(&from));
        }
        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
        let (src, pitch, stride) = (map.pData as *const u8, map.RowPitch as usize, aw as usize * 4);
        for p in &parts {
            let (x, len) = ((p.left - area.left) as usize * 4, (p.right - p.left) as usize * 4);
            for y in (p.top - area.top) as usize..(p.bottom - area.top) as usize {
                let row = std::slice::from_raw_parts(src.add(y * pitch + x), len);
                dst[y * stride + x..y * stride + x + len].copy_from_slice(row);
            }
        }
        self.context.Unmap(staging, 0);
        Ok(())
    }
}

/// Takes in what changed on screen since the last call (setting up first if
/// need be). Cheap when nothing did.
pub fn poll() {
    SCREENS.with(|s| {
        let mut s = s.borrow_mut();
        if !s.ready {
            if s.retry.is_some_and(|t| Instant::now() < t) {
                return;
            }
            s.outputs = setup();
            s.ready = true;
            if s.outputs.is_empty() {
                fail(&mut s);
                return;
            }
        }
        let mut lost = false;
        for o in &mut s.outputs {
            lost |= unsafe { o.take() }.is_err();
        }
        if lost {
            fail(&mut s);
        } else {
            s.backoff = Duration::ZERO;
        }
    });
}

/// Drops the duplications and tries again in a while (longer each time it
/// keeps failing, up to a minute; GDI fills in meanwhile).
fn fail(s: &mut Screens) {
    s.outputs.clear();
    s.ready = false;
    s.backoff = (s.backoff * 2).clamp(Duration::from_secs(1), Duration::from_secs(60));
    s.retry = Some(Instant::now() + s.backoff);
}

/// The frame count of the duplicated screen under the middle of `area`
/// (screen px), or None if that screen isn't duplicated.
pub fn frame(area: RECT) -> Option<u64> {
    let (x, y) = ((area.left + area.right) / 2, (area.top + area.bottom) / 2);
    SCREENS.with(|s| s.borrow().outputs.iter().find(|o| contains(o.rect, x, y)).map(|o| o.frame))
}

/// Copies `parts` of the screen (screen px, inside `area`) into `dst`, a
/// top-down BGRA image of `area`. Returns the part of `area` that is on the
/// screen read, and that screen's frame count; None if the screen under the
/// middle of `area` isn't duplicated or couldn't be read.
pub fn read(area: RECT, parts: &[RECT], dst: &mut [u8]) -> Option<(RECT, u64)> {
    let (x, y) = ((area.left + area.right) / 2, (area.top + area.bottom) / 2);
    SCREENS.with(|s| {
        let mut s = s.borrow_mut();
        let o = s.outputs.iter_mut().find(|o| contains(o.rect, x, y))?;
        let valid = intersect(area, o.rect)?;
        unsafe { o.read(area, valid, parts, dst) }.ok()?;
        Some((valid, o.frame))
    })
}

/// Lets go of the screens (nothing bends light any more), so the compositor
/// stops handing over frames.
pub fn release() {
    SCREENS.with(|s| *s.borrow_mut() = Screens::default());
}
