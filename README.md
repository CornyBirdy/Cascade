# Cascade

A tiny native notch, dock and launcher for Windows. Rust and Win32 only: no webview, no runtime, one small `.exe`.

## Notch

A small black notch at the top centre of your main screen. Turn it off in the right-click menu if you don't want it.

- **Live activity:** while music plays it widens to show album art and a visualizer in the art's colours. While the focus timer runs it shows a progress ring and the time left.
- **Hover to open.** Scroll, or click the dots at the bottom, to switch between three modes:
  - **Media:** title, artist, progress bar and play/pause/skip buttons. With nothing playing it shows the time, date, weather and battery instead.
  - **Controls:** Wi-Fi and Bluetooth toggles, a notification-centre button, and volume and brightness sliders. Brightness works on laptop screens and on external monitors that support DDC/CI.
  - **Calendar:** the current month, plus a focus/break timer (25/5 minutes by default).
- **Volume and brightness changes** show in the notch instead of the Windows flyout. Scroll over the closed notch to change volume.
- **Fullscreen apps and games:** the notch and dock hide automatically.

## Dock

- Pinned and running apps along the bottom. A dot marks apps with open windows; a wider one marks the app in front.
- **Click:** opens the app if it isn't running, brings it to the front if it is, minimises it if it's already in front, and cycles through its windows if it has several.
- **Middle-click:** opens a new window.
- **Hover:** icons grow under the cursor, and after a moment live previews of the app's windows appear. Click a preview to switch to that window, or its × to close it.
- **Drag pinned icons** to reorder them.
- **Right-click an icon** to pin or unpin it, open a new window, or close its windows.
- **Auto-hide** (optional): the dock fades away until you push the mouse to the bottom edge. This works best with the Windows taskbar hidden.
- **Replaces the Windows taskbar** while the dock is on (untick *Hide Windows taskbar* to keep both). The real taskbar is restored when Cascade quits, when the dock is turned off, and on the next start if Cascade crashed.

Pinned apps are stored in `%APPDATA%\Cascade\pinned.txt`, one per line.

## Launcher

Cascade's own Start menu. Windows doesn't let other apps restyle its real one, so Cascade replaces it instead.

- **Opening it:** tap the **Win key** on its own, or click the **Start button** at the left of the dock. Shortcuts like Win+E, Win+R and Win+Shift+S work as normal.
- **Searching:** start typing to search every installed app, Store apps included. Use the arrow keys and Enter, or click.
- **Layout:** pinned apps (the same as your dock's) sit above an alphabetical list of all apps. Right-click any app to pin or unpin it.
- **Footer:** your name, a button for Cascade settings, and a power menu (lock, sleep, sign out, restart, shut down).

Turn off the Win-key takeover or the whole launcher in Settings.

## Settings

Search **Cascade Settings** in the launcher or the Windows Start menu, or right-click the notch, dock or tray icon and choose **Settings…**. Every option has a toggle, slider or text box, and changes apply immediately. Starting Cascade again while it's already running also opens Settings.

## Running it

Grab `cascade.exe` from the latest successful **build** run in the Actions tab (artifact `cascade`). Put it anywhere and double-click it. Right-click the notch, the dock background or the tray icon for:

- Settings… / Open launcher
- Show notch / Show dock / Auto-hide dock / Hide Windows taskbar
- Start with Windows
- Hide Windows volume flyout
- Open / reload config (`%APPDATA%\Cascade\config.ini`; focus-timer lengths, weather location, sizes and more)
- Quit

## Updating

Right-click the notch, dock or tray icon and choose **Update Cascade**. One click checks GitHub for a newer build, downloads it, swaps the exe and restarts. Cascade also checks every few hours and shows a note in the notch when an update is out.

Every push to the default branch is published as a GitHub release (`build-N`), and that's where the updater looks. While the repo is **private**, GitHub only serves releases to signed-in users. In that case, create a fine-grained token with read-only *Contents* access to this repo and paste it after `github_token =` in the config file. If the repo is public, no token is needed.

To build it yourself, install [Rust](https://rustup.rs) on Windows and run `cargo build --release`. The exe lands in `target\release\cascade.exe`.

## Why it's light

| | Bloom-style (Tauri + React) | Cascade |
|---|---|---|
| UI | WebView2 windows | Direct2D into small layered windows |
| Click-through | Global mouse hook + toggling | Per-pixel alpha: transparent pixels pass clicks |
| Volume | Polled every 64 ms | `IAudioEndpointVolumeCallback` (pushed by Windows) |
| Media | Polled | Media-session events |
| Window list | Polled / hooks | WinEvent hooks + debounced rescan |
| Window previews | Captured bitmaps | DWM live thumbnails (composited by Windows) |
| Wi-Fi / Bluetooth | Queried on demand | Radio state-change events |
| Fullscreen | — | App-bar `ABN_FULLSCREENAPP` notification |
| Visualizer | Runs whenever media plays | Runs only while the bars are on screen |
| Weather | — | One HTTPS request per 30 min on a sleeping thread |
| Idle loop | Several polling threads | Blocks in `GetMessage`: 0% CPU |

Frames are only drawn while something is animating. Rendering uses Direct2D's software rasteriser, because the surfaces are small and this keeps the GPU asleep.

## Layout

| File | What it does |
|---|---|
| `src/ui.rs` | App core: windows, message loop, notch states, layout, input and drawing |
| `src/launcher.rs` | Launcher: app list, search, pinned, power |
| `src/keyhook.rs` | Lone Win key → launcher |
| `src/settings.rs` | Settings window |
| `src/dock.rs` | Dock: window tracking, layout, magnification, drag, launch/focus |
| `src/preview.rs` | Window previews (DWM thumbnails) |
| `src/gfx.rs` | Direct2D/DirectWrite drawing into layered windows |
| `src/spring.rs` | Spring physics behind every animation |
| `src/media.rs` | Now playing + album art (event-driven, own thread) |
| `src/audio.rs` | Volume callbacks, loopback visualizer + FFT |
| `src/brightness.rs` | Brightness events (WMI) and setting (WMI + DDC/CI) |
| `src/radios.rs` | Wi-Fi / Bluetooth toggles |
| `src/weather.rs` | Open-Meteo over WinHTTP |
| `src/pomodoro.rs` | Focus timer |
| `src/icons.rs` | App icons and names for the dock |
| `src/update.rs` | One-click updates from GitHub releases |
| `src/http.rs` | Tiny WinHTTP client |
| `src/shell.rs` | Tray, autostart, fullscreen, taskbar, flyout, menus |
| `src/config.rs` | `config.ini` |
