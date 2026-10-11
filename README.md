# Cascade

A tiny native notch for Windows. Rust and Win32 only: no webview, no runtime, one small `.exe`.

- **Idle:** a small black notch at the top centre of your main screen.
- **Music playing:** it widens to show the album art and a live visualizer tinted to the art's colours.
- **Hover:** it springs open to show the title, artist, progress bar and play/pause/skip buttons. With nothing playing it shows the clock, date and battery.
- **Volume / brightness:** changes appear in the notch instead of the Windows flyout. Scroll over the notch to change volume.
- **Fullscreen apps and games:** it hides automatically.

## Running it

Grab `cascade.exe` from the latest successful **build** run in the Actions tab (artifact `cascade`). Put it anywhere and double-click it. Right-click the notch, or the tray icon, for:

- Start with Windows
- Hide Windows volume flyout
- Open / reload config (`%APPDATA%\Cascade\config.ini`)
- Quit

To build it yourself, install [Rust](https://rustup.rs) on Windows and run `cargo build --release`. The exe lands in `target\release\cascade.exe`.

## Why it's light

| | Bloom-style (Tauri + React) | Cascade |
|---|---|---|
| UI | WebView2 windows | Direct2D into one layered window |
| Click-through | Global mouse hook + toggling | Per-pixel alpha: transparent pixels pass clicks |
| Volume | Polled every 64 ms | `IAudioEndpointVolumeCallback` (pushed by Windows) |
| Media | Polled | Media-session events |
| Fullscreen | — | App-bar `ABN_FULLSCREENAPP` notification |
| Visualizer | Runs whenever media plays | Runs only while the bars are on screen |
| Idle loop | Several polling threads | Blocks in `GetMessage`: 0% CPU |

Frames are only drawn while something is animating. Rendering uses Direct2D's software rasteriser, because the surface is small and this keeps the GPU asleep.

## Layout

| File | What it does |
|---|---|
| `src/ui.rs` | Window, states, layout, input, frame loop |
| `src/gfx.rs` | Direct2D/DirectWrite drawing into the layered window |
| `src/spring.rs` | Spring physics behind every animation |
| `src/media.rs` | Now playing + album art (event-driven, own thread) |
| `src/audio.rs` | Volume callbacks, loopback visualizer + FFT |
| `src/brightness.rs` | Laptop brightness events via WMI |
| `src/shell.rs` | Tray, autostart, fullscreen detection, flyout, menu |
| `src/config.rs` | `config.ini` |
