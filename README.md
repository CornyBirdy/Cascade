# Cascade

A tiny native notch, taskbar and Start menu for Windows. Rust and Win32 only: no webview, no runtime, one small `.exe`.

## Notch

The notch lives at the top centre of your main screen. Turn it off in the right-click menu if you don't want it.

- **The lip (default):** at rest it's only a tiny strip of glass along the top edge, in the current theme. Push the pointer up into it and the full notch pulls down; move away and it tucks back up. While music plays the lip's line, in the album art's colour, moves with the music: solid lines reach out from the middle (the whole mix, and the bass brighter on top) over a faint one the whole length (or stays a plain line: untick *Moving lip line* in Settings → Notch), and while the focus timer runs, the timer's progress. Set its length and thickness in Settings → Notch (`lip_width`, `lip_height`, in pixels at 100% scaling).
- **Any edge, anywhere along it:** put it on the top, bottom, left or right edge of the main screen, and slide it anywhere along that edge (Settings → Notch → Edge and Position; `notch_edge`, `notch_position` from 0 to 1). On a side edge it works like a sidebar: the lip stands upright, and the notch slides out of the edge like a drawer, rounded on its free side, with its contents the right way up. It keeps clear of taskbars, and near a corner it grows away from the corner.
- **The classic notch:** set Style to *Notch* (`notch_style = notch`) for the black notch that's always there. While music plays it widens to show album art and a visualizer in the art's colours. While the focus timer runs it shows a progress ring and the time left.
- **Hover to open.** Click a tab in the tab bar along the bottom (or scroll, or swipe sideways on a touchpad) to switch between three modes:
  - **Media:** title, artist, progress bar and play/pause/skip buttons. With nothing playing it shows the time, date, weather and battery instead.
  - **Controls:** Wi-Fi and Bluetooth toggles, a notifications button (opens the widgets panel), and volume and brightness sliders. Brightness works on laptop screens and on external monitors that support DDC/CI.
  - **Calendar:** the current month, plus a focus/break timer (25/5 minutes by default).
- **Volume and brightness changes** show in the notch instead of the Windows flyout. Scroll over the closed notch to change volume.
- **Fullscreen apps and games:** the notch and taskbar hide automatically.

## Themes

One theme styles everything Cascade draws: the notch's lip, the taskbar, the Start menu, the widgets panel, the tray flyout, window previews and Alt+Tab. Pick one in Settings → Appearance (or `theme =` in the config):

| Theme | Look |
|---|---|
| **Acrylic** (default) | Frosted dark glass with a fine edge, lit along the top, like Windows 11. |
| **Liquid Glass** | Apple-style clear glass you can see through (no blur), whose rounded edge bends what's behind it like a lens (see *Refraction* below). With Refraction off: light gathers in its thick edge, splits into colour (a cool fringe on top, a warm one below) and glints on the corners. |
| **Windows Glass** | Aero, as in Windows 7: coloured glass with a glossy top, faint diagonal light streaks and a dark-and-light edge. |
| **Frosted Glass** | Milky, sand-blasted glass that carries light to its edges, where it glows. |
| **Tinted Glass** | Acrylic in your Windows accent colour (follows it when you change it), edged and softly lit in a lighter shade of it. |
| **Translucent** | Barely-there clear glass: no blur, no tint, no outline, only a faint bend of light at its edge. |
| **Rosé Pine** | Deep purple glass edged and lit from iris to love, from the Rosé Pine palette. |
| **Neon** | Near-black glass whose cyan-to-magenta edge gives off light, glowing into the glass. |
| **Obsidian** | Polished solid black: it reflects the room, with a bevelled, lit edge. No blur. |
| **Clean Slate** | Matte, flat slate grey, softly lit from above. No blur, no shine. |

Each theme is a material, not just a colour: the kind of glass (frosted acrylic, a blur, clear glass with no blur at all, or none), how it's washed, how its edge is drawn, and how light passes through it. Thick glass bends light round its rounded edge, so it gathers in a band that fades smoothly into the glass; frosted glass glows at its edges; Aero has its gloss and streaks; polished stone reflects; matte slate is lit from above. Settings → Appearance shows a sample of each over a colourful backdrop: click one to use it. The glass stays on while panels slide and the notch opens, not just once they've settled.

Everything about the look is on that one page:

- **Colour → Based on:** the theme's own colours, your Windows accent, none (grey glass), or **Custom**, picked on the **Custom colour** slider. Windows Glass is blue only if you want it to be. A theme's other shades turn with it, so Rosé Pine and Neon keep their two-colour edges. (`theme_color = theme | accent | neutral | custom`, `theme_hue` in degrees.)
- **Colour intensity:** from 0% (grey) through 100% (as the theme was made) to 200% (`theme_intensity`, 0 to 2).
- **Refraction:** clear glass (Liquid Glass, Translucent) bends what's behind it round its rounded edge, the way thick glass does: near the edge the background is squeezed and pulled round the curve, its colours split a little, so the edge shows without any outline drawn. Cascade reads the screen just outside each pane's edge (never under it, where a screen copy would see the pane itself), folds it in over the edge as a thin ring under the pane, and refreshes it about 20 times a second, redrawing only when something behind changed. The middle of the pane stays truly see-through. Turn it off for the lightest Cascade (`refraction`).
- **Glass:** **Blur**, **Corners**, **Tint** (taskbar, panels and the lip), **Start menu tint**, and **Edge**: how strongly the glass's edge catches the light, from 0% (no outline at all) to 200% (`theme_edge`), on top of any theme.

## Taskbar

A Windows 11-style taskbar drawn by Cascade itself, over a real acrylic blur.

- **Left:** the current weather. **Centre:** Start, a search box, then your pinned and running apps. **Right:** hidden icons, network / volume / battery, and the clock.
- **App buttons** work like Windows: a short line marks apps with open windows and a longer blue one the app in front. Hover for a name and live window previews; click a preview to switch to it, or its × to close it.
- **Click an app** to open it if it isn't running, bring it to the front if it is, minimise it if it's already in front, or cycle through its windows. **Middle-click** opens a new window. **Right-click** to pin, unpin, close, or force close (Task Manager's *End task*). **Drag** pinned apps to reorder them.
- **Tray icons:** apps' notification-area icons show by the clock, the same ones you chose to show in Windows' taskbar settings. The rest are under the **^** button. Click and right-click them as usual. Cascade gets them the way alternative taskbars like RetroBar do: it keeps its own hidden tray window and passes everything on to Explorer, without touching Explorer itself.
- **Clicks on the right:** the status icons pull the notch down on its Controls tab (Wi-Fi, Bluetooth, volume, brightness), so there's one controls area instead of two. It stays open until you click elsewhere, or tucks away after a few seconds if you never move onto it. Without a notch (or with `status_click = windows`) they open Windows' quick settings instead. The clock opens the widgets panel with your notifications.
- **Look:** full width or floating islands, centred or left-aligned, the Start button with your apps or alone on its own island at the far left, icon size, icon padding, bar padding (the gap from the bar's edge to its buttons on all four sides, which together set the bar's height) and the gap from the screen edge (all three default to the same small gap), and which parts to show, in Settings → Taskbar. The theme, blur, corners and tint are in Settings → Appearance.
- **Every screen:** with more than one screen, each gets a taskbar of its own with Start, search, your apps and the clock (the widgets, tray and status icons stay on the main screen). Start, the widgets panel and Win+X open on the screen the pointer is on. Each taskbar shows every open window, or only the ones on its own screen (Settings → Taskbar → Other screens; `taskbar_screens`, `taskbar_screen_apps`, `taskbar_screen_clock`). Pinning or reordering on one does the same on all of them, and a fullscreen game only hides the taskbar on its own screen.
- **Auto-hide** (optional): push the mouse to the bottom edge to bring it back.
- **Replaces the Windows taskbar** while it's on (untick *Replace the Windows taskbar* to keep both). The real one comes back when Cascade quits, when the taskbar is turned off, and on the next start if Cascade crashed.

Pinned apps are stored in `%APPDATA%\Cascade\pinned.txt`, one per line.

## Widgets

- **On the taskbar:** the weather, what's playing (with a play / pause button and a little equaliser), CPU, memory, GPU, network speed and a grid button that opens the panel. Put each on the left side, the right side or nowhere, and in any order: in Settings → Widgets, drag their tiles between the Left, Right and Hidden lanes, or click a tile to show or hide it (or edit the two lists `taskbar_left` and `taskbar_right` in the config).
- **The panel:** click any of them, or the grid button at the right of the taskbar. It opens above the taskbar's right side with cards for your Windows notifications (click one to open the app it came from, dismiss one with its ×, or *Clear all*), what's playing (art, an equaliser under it, progress, previous / play / next), system stats with live graphs (CPU, memory, GPU, disk, network), the weather and a calendar. Click anywhere else to close it.
- **Customising it** (Settings → Widgets): the music visualizer on the music widget and card on or off (`widget_visualizer`), the Notifications card on or off, where each taskbar widget sits and in what order, the panel's cards and their order (drag the tiles; `widgets_panel` in the config), one or two columns (it goes to two by itself if one would be taller than the screen), and how often stats are read.
- **Weather location** (Settings → Focus & weather): a town (`Adelaide`), `Town, Country` to pick between places with the same name (`Adelaide, Australia`), or `lat,lon`; leave it empty to go by your IP address. The row underneath says what it found (*Found: Adelaide, South Australia, Australia*) or that it found nothing.
- **Light by design:** nothing is read until a stats widget is on screen. The timer stops, and the counters are released, as soon as none is (for example while the taskbar is hidden for a fullscreen game).

## Start menu

Cascade's own Start menu. Windows doesn't let other apps restyle its real one, so Cascade replaces it.

- **Opening it:** tap the **Win key** on its own, or click **Start** or the search box on the taskbar. Shortcuts like Win+E, Win+R and Win+Shift+S work as normal.
- **Over admin apps** (Task Manager, installers): Windows hides keys typed into apps running as administrator from normal apps. Turn on **Work over admin apps** in Settings → Start menu. It asks for permission once, then a tiny helper with admin rights starts with Windows and watches only the Win key. It has no windows and no timers.
- **Left:** a slim icon rail with your account, Settings, File Explorer, your Downloads, Documents, Pictures, Music and Videos folders, network settings, Cascade's settings and power (lock, sleep, sign out, restart, shut down, in a menu drawn by Cascade). The button at the top opens the full sidebar over the menu; it folds away when the pointer leaves. Untick **Compact sidebar** in Settings for the always-wide one.
- **Middle:** pinned apps (the same as your taskbar's) and recently opened files. Turn both off and the column goes away, leaving All apps.
- **Right:** every installed app A to Z, Store apps included. Scrolling is smooth, and click any letter heading to open a grid of letters and jump straight to one.
- **Searching:** just start typing. Use the arrow keys and Enter, or click. Right-click any app to pin or unpin it.

**Customising it** (Settings → Start menu): where it opens (auto, centre or left), its height, how many columns and rows of pinned apps, and whether to show the sidebar, recent files and the all-apps list. Its glass tint is in Settings → Appearance. Turn off the Win-key takeover or the whole Start menu there too.

## Shortcuts

Cascade's own versions of Windows' shortcut menus, in the current theme and where Windows puts them. Each can be turned off in Settings → Shortcuts, and then the key does what it always did; every other Win+key shortcut is left alone.

- **Win+X** (or right-click **Start**): the quick links menu above the Start button: Installed apps, Power Options, Event Viewer, System, Device Manager, Network Connections, Disk Management, Computer Management, Terminal, Terminal (Admin), Task Manager, Settings, File Explorer, Search, Run, *Shut down or sign out* and Desktop (plus Mobility Center on a laptop). The same keys work as in Windows', shown beside each item: Win+X then A for an admin terminal, or U then R to restart. Arrow keys, Enter and Esc work too. (`shortcut_win_x`)
- **Win+N:** the widgets panel, with your notifications and the calendar (`shortcut_win_n`; needs Cascade's taskbar).
- **Win+A** (off by default): the notch's Controls instead of Windows' quick settings (`shortcut_win_a`).

## Screens

Turn on **Match the pointer across screens** (Settings → Screens) if you have monitors of different sizes. Windows lines screens up by pixels, so the pointer comes out at a different real-world height after crossing from, say, a 27" screen to a 24" one. Cascade reads each screen's real size (or you can type the diagonal sizes in inches, left to right, like `27,24`) and moves the pointer to where it would be if the screens were lined up the way they sit on your desk. Choose whether they line up at the top, middle or bottom, or leave it on Auto to read it from the Windows arrangement.

It doesn't watch the mouse at all: thin invisible strips along the shared edges let every click through, and Windows only asks them anything while the pointer is inside one. There's nothing running in between, even with a very fast mouse. If the pointer is blocked where your screens really do overlap, drag them so they overlap more in Windows' display settings.

## Settings

Search **Cascade Settings** in Cascade's Start menu or the Windows one, or right-click the notch, taskbar or tray icon and choose **Settings…**. Every option has a toggle, slider, choice or text box, and changes apply immediately. A ↺ button appears next to anything you've changed; it puts that setting back to its default. Settings that wouldn't do anything as things are set up stay out of the way: the lip's length shows only in Lip style, the notch's width only in Notch style, the custom colour only with Custom picked, the Start menu's parts only with Cascade's Start menu on, and so on. They come back as soon as they'd make a difference. When one needs something on another page (Win+N needs Cascade's taskbar), its description says so, in amber. Starting Cascade again while it's already running also opens Settings.

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
| Themes | — | A DWM blur style and a few gradients per panel; nothing extra runs |
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
| `src/quickmenu.rs` | The Win+X menu |
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
