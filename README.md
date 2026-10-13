# Cascade

A tiny native notch, taskbar and Start menu for Windows. Rust and Win32 only: no webview, no runtime, one small `.exe`.

## Notch

A small black notch at the top centre of your main screen. Turn it off in the right-click menu if you don't want it.

- **Live activity:** while music plays it widens to show album art and a visualizer in the art's colours. While the focus timer runs it shows a progress ring and the time left.
- **Hover to open.** Scroll, or click the dots at the bottom, to switch between three modes:
  - **Media:** title, artist, progress bar and play/pause/skip buttons. With nothing playing it shows the time, date, weather and battery instead.
  - **Controls:** Wi-Fi and Bluetooth toggles, a notifications button (opens the widgets panel), and volume and brightness sliders. Brightness works on laptop screens and on external monitors that support DDC/CI.
  - **Calendar:** the current month, plus a focus/break timer (25/5 minutes by default).
- **Volume and brightness changes** show in the notch instead of the Windows flyout. Scroll over the closed notch to change volume.
- **Fullscreen apps and games:** the notch and taskbar hide automatically.

## Taskbar

A Windows 11-style taskbar drawn by Cascade itself, over a real acrylic blur.

- **Left:** the current weather. **Centre:** Start, a search box, then your pinned and running apps. **Right:** hidden icons, network / volume / battery, and the clock.
- **App buttons** work like Windows: a short line marks apps with open windows and a longer blue one the app in front. Hover for a name and live window previews; click a preview to switch to it, or its × to close it.
- **Click an app** to open it if it isn't running, bring it to the front if it is, minimise it if it's already in front, or cycle through its windows. **Middle-click** opens a new window. **Right-click** to pin, unpin, close, or force close (Task Manager's *End task*). **Drag** pinned apps to reorder them.
- **Tray icons:** apps' notification-area icons show by the clock, the same ones you chose to show in Windows' taskbar settings. The rest are under the **^** button. Click and right-click them as usual. Cascade gets them the way alternative taskbars like RetroBar do: it keeps its own hidden tray window and passes everything on to Explorer, without touching Explorer itself.
- **Clicks on the right:** the status icons open quick settings, and the clock opens the widgets panel with your notifications.
- **Look:** full width or floating islands, centred or left-aligned, the Start button with your apps or alone on its own island at the far left, icon size, icon padding, bar padding (the gap from the bar's edge to its buttons on all four sides, which together set the bar's height) and the gap from the screen edge (all three default to the same small gap), corners, blur and tint, and which parts to show. All of it is in Settings → Taskbar.
- **Auto-hide** (optional): push the mouse to the bottom edge to bring it back.
- **Replaces the Windows taskbar** while it's on (untick *Replace the Windows taskbar* to keep both). The real one comes back when Cascade quits, when the taskbar is turned off, and on the next start if Cascade crashed.

Pinned apps are stored in `%APPDATA%\Cascade\pinned.txt`, one per line.

## Widgets

- **On the taskbar:** the weather, what's playing (with a play / pause button), CPU, memory, GPU, network speed and a grid button that opens the panel. Put each on the left side, the right side or nowhere, and in any order (Settings → Widgets, or the two lists `taskbar_left` and `taskbar_right` in the config).
- **The panel:** click any of them, or the grid button at the right of the taskbar. It opens above the taskbar's right side with cards for your Windows notifications (dismiss one with its ×, or *Clear all*), what's playing (art, progress, previous / play / next), system stats with live graphs (CPU, memory, GPU, disk, network), the weather and a calendar. Click anywhere else to close it.
- **Customising it** (Settings → Widgets): the Notifications card on or off, where each taskbar widget sits and in what order, the panel's cards and their order (a list like `media,system,weather,calendar`; leave one out to hide it), one or two columns (it goes to two by itself if one would be taller than the screen), and how often stats are read.
- **Light by design:** nothing is read until a stats widget is on screen. The timer stops, and the counters are released, as soon as none is (for example while the taskbar is hidden for a fullscreen game).

## Start menu

Cascade's own Start menu. Windows doesn't let other apps restyle its real one, so Cascade replaces it.

- **Opening it:** tap the **Win key** on its own, or click **Start** or the search box on the taskbar. Shortcuts like Win+E, Win+R and Win+Shift+S work as normal.
- **Over admin apps** (Task Manager, installers): Windows hides keys typed into apps running as administrator from normal apps. Turn on **Work over admin apps** in Settings → Start menu. It asks for permission once, then a tiny helper with admin rights starts with Windows and watches only the Win key. It has no windows and no timers.
- **Left:** a slim icon rail with your account, Settings, File Explorer, your Downloads, Documents, Pictures, Music and Videos folders, network settings, Cascade's settings and power (lock, sleep, sign out, restart, shut down, in a menu drawn by Cascade). The button at the top opens the full sidebar over the menu; it folds away when the pointer leaves. Untick **Compact sidebar** in Settings for the always-wide one.
- **Middle:** pinned apps (the same as your taskbar's) and recently opened files. Turn both off and the column goes away, leaving All apps.
- **Right:** every installed app A to Z, Store apps included. Scrolling is smooth, and click any letter heading to open a grid of letters and jump straight to one.
- **Searching:** just start typing. Use the arrow keys and Enter, or click. Right-click any app to pin or unpin it.

**Customising it** (Settings → Start menu): where it opens (auto, centre or left), its height, how many columns and rows of pinned apps, whether to show the sidebar, recent files and the all-apps list, and the glass tint. Turn off the Win-key takeover or the whole Start menu there too.

## Screens

Turn on **Match the pointer across screens** (Settings → Screens) if you have monitors of different sizes. Windows lines screens up by pixels, so the pointer comes out at a different real-world height after crossing from, say, a 27" screen to a 24" one. Cascade reads each screen's real size (or you can type the diagonal sizes in inches, left to right, like `27,24`) and moves the pointer to where it would be if the screens were lined up the way they sit on your desk. Choose whether they line up at the top, middle or bottom, or leave it on Auto to read it from the Windows arrangement.

It doesn't watch the mouse at all: thin invisible strips along the shared edges let every click through, and Windows only asks them anything while the pointer is inside one. There's nothing running in between, even with a very fast mouse. If the pointer is blocked where your screens really do overlap, drag them so they overlap more in Windows' display settings.

## Settings

Search **Cascade Settings** in Cascade's Start menu or the Windows one, or right-click the notch, taskbar or tray icon and choose **Settings…**. Every option has a toggle, slider, choice or text box, and changes apply immediately. A ↺ button appears next to anything you've changed; it puts that setting back to its default. Starting Cascade again while it's already running also opens Settings.

## Running it

Grab `cascade.exe` from the latest successful **build** run in the Actions tab (artifact `cascade`). Put it anywhere and double-click it. Right-click the notch, an empty part of the taskbar or the tray icon for:

- Settings… / Open Start menu
- Show notch / Show taskbar / Auto-hide taskbar / Replace the Windows taskbar
- Start with Windows
- Hide Windows volume flyout
- Open / reload config (`%APPDATA%\Cascade\config.ini`; focus-timer lengths, weather location, sizes and more)
- Quit

## Updating

Right-click the notch, taskbar or tray icon and choose **Update Cascade**. One click checks GitHub for a newer build, downloads it, swaps the exe and restarts. Cascade also checks every few hours and shows a note in the notch when an update is out.

Every push to the default branch is published as a GitHub release (`build-N`), and that's where the updater looks. While the repo is **private**, GitHub only serves releases to signed-in users. In that case, create a fine-grained token with read-only *Contents* access to this repo and paste it after `github_token =` in the config file. If the repo is public, no token is needed.

To build it yourself, install [Rust](https://rustup.rs) on Windows and run `cargo build --release`. The exe lands in `target\release\cascade.exe`.

## Why it's light

| | Bloom-style (Tauri + React) | Cascade |
|---|---|---|
| UI | WebView2 windows | Direct2D into small layered windows |
| Click-through | Global mouse hook + toggling | Per-pixel alpha: transparent pixels pass clicks |
| Volume | Polled every 64 ms | `IAudioEndpointVolumeCallback` (pushed by Windows) |
| Media | Polled | Media-session events |
| Window list | Polled / hooks | The shell's window notifications (top-level windows only) + debounced rescan |
| Window previews | Captured bitmaps | DWM live thumbnails (composited by Windows) |
| Wi-Fi / Bluetooth | Queried on demand | Radio state-change events |
| Fullscreen | — | App-bar `ABN_FULLSCREENAPP` notification |
| Visualizer | Runs whenever media plays | Runs only while the bars are on screen |
| Weather | — | One HTTPS request per 30 min on a sleeping thread |
| Idle loop | Several polling threads | Blocks in `GetMessage`: 0% CPU |

Frames are only drawn while something is animating; the taskbar clock costs one redraw a minute. Rendering uses Direct2D's software rasteriser, because the surfaces are small and this keeps the GPU asleep. The acrylic blur is done by Windows' own compositor, the same way the real taskbar gets it, so it costs Cascade nothing.

Cascade also never injects code into Explorer or patches Windows files, unlike Explorer mods. It draws its own windows next to Explorer, so Windows updates have nothing of it to break.

## Layout

| File | What it does |
|---|---|
| `src/ui.rs` | App core: windows, message loop, notch states, layout, input and drawing |
| `src/launcher.rs` | Start menu: places, pinned, recent files, all apps, search, power |
| `src/keyhook.rs` | Lone Win key → Start menu |
| `src/settings.rs` | Settings window |
| `src/dock.rs` | Taskbar: window tracking, layout, drawing, drag, launch/focus |
| `src/backdrop.rs` | Acrylic blur behind the taskbar and Start menu |
| `src/systray.rs` | Tray icons from apps, and the hidden-icons flyout |
| `src/mouseedge.rs` | Pointer matching across screens of different sizes |
| `src/usage.rs` | Cascade's own memory and CPU use, for Settings → About |
| `src/helper.rs` | Optional admin helper for the Win key over admin apps |
| `src/sysmon.rs` | CPU, memory, network, GPU and disk readings for the stats widgets |
| `src/widgets.rs` | The widgets panel |
| `src/preview.rs` | Window previews (DWM thumbnails) |
| `src/gfx.rs` | Direct2D/DirectWrite drawing into layered windows |
| `src/spring.rs` | Spring physics behind every animation |
| `src/media.rs` | Now playing + album art (event-driven, own thread) |
| `src/audio.rs` | Volume callbacks, loopback visualizer + FFT |
| `src/brightness.rs` | Brightness events (WMI) and setting (WMI + DDC/CI) |
| `src/radios.rs` | Wi-Fi / Bluetooth toggles |
| `src/weather.rs` | Open-Meteo over WinHTTP |
| `src/pomodoro.rs` | Focus timer |
| `src/icons.rs` | App icons and names |
| `src/update.rs` | One-click updates from GitHub releases |
| `src/http.rs` | Tiny WinHTTP client |
| `src/shell.rs` | Tray, autostart, fullscreen, taskbar, flyout, menus |
| `src/config.rs` | `config.ini` |
