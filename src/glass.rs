//! Liquid Glass on the GPU: what's behind each pane, bent by its curved edge
//! (see `glassmath` for the optics), softened a little, and lit along the rim,
//! redrawn whenever what's behind it changes.
//!
//! A browser can do this cheaply because it draws the whole page itself, so
//! it always has the pixels behind a glass element. On the desktop the
//! pixels behind a pane belong to other apps, and only Windows' compositor
//! (DWM) has them. So:
//!
//! 1. **What's behind.** DXGI Desktop Duplication hands over each screen as
//!    DWM composed it, on the GPU, only when it changed, and says which parts
//!    changed. The glass windows are left out of it (display affinity
//!    `WDA_EXCLUDEFROMCAPTURE`), so the copy shows what's under them, never
//!    the glass itself, and the glass can't feed back into itself. (The same
//!    flag leaves the glass out of screenshots and screen sharing: they show
//!    each panel's own drawing, its tint and icons, over what's behind.)
//! 2. **Cascade's own drawing, taken back out.** A panel's icons, text and
//!    tint are in its own window over the glass, which screenshots should
//!    keep, so it isn't left out of the copy. Each frame a panel presents is
//!    also handed to the GPU (`presented`), and since DWM laid it over what's
//!    behind as `shown = drawn + (1 - alpha) * behind`, what's behind is
//!    worked back out wherever the drawing is see-through. Under icons and
//!    text, and while a panel moves (when the copy and the drawing are a
//!    frame apart), the last clean pixels are kept instead. That gives a
//!    clean picture of what's behind every pane, per screen.
//! 3. **The glass.** Per pane: a two-pass Gaussian blur of the clean picture
//!    around it (light; holes under icons fill in from around them), then
//!    one pass that, for each pixel, measures the distance to the pane's
//!    rounded rect, reads the bend for that depth from a table (Snell's law
//!    on a squircle edge), reads what's behind from that much further in
//!    (red a little further than blue), softer near the rim, and adds the
//!    light caught by the curve. It's drawn into a DirectComposition swap
//!    chain in a window just under the panel's own.
//!
//! All of it stays on the GPU: nothing is read back, and with nothing moving
//! behind the panes nothing is redrawn. It's all on the UI thread; any
//! failure (no Direct3D 11, no duplication, an old Windows) makes `Pane::new`
//! or `Pane::show` say so, and the panel falls back to DWM's blur and the
//! CPU ring (see `lens`).

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::{s, w, Interface, BOOL, HRESULT, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::glassmath::{self, Look, LUT_LEN};

/// How long a panel's window keeps still before what's behind it is worked
/// out from the screen copy again (the copy runs a frame or two behind).
const SETTLE: Duration = Duration::from_millis(90);
/// How long a screen's duplication is kept with no glass on it.
const IDLE: Duration = Duration::from_secs(3);
/// How long to wait before trying a screen's duplication again after it failed.
const RETRY: Duration = Duration::from_millis(750);
/// How long a pane's own present may show up in its screen's copy as a
/// change (the glass is left out of the copy, but Windows may still say
/// the part it covers changed).
const OWN: Duration = Duration::from_millis(100);
/// How often the screens are looked for again, at most.
const REENUMERATE: Duration = Duration::from_secs(1);
/// How many times in a row a pane asks its panel to draw again before
/// giving up on getting the drawing (and leaving what's under it unknown).
const ASKS: u8 = 8;
/// Parts of a screen to work out again, past which they're joined into one.
const PENDING: usize = 64;
/// Most see-through a panel's drawing can be over what's behind and still
/// let it be worked out (more solid than this, the last clean pixels stay).
const SOLID: f32 = 0.75;

const VS: usize = 0;
const PS_COPY: usize = 1;
const PS_UNCOMP: usize = 2;
const PS_BLUR: usize = 3;
const PS_GLASS: usize = 4;

const SHADERS: &str = r#"
cbuffer C : register(b0) {
    float4 quad;   // the target pixels drawn: x0, y0, x1, y1
    float4 target; // the target's size
    float4 map;    // source px = target px + map.xy; map.zw: the source's size
    float4 p0;
    float4 p1;
    float4 p2;
    float4 p3;
    float4 p4;
    float4 rects[16];
    float4 lut[16];
};
Texture2D t0 : register(t0);
Texture2D t1 : register(t1);
Texture2D t2 : register(t2);
SamplerState lin : register(s0);

float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 c = float2(id & 1, id >> 1);
    float2 px = lerp(quad.xy, quad.zw, c);
    return float4(px.x / target.x * 2 - 1, 1 - px.y / target.y * 2, 0, 1);
}

bool within(float2 p, float n) {
    [loop] for (int i = 0; i < (int)n; i++) {
        float4 r = rects[i];
        if (p.x >= r.x && p.y >= r.y && p.x < r.z && p.y < r.w) return true;
    }
    return false;
}

// The screen as it is, where none of Cascade's panels is drawn.
float4 ps_copy(float4 pos : SV_Position) : SV_Target {
    if (within(pos.xy, p0.x)) discard;
    return float4(t0.Load(int3(pos.xy, 0)).rgb, 1);
}

// What's behind a panel's own drawing: t0 the screen, t1 the drawing as last
// presented, t2 as presented before that. p0.xy: the drawing's corner,
// p0.z: its window's fade, p0.w: 1 where pixels that just changed are left
// alone (the copy may not have caught up with them); p1.x: rects to leave.
float4 ps_uncomp(float4 pos : SV_Position) : SV_Target {
    if (within(pos.xy, p1.x)) discard;
    int3 at = int3(pos.xy - p0.xy, 0);
    float4 now = t1.Load(at) * p0.z;
    if (p0.w > 0.5) {
        float4 d = abs(now - t2.Load(at) * p0.z);
        if (max(max(d.r, d.g), max(d.b, d.a)) > 3.0 / 255.0) discard;
    }
    if (now.a > p1.y) discard;
    float3 f = t0.Load(int3(pos.xy, 0)).rgb;
    return float4(saturate((f - now.rgb) / (1.0 - now.a)), 1);
}

// Half a Gaussian blur, weighted by how much of each pixel is known (alpha),
// two taps per sample. p0.xy: the step along the blur, p0.z: sigma,
// p0.w: taps either side.
float4 ps_blur(float4 pos : SV_Position) : SV_Target {
    float2 at = pos.xy + map.xy;
    float2 inv = 1.0 / map.zw;
    float s2 = 2.0 * p0.z * p0.z;
    float4 sum = t0.SampleLevel(lin, at * inv, 0);
    float total = 1;
    [loop] for (int i = 1; i <= (int)p0.w; i += 2) {
        float w0 = exp(-(i * i) / s2);
        float w1 = exp(-((i + 1) * (i + 1)) / s2);
        float w = w0 + w1;
        float2 o = p0.xy * (i + w1 / w);
        sum += (t0.SampleLevel(lin, (at + o) * inv, 0) + t0.SampleLevel(lin, (at - o) * inv, 0)) * w;
        total += 2 * w;
    }
    return sum / total;
}

// The glass. t0: what's behind, blurred, with mips; it starts p3.y px up
// and left of the target's corner. p0: the pane's centre and half size;
// p1: corner radius, curved band, bend at the rim (px), colour split;
// p2: extra softness at the rim (mips), light on the lit side, the far side,
// all round; p3.x: vividness, p3.z: the glass's thickness against its band.
float sd_pane(float2 p) {
    float2 q = abs(p - p0.xy) - (p0.zw - p1.x);
    return length(max(q, 0)) + min(max(q.x, q.y), 0) - p1.x;
}

float bend_at(float x) {
    float f = saturate(x) * 63.0;
    int i = min((int)f, 62);
    float a = lut[i >> 2][i & 3];
    float b = lut[(i + 1) >> 2][(i + 1) & 3];
    return lerp(a, b, f - i);
}

float slope(float x) {
    float u = 1 - clamp(x, 1e-4, 1);
    return u * u * u * pow(max(1 - u * u * u * u, 1e-6), -0.75);
}

float4 behind(float2 at, float lod) {
    float2 uv = (at + p3.y) / map.zw;
    float4 acc = t0.SampleLevel(lin, uv, lod);
    // Holes (under icons, or never seen) fill in from coarser and coarser
    // copies, front to back.
    [unroll] for (int i = 1; i <= 6; i++) {
        if (acc.a > 0.98) break;
        acc += t0.SampleLevel(lin, uv, lod + i) * (1 - acc.a);
    }
    return acc;
}

float4 ps_glass(float4 pos : SV_Position) : SV_Target {
    float2 p = pos.xy;
    float sd = sd_pane(p);
    float cover = saturate(0.5 - sd);
    if (cover <= 0) return 0;
    float band = max(min(p1.y, min(p0.z, p0.w)), 1);
    float x = saturate(-sd / band);
    float h = max(band * 0.25, 1);
    float2 g = float2(sd_pane(p + float2(h, 0)) - sd_pane(p - float2(h, 0)),
                      sd_pane(p + float2(0, h)) - sd_pane(p - float2(0, h))) / (2 * h);
    float d = bend_at(x) * p1.z * band;
    float lod = p2.x * (1 - x) * (1 - x);
    float4 r = behind(p - g * d * (1 + p1.w), lod);
    float4 gr = behind(p - g * d, lod);
    float4 b = behind(p - g * d * (1 - p1.w), lod);
    float3 c = float3(r.r / max(r.a, 1e-4), gr.g / max(gr.a, 1e-4), b.b / max(b.a, 1e-4));
    float len = length(g);
    float light = 0;
    if (len > 1e-4 && x < 1) {
        float tilt = min(p3.z * slope(x), 60) * len;
        float lean = 1 - rsqrt(1 + tilt * tilt);
        float facing = dot(g, float2(-0.6, -0.8)) / len;
        light = lean * lean * lean
            * (p2.y * pow(max(facing, 0), 1.5) + p2.z * pow(max(-facing, 0), 1.5) + p2.w);
    }
    float l = dot(c, float3(0.2126, 0.7152, 0.0722));
    c = saturate(l + (c - l) * p3.x + light);
    return float4(c * cover, cover);
}
"#;

/// The constants every pass reads (`C` in the shaders).
#[repr(C)]
#[derive(Clone, Copy)]
struct Consts {
    quad: [f32; 4],
    target: [f32; 4],
    map: [f32; 4],
    p: [[f32; 4]; 5],
    rects: [[f32; 4]; 16],
    lut: [[f32; 4]; 16],
}

impl Default for Consts {
    fn default() -> Self {
        Self {
            quad: [0.0; 4],
            target: [0.0; 4],
            map: [0.0; 4],
            p: [[0.0; 4]; 5],
            rects: [[0.0; 4]; 16],
            lut: [[0.0; 4]; 16],
        }
    }
}

type Compile = unsafe extern "system" fn(
    *const c_void,
    usize,
    PCSTR,
    *const c_void,
    *mut c_void,
    PCSTR,
    PCSTR,
    u32,
    u32,
    *mut *mut c_void,
    *mut *mut c_void,
) -> HRESULT;

/// A texture and the views the passes use it through.
struct Tex {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: Option<ID3D11RenderTargetView>,
    w: u32,
    h: u32,
}

/// One graphics card: the device everything on its screens is drawn with.
struct Gpu {
    luid: i64,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    factory: IDXGIFactory2,
    dcomp: IDCompositionDevice,
    vs: ID3D11VertexShader,
    ps: Vec<ID3D11PixelShader>,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    raster: ID3D11RasterizerState,
}

/// One screen, duplicated.
struct Output {
    name: [u16; 32],
    /// Where it is on the desktop (px).
    rect: RECT,
    gpu: usize,
    output: IDXGIOutput1,
    dup: Option<IDXGIOutputDuplication>,
    /// The screen as last duplicated, and the clean picture of what's behind
    /// Cascade's panels on it (alpha: whether a pixel is known).
    frame: Option<Tex>,
    clean: Option<Tex>,
    /// Whether the next frame must be copied whole (the duplication is new).
    fresh: bool,
    /// Parts of the screen (its own px) to work out again in `clean`.
    pending: Vec<RECT>,
    /// Where panes on it presented lately (its px), and when: a change the
    /// copy reports there is the pane's own, not what's behind it.
    own: Vec<(RECT, Instant)>,
    retry_at: Option<Instant>,
    idle_since: Option<Instant>,
    /// It can't be duplicated (turned, or Windows says no): its panes fall back.
    unsupported: bool,
}

/// A panel's own window, as last presented, on one GPU.
struct Content {
    hwnd: HWND,
    gpu: usize,
    /// The drawing as last presented, and as presented before that.
    now: Option<Tex>,
    before: Option<Tex>,
    rect: RECT,
    alpha: u8,
    uploaded: bool,
    /// When its window last moved, changed size or fade, or came on.
    moved_at: Instant,
    /// When it was last presented.
    presented_at: Instant,
    /// The screen it has covered since it started moving.
    swept: RECT,
    settled: bool,
    /// It presented since what's behind it was last worked out in full.
    remerge: bool,
    /// How many times in a row a pane asked for its drawing (see `Shown`).
    asks: u8,
    /// A pane on screen belongs to it (else it's left alone: it's part of
    /// what's behind other panes, if anything).
    active: bool,
}

/// What a pane draws with, on its GPU.
struct PaneGpu {
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    chain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    chain_w: u32,
    chain_h: u32,
    /// The blur's two passes; the second has mips.
    t1: Option<Tex>,
    t2: Option<Tex>,
}

struct PaneState {
    id: u32,
    hwnd: HWND,
    look: Look,
    rect: RECT,
    /// The rounded rect it's cut to (window px, may reach past the window)
    /// and its radius, else the whole window with corners of `radius`.
    shape: Option<(RECT, i32)>,
    radius: i32,
    content: HWND,
    visible: bool,
    dirty: bool,
    gpu: Option<usize>,
    output: Option<usize>,
    res: Option<PaneGpu>,
}

#[derive(Default)]
struct State {
    /// Whether it can work here at all (None until first asked).
    usable: Option<bool>,
    compile: Option<Compile>,
    code: Vec<Vec<u8>>,
    lut: [[f32; 4]; 16],
    gpus: Vec<Gpu>,
    outputs: Vec<Output>,
    contents: Vec<Content>,
    panes: Vec<PaneState>,
    next_id: u32,
    /// When the screens were last looked for.
    enumerated_at: Option<Instant>,
    /// Problems already written to the log, so each is written once.
    logged: Vec<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Whether the GPU glass works here (set once it has been tried; see `ready`).
static READY: AtomicBool = AtomicBool::new(false);
/// Glass on screen stopped working (see `fell_back`).
static FELL: AtomicBool = AtomicBool::new(false);

/// Whether the GPU glass is known to work here, without trying it.
pub fn ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Runs `f` on the state, or not at all if it's in use (a call from inside
/// another, which nothing should make).
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.try_with(|s| s.try_borrow_mut().ok().map(|mut s| f(&mut s))).ok().flatten()
}

impl State {
    fn log(&mut self, what: &str) {
        if !self.logged.iter().any(|l| l == what) {
            self.logged.push(what.to_string());
            crate::config::log(&format!("liquid glass: {what}"));
        }
    }
}

/// Whether the GPU glass can work on this PC (sets it up the first time).
pub fn usable() -> bool {
    with(|s| {
        if s.usable.is_none() {
            let ok = init(s);
            if let Err(e) = &ok {
                s.log(&format!("not available ({e}), using DWM's blur instead"));
            }
            s.usable = Some(ok.is_ok());
            READY.store(ok.is_ok(), Ordering::Relaxed);
        }
        s.usable == Some(true)
    })
    .unwrap_or(false)
}

/// Whether glass on screen stopped working since last asked (the graphics
/// card was lost, or a screen can't be copied any more). Its panels only
/// fall back to DWM's blur when they're placed again, so redraw them all.
pub fn fell_back() -> bool {
    FELL.swap(false, Ordering::Relaxed)
}

/// Whether any pane is on screen or a screen is still being duplicated (so
/// `tick` needs calling).
pub fn active() -> bool {
    with(|s| s.panes.iter().any(|p| p.visible) || s.outputs.iter().any(|o| o.dup.is_some())).unwrap_or(false)
}

/// Windows 10 2004 or later: before it, a window left out of screen copies
/// shows in them black instead, which the glass would read back.
fn new_enough() -> bool {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    crate::winset::reg_text(HKEY_LOCAL_MACHINE, "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion", "CurrentBuild")
        .and_then(|b| b.parse::<u32>().ok())
        .is_some_and(|b| b >= 19041)
}

fn init(s: &mut State) -> Result<(), String> {
    if !new_enough() {
        return Err("needs Windows 10 2004 or later".into());
    }
    // d3dcompiler_47 comes with Windows; looked up at run time, so a PC
    // without it only loses the glass.
    let compile = unsafe {
        let dll = LoadLibraryW(w!("d3dcompiler_47.dll")).map_err(|e| format!("no shader compiler: {e}"))?;
        let f = GetProcAddress(dll, s!("D3DCompile")).ok_or("no D3DCompile")?;
        // SAFETY: it is d3dcompiler_47's D3DCompile, which has this signature.
        std::mem::transmute::<unsafe extern "system" fn() -> isize, Compile>(f)
    };
    s.compile = Some(compile);
    let entries = [
        ("vs", "vs_5_0"),
        ("ps_copy", "ps_5_0"),
        ("ps_uncomp", "ps_5_0"),
        ("ps_blur", "ps_5_0"),
        ("ps_glass", "ps_5_0"),
    ];
    for (entry, profile) in entries {
        s.code.push(build(compile, entry, profile)?);
    }
    let table = glassmath::profile();
    for (i, v) in table.iter().enumerate().take(LUT_LEN) {
        s.lut[i / 4][i % 4] = *v;
    }
    enumerate(s)?;
    if s.outputs.is_empty() {
        return Err("no screens to duplicate".into());
    }
    register_class();
    Ok(())
}

/// Compiles one entry point of `SHADERS`.
fn build(compile: Compile, entry: &str, profile: &str) -> Result<Vec<u8>, String> {
    let entry_c = format!("{entry}\0");
    let profile_c = format!("{profile}\0");
    let mut code: *mut c_void = std::ptr::null_mut();
    let mut errors: *mut c_void = std::ptr::null_mut();
    // D3DCOMPILE_OPTIMIZATION_LEVEL3
    let flags = 1 << 15;
    let hr = unsafe {
        compile(
            SHADERS.as_ptr() as *const c_void,
            SHADERS.len(),
            s!("glass"),
            std::ptr::null(),
            std::ptr::null_mut(),
            PCSTR(entry_c.as_ptr()),
            PCSTR(profile_c.as_ptr()),
            flags,
            0,
            &mut code,
            &mut errors,
        )
    };
    let errors = (!errors.is_null()).then(|| unsafe { ID3DBlob::from_raw(errors) });
    if hr.is_err() || code.is_null() {
        let text = errors
            .map(|b| unsafe {
                let bytes = std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize());
                String::from_utf8_lossy(bytes).trim().to_string()
            })
            .unwrap_or_default();
        if !code.is_null() {
            drop(unsafe { ID3DBlob::from_raw(code) });
        }
        return Err(format!("shader {entry} didn't compile: {hr:?} {text}"));
    }
    let blob = unsafe { ID3DBlob::from_raw(code) };
    let bytes = unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) };
    Ok(bytes.to_vec())
}

/// Finds every screen and the graphics card it's on (each card's device is
/// made the first time one of its screens is seen).
fn enumerate(s: &mut State) -> Result<(), String> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("no DXGI: {e}"))?;
    let mut seen = Vec::new();
    for a in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else { break };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        // The Basic Render Driver (software) can't duplicate anything worth it.
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let luid = ((desc.AdapterLuid.HighPart as i64) << 32) | desc.AdapterLuid.LowPart as i64;
        for o in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else { break };
            let Ok(od) = (unsafe { output.GetDesc() }) else { continue };
            if !od.AttachedToDesktop.as_bool() {
                continue;
            }
            let Ok(output1) = output.cast::<IDXGIOutput1>() else { continue };
            let gpu = match s.gpus.iter().position(|g| g.luid == luid) {
                Some(g) => g,
                None => match make_gpu(&s.code, &adapter, luid) {
                    Ok(g) => {
                        s.gpus.push(g);
                        s.gpus.len() - 1
                    }
                    Err(e) => {
                        s.log(&format!("graphics card skipped: {e}"));
                        continue;
                    }
                },
            };
            seen.push(od.DeviceName);
            let turned = od.Rotation != DXGI_MODE_ROTATION_IDENTITY && od.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED;
            match s.outputs.iter_mut().find(|x| x.name == od.DeviceName) {
                Some(x) if x.gpu == gpu && x.rect == od.DesktopCoordinates => {
                    x.output = output1;
                    x.unsupported = turned;
                }
                Some(x) => {
                    // Moved, resized or now on another card: start it over.
                    *x = new_output(od.DeviceName, od.DesktopCoordinates, gpu, output1, turned);
                }
                None => s.outputs.push(new_output(od.DeviceName, od.DesktopCoordinates, gpu, output1, turned)),
            }
        }
    }
    s.outputs.retain(|o| seen.contains(&o.name));
    s.enumerated_at = Some(Instant::now());
    // Indexes may have moved: each pane finds its screen again (one now on
    // another card waits for its panel to show it there).
    for p in &mut s.panes {
        let found = screen_at(&s.outputs, p.rect);
        p.output = found.filter(|&o| Some(s.outputs[o].gpu) == p.gpu);
        p.dirty = true;
    }
    Ok(())
}

fn new_output(name: [u16; 32], rect: RECT, gpu: usize, output: IDXGIOutput1, unsupported: bool) -> Output {
    Output {
        name,
        rect,
        gpu,
        output,
        dup: None,
        frame: None,
        clean: None,
        fresh: true,
        pending: Vec::new(),
        own: Vec::new(),
        retry_at: None,
        idle_since: None,
        unsupported,
    }
}

fn make_gpu(code: &[Vec<u8>], adapter: &IDXGIAdapter1, luid: i64) -> Result<Gpu, String> {
    unsafe {
        let mut device = None;
        let mut ctx = None;
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(|e| format!("no Direct3D 11 device: {e}"))?;
        let (device, ctx): (ID3D11Device, ID3D11DeviceContext) = (device.ok_or("no device")?, ctx.ok_or("no context")?);
        let dxgi: IDXGIDevice = device.cast().map_err(|e| e.to_string())?;
        let factory: IDXGIFactory2 = adapter.GetParent().map_err(|e| e.to_string())?;
        let dcomp: IDCompositionDevice =
            DCompositionCreateDevice(&dxgi).map_err(|e| format!("no DirectComposition: {e}"))?;
        let mut vs = None;
        device.CreateVertexShader(&code[VS], None, Some(&mut vs)).map_err(|e| e.to_string())?;
        let mut ps = Vec::new();
        for c in &code[PS_COPY..=PS_GLASS] {
            let mut p = None;
            device.CreatePixelShader(c, None, Some(&mut p)).map_err(|e| e.to_string())?;
            ps.push(p.ok_or("no pixel shader")?);
        }
        let mut cb = None;
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<Consts>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        device.CreateBuffer(&desc, None, Some(&mut cb)).map_err(|e| e.to_string())?;
        let mut sampler = None;
        let sd = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_BORDER,
            AddressV: D3D11_TEXTURE_ADDRESS_BORDER,
            AddressW: D3D11_TEXTURE_ADDRESS_BORDER,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            // Off the screen nothing is known (alpha 0), so the blur leaves it out.
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
        };
        device.CreateSamplerState(&sd, Some(&mut sampler)).map_err(|e| e.to_string())?;
        let mut raster = None;
        let rd = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ..Default::default()
        };
        device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(|e| e.to_string())?;
        Ok(Gpu {
            luid,
            device,
            ctx,
            factory,
            dcomp,
            vs: vs.ok_or("no vertex shader")?,
            ps,
            cb: cb.ok_or("no constants")?,
            sampler: sampler.ok_or("no sampler")?,
            raster: raster.ok_or("no rasterizer state")?,
        })
    }
}

/// A texture of `w` x `h`, viewed by the shaders and, if `target`, drawn
/// into; `mips` levels (0 for the full chain, made by `GenerateMips`).
fn texture(gpu: &Gpu, w: u32, h: u32, format: DXGI_FORMAT, target: bool, mips: u32) -> Option<Tex> {
    let mut bind = D3D11_BIND_SHADER_RESOURCE.0 as u32;
    if target {
        bind |= D3D11_BIND_RENDER_TARGET.0 as u32;
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w.max(1),
        Height: h.max(1),
        MipLevels: mips,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: if mips != 1 { D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32 } else { 0 },
    };
    unsafe {
        let mut tex = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)).ok()?;
        let tex = tex?;
        let mut srv = None;
        gpu.device.CreateShaderResourceView(&tex, None, Some(&mut srv)).ok()?;
        let rtv = if target {
            let mut rtv = None;
            gpu.device.CreateRenderTargetView(&tex, None, Some(&mut rtv)).ok()?;
            rtv
        } else {
            None
        };
        Some(Tex { tex, srv: srv?, rtv, w: desc.Width, h: desc.Height })
    }
}

/// Rounds a size up, so a pane that grows a little each frame doesn't need
/// new textures each frame.
fn roomy(v: i32) -> u32 {
    ((v.max(1) as u32).div_ceil(64)) * 64
}

fn intersect(a: RECT, b: RECT) -> Option<RECT> {
    let r = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

/// Both rects, ignoring an empty one.
fn join(a: RECT, b: RECT) -> RECT {
    let empty = |r: RECT| r.right <= r.left || r.bottom <= r.top;
    match (empty(a), empty(b)) {
        (true, _) => b,
        (_, true) => a,
        _ => union(a, b),
    }
}

fn union(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn offset(r: RECT, dx: i32, dy: i32) -> RECT {
    RECT { left: r.left + dx, top: r.top + dy, right: r.right + dx, bottom: r.bottom + dy }
}

/// Whether `a` lies inside `b`.
fn within(a: RECT, b: RECT) -> bool {
    a.left >= b.left && a.top >= b.top && a.right <= b.right && a.bottom <= b.bottom
}

fn grow(r: RECT, by: i32) -> RECT {
    RECT { left: r.left - by, top: r.top - by, right: r.right + by, bottom: r.bottom + by }
}

fn fl(r: RECT) -> [f32; 4] {
    [r.left as f32, r.top as f32, r.right as f32, r.bottom as f32]
}

// ------------------------------------------------------------- window --

unsafe extern "system" fn proc(hwnd: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Clicks go to the panel over it (or past it).
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, m, wp, lp),
    }
}

fn register_class() {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            lpszClassName: w!("CascadeGlass"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

/// The window a pane's glass is drawn in: no bitmap of its own (it shows
/// a DirectComposition swap chain), never activated, and left out of screen
/// copies, the duplication included.
fn glass_window() -> Option<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            w!("CascadeGlass"),
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = DestroyWindow(hwnd);
            return None;
        }
        // Square (the glass draws its own corners) and no border line.
        let corners = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corners as *const _ as *const c_void,
            std::mem::size_of_val(&corners) as u32,
        );
        let border = COLORREF(0xFFFF_FFFE);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const c_void, 4);
        let off = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as *const c_void, 4);
        Some(hwnd)
    }
}

// --------------------------------------------------------------- panes --

/// The GPU glass of one panel (owned by its `Backdrop`).
pub struct Pane {
    id: u32,
}

/// What `Pane::show` did.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shown {
    /// The glass is on screen (or will be, once its screen's first copy
    /// comes in). `again`: draw the panel once more, so the glass gets its
    /// drawing to take back out of the screen copy.
    Yes { again: bool },
    /// It can't be shown here: use DWM's blur instead.
    No,
}

impl Pane {
    /// A pane, if the GPU glass works on this PC.
    pub fn new() -> Option<Self> {
        if !usable() {
            return None;
        }
        let hwnd = glass_window()?;
        with(|s| {
            s.next_id += 1;
            let id = s.next_id;
            s.panes.push(PaneState {
                id,
                hwnd,
                look: Look {
                    band: 0.0,
                    bend: 0.0,
                    split: 0.0,
                    blur: 0.0,
                    edge_blur: 0.0,
                    key: 0.0,
                    back: 0.0,
                    ambient: 0.0,
                    vivid: 1.0,
                },
                rect: RECT::default(),
                shape: None,
                radius: 0,
                content: HWND::default(),
                visible: false,
                dirty: true,
                gpu: None,
                output: None,
                res: None,
            });
            Pane { id }
        })
        .or_else(|| {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            None
        })
    }

    /// Shows the glass at `rect` (screen px), directly under `content` (the
    /// panel's own window), cut to `shape` (see `Backdrop::place_shaped`) or
    /// else to the whole rect with corners of `radius` px, looking like
    /// `look` (sizes at 100% scaling).
    pub fn show(&self, rect: RECT, shape: Option<(RECT, i32)>, radius: i32, look: Look, content: HWND) -> Shown {
        with(|s| {
            let shown = show(s, self.id, rect, shape, radius, look, content);
            if shown == Shown::No {
                // Its panel takes DWM's blur instead: none of the glass stays up.
                hide(s, self.id);
            }
            shown
        })
        .unwrap_or(Shown::No)
    }

    /// Puts the glass back directly under `content`.
    pub fn restack(&self, content: HWND) {
        with(|s| {
            if let Some(p) = s.panes.iter().find(|p| p.id == self.id && p.visible) {
                unsafe {
                    let _ = SetWindowPos(p.hwnd, Some(content), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
        });
    }

    pub fn hide(&self) {
        with(|s| hide(s, self.id));
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        with(|s| {
            hide(s, self.id);
            if let Some(i) = s.panes.iter().position(|p| p.id == self.id) {
                let p = s.panes.remove(i);
                drop(p.res);
                unsafe {
                    let _ = DestroyWindow(p.hwnd);
                }
            }
        });
    }
}

fn hide(s: &mut State, id: u32) {
    let Some(p) = s.panes.iter_mut().find(|p| p.id == id) else { return };
    if !p.visible {
        return;
    }
    p.visible = false;
    let content = p.content;
    unsafe {
        let _ = ShowWindow(p.hwnd, SW_HIDE);
    }
    refresh_active(s, content);
}

/// Marks `content` active while a pane on screen belongs to it.
fn refresh_active(s: &mut State, content: HWND) {
    let on = s.panes.iter().any(|p| p.visible && p.content == content);
    for c in s.contents.iter_mut().filter(|c| c.hwnd == content) {
        if on && !c.active {
            // Just came on: its last drawing may be old, and the copy
            // behind it may not have caught up.
            c.uploaded = false;
            c.moved_at = Instant::now();
            c.settled = false;
            c.swept = c.rect;
            c.asks = 0;
        }
        c.active = on;
    }
}

/// The screen under the middle of `r`.
fn screen_at(outputs: &[Output], r: RECT) -> Option<usize> {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    outputs.iter().position(|o| cx >= o.rect.left && cx < o.rect.right && cy >= o.rect.top && cy < o.rect.bottom)
}

/// The screen under the middle of `r`, finding the screens again (at most
/// once a second) if none is.
fn output_for(s: &mut State, r: RECT) -> Option<usize> {
    if let Some(i) = screen_at(&s.outputs, r) {
        return Some(i);
    }
    if s.enumerated_at.is_some_and(|t| t.elapsed() < REENUMERATE) {
        return None;
    }
    let _ = enumerate(s);
    screen_at(&s.outputs, r)
}

fn show(
    s: &mut State,
    id: u32,
    rect: RECT,
    shape: Option<(RECT, i32)>,
    radius: i32,
    look: Look,
    content: HWND,
) -> Shown {
    if s.usable != Some(true) {
        return Shown::No;
    }
    let Some(pi) = s.panes.iter().position(|p| p.id == id) else { return Shown::No };
    if rect.right - rect.left < 2 || rect.bottom - rect.top < 2 {
        hide(s, id);
        return Shown::Yes { again: false };
    }
    let Some(oi) = output_for(s, rect) else { return Shown::No };
    if s.outputs[oi].unsupported {
        return Shown::No;
    }
    let gpu = s.outputs[oi].gpu;
    // On another card than before: what it drew with belongs to the old one.
    if s.panes[pi].gpu.is_some_and(|g| g != gpu) {
        let old = s.panes[pi].hwnd;
        s.panes[pi].res = None;
        // A window takes one DirectComposition target: a fresh window for the new card.
        let Some(hwnd) = glass_window() else { return Shown::No };
        unsafe {
            let _ = DestroyWindow(old);
        }
        s.panes[pi].hwnd = hwnd;
        s.panes[pi].visible = false;
    }
    if s.panes[pi].res.is_none() {
        match pane_gpu(&s.gpus[gpu], s.panes[pi].hwnd) {
            Ok(r) => s.panes[pi].res = Some(r),
            Err(e) => {
                s.log(&format!("couldn't make a pane's swap chain: {e}"));
                return Shown::No;
            }
        }
    }
    let p = &mut s.panes[pi];
    let moved = p.rect != rect || p.shape != shape || p.radius != radius || p.look != look || !p.visible;
    let was = p.visible;
    let old_content = p.content;
    p.rect = rect;
    p.shape = shape;
    p.radius = radius;
    p.look = look;
    p.content = content;
    p.gpu = Some(gpu);
    p.output = Some(oi);
    p.visible = true;
    p.dirty |= moved;
    let hwnd = p.hwnd;
    if moved || old_content != content {
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(content),
                rect.left,
                rect.top,
                (rect.right - rect.left).max(1),
                (rect.bottom - rect.top).max(1),
                SWP_NOACTIVATE | if was { SWP_NOOWNERZORDER } else { SWP_SHOWWINDOW },
            );
        }
    }
    if old_content != content && !old_content.is_invalid() {
        refresh_active(s, old_content);
    }
    if !s.contents.iter().any(|c| c.hwnd == content && c.gpu == gpu) {
        let now = Instant::now();
        s.contents.push(Content {
            hwnd: content,
            gpu,
            now: None,
            before: None,
            rect: RECT::default(),
            alpha: 0,
            uploaded: false,
            moved_at: now,
            presented_at: now,
            swept: RECT::default(),
            settled: false,
            remerge: false,
            asks: 0,
            active: false,
        });
    }
    refresh_active(s, content);
    // Start duplicating its screen, and draw now if there's something to draw from.
    s.outputs[oi].idle_since = None;
    if s.outputs[oi].dup.is_none() {
        poll(s, oi);
        merge(s, oi);
        if s.outputs[oi].unsupported {
            return Shown::No;
        }
    }
    if s.panes[pi].dirty {
        render(s, pi);
    }
    // Its drawing isn't in yet: ask for another frame, a few times at most
    // (a panel that never presents through `gfx` would otherwise be asked forever).
    let mut again = false;
    for c in s.contents.iter_mut().filter(|c| c.hwnd == content && c.gpu == gpu && !c.uploaded) {
        if c.asks < ASKS {
            c.asks += 1;
            again = true;
        }
    }
    if s.panes[pi].gpu.is_none() {
        // Rendering found its card gone.
        return Shown::No;
    }
    Shown::Yes { again }
}

/// Sets up a pane's window to show a swap chain drawn on `gpu`.
fn pane_gpu(gpu: &Gpu, hwnd: HWND) -> windows::core::Result<PaneGpu> {
    unsafe {
        let (w, h) = (64, 64);
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        let chain = gpu.factory.CreateSwapChainForComposition(&gpu.device, &desc, None)?;
        let target = gpu.dcomp.CreateTargetForHwnd(hwnd, true)?;
        let visual = gpu.dcomp.CreateVisual()?;
        visual.SetContent(&chain)?;
        target.SetRoot(&visual)?;
        gpu.dcomp.Commit()?;
        Ok(PaneGpu { _target: target, _visual: visual, chain, rtv: None, chain_w: w, chain_h: h, t1: None, t2: None })
    }
}

// --------------------------------------------------- panel drawings in --

/// A panel window `hwnd` presented the `width` x `height` px of `bits`
/// (premultiplied BGRA, rows `stride` bytes apart) at `pos`, faded to
/// `alpha`; only `part` of it changed, if given. Called by `gfx` for every
/// layered window; ignored unless a pane on screen belongs to it.
#[allow(clippy::too_many_arguments)]
pub fn presented(
    hwnd: HWND,
    pos: POINT,
    width: i32,
    height: i32,
    bits: *const u8,
    stride: usize,
    alpha: u8,
    part: Option<RECT>,
) {
    if bits.is_null() || width <= 0 || height <= 0 {
        return;
    }
    with(|s| {
        let now = Instant::now();
        for i in 0..s.contents.len() {
            let c = &s.contents[i];
            if c.hwnd != hwnd || !c.active {
                continue;
            }
            let gpu = &s.gpus[c.gpu];
            let rect = RECT { left: pos.x, top: pos.y, right: pos.x + width, bottom: pos.y + height };
            let c = &mut s.contents[i];
            if c.rect != rect || c.alpha != alpha {
                // Moved: the screen copy runs a frame or two behind, so leave
                // all it covered alone until it keeps still.
                c.swept = join(join(c.swept, c.rect), rect);
                c.moved_at = now;
                c.settled = false;
                c.rect = rect;
                c.alpha = alpha;
            }
            c.presented_at = now;
            let fits = c.now.as_ref().is_some_and(|t| t.w >= width as u32 && t.h >= height as u32);
            if !fits {
                let (tw, th) = (roomy(width), roomy(height));
                c.now = texture(gpu, tw, th, DXGI_FORMAT_B8G8R8A8_UNORM, false, 1);
                c.before = texture(gpu, tw, th, DXGI_FORMAT_B8G8R8A8_UNORM, false, 1);
                c.uploaded = false;
            }
            let (Some(cur), Some(before)) = (c.now.as_ref(), c.before.as_ref()) else { continue };
            let whole = RECT { left: 0, top: 0, right: width, bottom: height };
            let part = if c.uploaded { part.and_then(|p| intersect(p, whole)).unwrap_or(whole) } else { whole };
            let b = D3D11_BOX {
                left: part.left as u32,
                top: part.top as u32,
                front: 0,
                right: part.right as u32,
                bottom: part.bottom as u32,
                back: 1,
            };
            unsafe {
                if c.uploaded {
                    gpu.ctx.CopySubresourceRegion(&before.tex, 0, b.left, b.top, 0, &cur.tex, 0, Some(&b));
                }
                let at = bits.add(part.top as usize * stride + part.left as usize * 4);
                gpu.ctx.UpdateSubresource(&cur.tex, 0, Some(&b), at as *const c_void, stride as u32, 0);
                if !c.uploaded {
                    gpu.ctx.CopySubresourceRegion(&before.tex, 0, b.left, b.top, 0, &cur.tex, 0, Some(&b));
                }
            }
            c.uploaded = true;
            c.remerge = true;
            c.asks = 0;
        }
    });
}

// ---------------------------------------------------------- the screens --

/// Whether any pane on screen is on output `oi`.
fn has_panes(s: &State, oi: usize) -> bool {
    s.panes.iter().any(|p| p.visible && p.output == Some(oi))
}

/// Takes the screen's newest frame, if it changed, into `frame`, noting the
/// parts that changed in `pending`.
fn poll(s: &mut State, oi: usize) {
    let now = Instant::now();
    let o = &mut s.outputs[oi];
    if o.unsupported {
        return;
    }
    let gpu = &s.gpus[o.gpu];
    if o.dup.is_none() {
        if o.retry_at.is_some_and(|t| now < t) {
            return;
        }
        let made = unsafe {
            match o.output.cast::<IDXGIOutput5>() {
                Ok(o5) => o5
                    .DuplicateOutput1(&gpu.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM])
                    .or_else(|_| o.output.DuplicateOutput(&gpu.device)),
                Err(_) => o.output.DuplicateOutput(&gpu.device),
            }
        };
        match made {
            Ok(d) => {
                o.dup = Some(d);
                o.fresh = true;
                o.retry_at = None;
            }
            Err(e) => {
                o.retry_at = Some(now + RETRY);
                let code = e.code();
                if code == DXGI_ERROR_UNSUPPORTED {
                    o.unsupported = true;
                    FELL.store(true, Ordering::Relaxed);
                }
                let what = format!("couldn't duplicate a screen: {e}");
                s.log(&what);
                return;
            }
        }
    }
    let o = &mut s.outputs[oi];
    let Some(dup) = o.dup.clone() else { return };
    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut res: Option<IDXGIResource> = None;
    if let Err(e) = unsafe { dup.AcquireNextFrame(0, &mut info, &mut res) } {
        if e.code() != DXGI_ERROR_WAIT_TIMEOUT {
            // Lost (a mode change, a full-screen game, the secure desktop): start over soon.
            o.dup = None;
            o.pending.clear();
            o.retry_at = Some(now + RETRY);
            if e.code() != DXGI_ERROR_ACCESS_LOST {
                let what = format!("screen duplication stopped: {e}");
                s.log(&what);
            }
        }
        return;
    }
    let mut failed = None;
    let taken = (|| -> Option<()> {
        // Only the pointer moved: nothing new on screen.
        if info.LastPresentTime == 0 && !o.fresh {
            return Some(());
        }
        let tex: ID3D11Texture2D = res.as_ref()?.cast().ok()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let fits = o.frame.as_ref().is_some_and(|f| f.w == desc.Width && f.h == desc.Height);
        if !fits {
            o.frame = texture(gpu, desc.Width, desc.Height, desc.Format, false, 1);
            o.clean = texture(gpu, desc.Width, desc.Height, DXGI_FORMAT_B8G8R8A8_UNORM, true, 1);
            match o.clean.as_ref().and_then(|c| c.rtv.as_ref()) {
                Some(rtv) if o.frame.is_some() => unsafe { gpu.ctx.ClearRenderTargetView(rtv, &[0.0; 4]) },
                _ => {
                    // Out of video memory, or a format it can't copy: this screen falls back.
                    o.frame = None;
                    o.clean = None;
                    o.unsupported = true;
                    failed =
                        Some(format!("no room for a {}x{} screen copy ({:?})", desc.Width, desc.Height, desc.Format));
                    return None;
                }
            }
            o.fresh = true;
        }
        let frame = o.frame.as_ref()?;
        let whole = RECT { left: 0, top: 0, right: desc.Width as i32, bottom: desc.Height as i32 };
        let mut changed = Vec::new();
        if !o.fresh {
            changed = changed_rects(&dup, &info).unwrap_or_else(|| vec![whole]);
        }
        o.own.retain(|&(_, at)| now.duration_since(at) < OWN);
        // About exactly where a pane just presented: the pane's own change.
        let own = |r: &RECT| o.own.iter().any(|&(p, _)| within(*r, grow(p, 2)) && within(p, grow(*r, 2)));
        if o.fresh || changed.len() > 48 {
            unsafe { gpu.ctx.CopyResource(&frame.tex, &tex) };
            o.pending.push(whole);
        } else {
            for r in changed {
                let Some(r) = intersect(r, whole) else { continue };
                // Copied either way (a panel's own window may have changed
                // there too: see `remerge`), but only worked out again if
                // it's not just the glass.
                if !own(&r) {
                    o.pending.push(r);
                }
                let b = D3D11_BOX {
                    left: r.left as u32,
                    top: r.top as u32,
                    front: 0,
                    right: r.right as u32,
                    bottom: r.bottom as u32,
                    back: 1,
                };
                unsafe { gpu.ctx.CopySubresourceRegion(&frame.tex, 0, b.left, b.top, 0, &tex, 0, Some(&b)) };
            }
        }
        o.fresh = false;
        Some(())
    })();
    unsafe {
        // The copies out of the frame are sent before it's handed back.
        gpu.ctx.Flush();
        let _ = dup.ReleaseFrame();
    }
    if taken.is_none() {
        o.fresh = true;
    }
    if o.pending.len() > PENDING {
        let all = o.pending.iter().copied().reduce(union).unwrap_or_default();
        o.pending = vec![all];
    }
    if o.unsupported {
        o.dup = None;
        o.pending.clear();
    }
    if let Some(what) = failed {
        s.log(&what);
        FELL.store(true, Ordering::Relaxed);
    }
}

/// The parts of the screen a frame changed: its dirty rects and where its
/// moved rects landed (the frame holds the whole new picture, so both are
/// copied from it). None if Windows wouldn't say.
fn changed_rects(dup: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO) -> Option<Vec<RECT>> {
    let size = info.TotalMetadataBufferSize as usize;
    if size == 0 {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    unsafe {
        let n = size / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>() + 1;
        let mut moves = vec![DXGI_OUTDUPL_MOVE_RECT::default(); n];
        let mut used = 0u32;
        dup.GetFrameMoveRects(
            (n * std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()) as u32,
            moves.as_mut_ptr(),
            &mut used,
        )
        .ok()?;
        let count = used as usize / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>();
        out.extend(moves[..count].iter().map(|m| m.DestinationRect));
        let n = size / std::mem::size_of::<RECT>() + 1;
        let mut dirty = vec![RECT::default(); n];
        let mut used = 0u32;
        dup.GetFrameDirtyRects((n * std::mem::size_of::<RECT>()) as u32, dirty.as_mut_ptr(), &mut used).ok()?;
        let count = used as usize / std::mem::size_of::<RECT>();
        out.extend_from_slice(&dirty[..count]);
    }
    Some(out)
}

/// Writes one set of constants (for the next draw).
fn set_consts(gpu: &Gpu, c: &Consts) {
    unsafe {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        if gpu.ctx.Map(&gpu.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() && !mapped.pData.is_null() {
            std::ptr::copy_nonoverlapping(
                c as *const Consts as *const u8,
                mapped.pData as *mut u8,
                std::mem::size_of::<Consts>(),
            );
            gpu.ctx.Unmap(&gpu.cb, 0);
        }
    }
}

/// Sets up the pipeline every pass shares: the quad, the constants, the sampler.
fn begin(gpu: &Gpu, rtv: &ID3D11RenderTargetView, w: u32, h: u32) {
    unsafe {
        let ctx = &gpu.ctx;
        ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
        ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: w as f32,
            Height: h as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }]));
        ctx.RSSetState(&gpu.raster);
        ctx.OMSetBlendState(None, None, u32::MAX);
        ctx.IASetInputLayout(None);
        ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        ctx.VSSetShader(&gpu.vs, None);
        ctx.VSSetConstantBuffers(0, Some(&[Some(gpu.cb.clone())]));
        ctx.PSSetConstantBuffers(0, Some(&[Some(gpu.cb.clone())]));
        ctx.PSSetSamplers(0, Some(&[Some(gpu.sampler.clone())]));
    }
}

/// Draws the quad `c.quad` with pixel shader `ps` reading `srvs`.
fn draw(gpu: &Gpu, ps: usize, srvs: &[Option<ID3D11ShaderResourceView>], c: &Consts) {
    set_consts(gpu, c);
    unsafe {
        gpu.ctx.PSSetShader(&gpu.ps[ps - PS_COPY], None);
        gpu.ctx.PSSetShaderResources(0, Some(srvs));
        gpu.ctx.Draw(4, 0);
        // Nothing stays bound, so any of them can be drawn into next.
        gpu.ctx.PSSetShaderResources(0, Some(&[None, None, None]));
    }
}

/// Brings `clean` up to date where the screen changed: the screen as it is
/// where no panel is drawn, what's behind a panel's drawing where it's
/// worked out, and the last clean pixels where it can't be (under solid
/// drawing, and while a panel moves).
fn merge(s: &mut State, oi: usize) {
    let now = Instant::now();
    let o = &mut s.outputs[oi];
    if o.pending.is_empty() {
        return;
    }
    let regions = coalesce(std::mem::take(&mut o.pending));
    let (Some(frame), Some(clean)) = (o.frame.as_ref(), o.clean.as_ref()) else { return };
    let Some(rtv) = clean.rtv.as_ref() else { return };
    let gpu = &s.gpus[o.gpu];
    let origin = (o.rect.left, o.rect.top);
    let screen = RECT { left: 0, top: 0, right: clean.w as i32, bottom: clean.h as i32 };
    // The panels on this screen, in its px: where each is drawn now, and (for
    // one on the move) everywhere it has been since it started moving.
    let mut all: Vec<[f32; 4]> = Vec::new();
    let mut unsettled: Vec<[f32; 4]> = Vec::new();
    let mut settled: Vec<(usize, RECT)> = Vec::new();
    for (i, c) in s.contents.iter().enumerate() {
        if !c.active || c.gpu != o.gpu {
            continue;
        }
        let still = c.uploaded && now.duration_since(c.moved_at) >= SETTLE;
        let cover = if still { c.rect } else { join(c.rect, c.swept) };
        let Some(local) = intersect(offset(cover, -origin.0, -origin.1), screen) else { continue };
        if all.len() < 16 {
            all.push(fl(local));
        }
        if still {
            settled.push((i, local));
        } else if unsettled.len() < 16 {
            unsettled.push(fl(local));
        }
    }
    begin(gpu, rtv, clean.w, clean.h);
    let target = [clean.w as f32, clean.h as f32, 0.0, 0.0];
    let mut c = Consts { target, ..Default::default() };
    for &region in &regions {
        c.quad = fl(region);
        c.p[0] = [all.len() as f32, 0.0, 0.0, 0.0];
        c.rects[..all.len()].copy_from_slice(&all);
        draw(gpu, PS_COPY, &[Some(frame.srv.clone())], &c);
        for &(ci, cr) in &settled {
            let Some(part) = intersect(region, cr) else { continue };
            let content = &s.contents[ci];
            let (Some(now_t), Some(before_t)) = (content.now.as_ref(), content.before.as_ref()) else { continue };
            let corner = (content.rect.left - origin.0, content.rect.top - origin.1);
            // Pixels that changed in its last frame are left until the copy
            // has surely caught up with them.
            let fresh = now.duration_since(content.presented_at) < SETTLE;
            c.quad = fl(part);
            c.p[0] = [corner.0 as f32, corner.1 as f32, content.alpha as f32 / 255.0, fresh as u32 as f32];
            c.p[1] = [unsettled.len() as f32, SOLID, 0.0, 0.0];
            c.rects[..unsettled.len()].copy_from_slice(&unsettled);
            draw(gpu, PS_UNCOMP, &[Some(frame.srv.clone()), Some(now_t.srv.clone()), Some(before_t.srv.clone())], &c);
        }
    }
    unsafe { gpu.ctx.OMSetRenderTargets(None, None) };
    // Panes reading from what changed draw again.
    for p in s.panes.iter_mut().filter(|p| p.visible && p.output == Some(oi)) {
        let reach = grow(offset(p.rect, -origin.0, -origin.1), 64);
        if regions.iter().any(|&r| intersect(r, reach).is_some()) {
            p.dirty = true;
        }
    }
}

/// Fewer, larger rects: past a dozen, their bounding box.
fn coalesce(mut rects: Vec<RECT>) -> Vec<RECT> {
    rects.retain(|r| r.right > r.left && r.bottom > r.top);
    if rects.len() > 12 {
        let all = rects.iter().copied().reduce(union).unwrap_or_default();
        return vec![all];
    }
    rects
}

// ---------------------------------------------------------------- draw --

/// Draws pane `pi` from its screen's clean picture and presents it.
fn render(s: &mut State, pi: usize) {
    let p = &s.panes[pi];
    let (Some(gi), Some(oi)) = (p.gpu, p.output) else { return };
    if !p.visible {
        return;
    }
    let (rect, hwnd, cut, radius, base) = (p.rect, p.hwnd, p.shape, p.radius, p.look);
    let o = &s.outputs[oi];
    if o.gpu != gi {
        return;
    }
    let Some(clean) = o.clean.as_ref() else { return };
    if o.frame.is_none() {
        return;
    }
    let gpu = &s.gpus[gi];
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w < 2 || h < 2 {
        return;
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let k = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    let look = Look { band: base.band * k, blur: base.blur * k, ..base };
    let sigma = look.blur.max(0.0);
    let taps = if sigma < 0.4 { 0.0 } else { (sigma * 2.5).ceil().min(31.0) };
    // Room round the pane for the blur and the rim's softer copies to reach.
    let margin = ((sigma * 3.0).ceil() as i32 + (2.0f32).powf(look.edge_blur + 1.0).ceil() as i32 + 2).min(64);
    let (rw, rh) = (w + 2 * margin, h + 2 * margin);
    let shape = match cut {
        Some((r, rad)) => glassmath::Shape::new(
            r.left as f32,
            r.top as f32,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            rad as f32,
        ),
        None => glassmath::Shape::new(0.0, 0.0, w as f32, h as f32, radius as f32),
    };
    let lut = s.lut;
    let res = s.panes[pi].res.as_mut();
    let Some(res) = res else { return };
    unsafe {
        // Room for the pane in the swap chain and the blur's textures.
        if res.chain_w < w as u32 || res.chain_h < h as u32 {
            // Nothing may still hold the old buffers, bound or waiting to go.
            res.rtv = None;
            gpu.ctx.OMSetRenderTargets(None, None);
            gpu.ctx.Flush();
            let (cw, ch) = (roomy(w).max(res.chain_w), roomy(h).max(res.chain_h));
            if res.chain.ResizeBuffers(2, cw, ch, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0)).is_err() {
                return;
            }
            res.chain_w = cw;
            res.chain_h = ch;
        }
        if res.rtv.is_none() {
            let Ok(buffer) = res.chain.GetBuffer::<ID3D11Texture2D>(0) else { return };
            let mut rtv = None;
            if gpu.device.CreateRenderTargetView(&buffer, None, Some(&mut rtv)).is_err() {
                return;
            }
            res.rtv = rtv;
        }
        if res.t1.as_ref().is_none_or(|t| t.w < rw as u32 || t.h < rh as u32) {
            let (tw, th) = (roomy(rw), roomy(rh));
            res.t1 = texture(gpu, tw, th, DXGI_FORMAT_R16G16B16A16_FLOAT, true, 1);
            res.t2 = texture(gpu, tw, th, DXGI_FORMAT_R16G16B16A16_FLOAT, true, 7);
        }
    }
    let (Some(t1), Some(t2), Some(out)) = (res.t1.as_ref(), res.t2.as_ref(), res.rtv.as_ref()) else { return };
    let (Some(t1_rtv), Some(t2_rtv)) = (t1.rtv.as_ref(), t2.rtv.as_ref()) else { return };
    let region = (rect.left - o.rect.left - margin, rect.top - o.rect.top - margin);

    // What's behind, blurred: across, then down. Both start empty, so what's
    // past the part drawn (a larger pane's, from before) reads as unknown.
    let mut c = Consts::default();
    unsafe {
        gpu.ctx.ClearRenderTargetView(t1_rtv, &[0.0; 4]);
        gpu.ctx.ClearRenderTargetView(t2_rtv, &[0.0; 4]);
    }
    begin(gpu, t1_rtv, t1.w, t1.h);
    c.target = [t1.w as f32, t1.h as f32, 0.0, 0.0];
    c.quad = [0.0, 0.0, rw as f32, rh as f32];
    c.map = [region.0 as f32, region.1 as f32, clean.w as f32, clean.h as f32];
    c.p[0] = [1.0, 0.0, sigma.max(0.1), taps];
    draw(gpu, PS_BLUR, &[Some(clean.srv.clone())], &c);
    begin(gpu, t2_rtv, t2.w, t2.h);
    c.target = [t2.w as f32, t2.h as f32, 0.0, 0.0];
    c.map = [0.0, 0.0, t1.w as f32, t1.h as f32];
    c.p[0] = [0.0, 1.0, sigma.max(0.1), taps];
    draw(gpu, PS_BLUR, &[Some(t1.srv.clone())], &c);
    unsafe {
        gpu.ctx.OMSetRenderTargets(None, None);
        gpu.ctx.GenerateMips(&t2.srv);
    }

    // The glass.
    begin(gpu, out, res.chain_w, res.chain_h);
    unsafe { gpu.ctx.ClearRenderTargetView(out, &[0.0; 4]) };
    c.target = [res.chain_w as f32, res.chain_h as f32, 0.0, 0.0];
    c.quad = [0.0, 0.0, w as f32, h as f32];
    c.map = [0.0, 0.0, t2.w as f32, t2.h as f32];
    c.p[0] = [shape.cx, shape.cy, shape.hx, shape.hy];
    c.p[1] = [shape.r, look.band, look.bend, look.split];
    c.p[2] = [look.edge_blur, look.key, look.back, look.ambient];
    c.p[3] = [look.vivid, margin as f32, glassmath::THICKNESS, 0.0];
    c.lut = lut;
    draw(gpu, PS_GLASS, &[Some(t2.srv.clone())], &c);
    unsafe {
        gpu.ctx.OMSetRenderTargets(None, None);
        let hr = res.chain.Present(0, DXGI_PRESENT_DO_NOT_WAIT);
        if hr == DXGI_ERROR_WAS_STILL_DRAWING {
            // Try again on the next tick.
            return;
        }
        if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
            lost(s);
            return;
        }
    }
    s.panes[pi].dirty = false;
    let o = &mut s.outputs[oi];
    let local = offset(rect, -o.rect.left, -o.rect.top);
    if o.own.len() >= 16 {
        o.own.remove(0);
    }
    o.own.push((local, Instant::now()));
}

/// The graphics card went away (a driver update, a reset): everything made
/// on it goes, and the panes say they can't show (their panels fall back).
fn lost(s: &mut State) {
    s.log("the graphics device was lost");
    for p in &mut s.panes {
        p.res = None;
        p.gpu = None;
        p.output = None;
        p.dirty = true;
    }
    s.contents.clear();
    s.outputs.clear();
    s.gpus.clear();
    s.usable = Some(false);
    READY.store(false, Ordering::Relaxed);
    FELL.store(true, Ordering::Relaxed);
}

/// Keeps the glass up to date: takes each screen's newest frame, works out
/// what's behind the panels where it changed, redraws the panes that read
/// from there, and stops duplicating screens with no glass on them for a
/// while. Call it about every frame while `active`. Returns `active()`.
pub fn tick() -> bool {
    with(|s| {
        let now = Instant::now();
        // Panels that just kept still long enough (what they covered while
        // moving), and panels drawn anew a while ago (the pixels that changed
        // were left alone the first time): what's behind them is worked out
        // again, in full.
        for i in 0..s.contents.len() {
            let c = &s.contents[i];
            if !c.active || !c.uploaded {
                continue;
            }
            let settling = !c.settled && now.duration_since(c.moved_at) >= SETTLE;
            let redrawn = c.remerge && now.duration_since(c.presented_at) >= SETTLE;
            if !settling && !redrawn {
                continue;
            }
            let area = if settling { join(c.rect, c.swept) } else { c.rect };
            let gpu = c.gpu;
            let c = &mut s.contents[i];
            c.remerge = false;
            if settling {
                c.settled = true;
                c.swept = c.rect;
            }
            for o in s.outputs.iter_mut().filter(|o| o.gpu == gpu && o.dup.is_some()) {
                if let Some(r) = intersect(area, o.rect) {
                    o.pending.push(offset(r, -o.rect.left, -o.rect.top));
                }
            }
        }
        for oi in 0..s.outputs.len() {
            if has_panes(s, oi) {
                s.outputs[oi].idle_since = None;
                poll(s, oi);
                merge(s, oi);
            } else if s.outputs[oi].dup.is_some() {
                let o = &mut s.outputs[oi];
                let since = *o.idle_since.get_or_insert(now);
                if now.duration_since(since) >= IDLE {
                    o.dup = None;
                    o.idle_since = None;
                    o.fresh = true;
                    // The next duplication starts with the whole screen anyway.
                    o.pending.clear();
                }
            }
        }
        for pi in 0..s.panes.len() {
            if s.panes[pi].visible && s.panes[pi].dirty {
                render(s, pi);
            }
        }
        s.panes.iter().any(|p| p.visible) || s.outputs.iter().any(|o| o.dup.is_some())
    })
    .unwrap_or(false)
}
