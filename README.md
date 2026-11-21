# Cascade

A tiny native notch, taskbar and Start menu for Windows. Rust and Win32 only: no webview, no runtime, one small `.exe`.

## Notch

The notch lives at the top centre of your main screen. Turn it off in the right-click menu if you don't want it.

- **The lip (default):** at rest it's only a tiny strip of glass along the top edge, in the current theme. Push the pointer up into it and the full notch pulls down; move away and it tucks back up. While music plays the lip's line, in the album art's colour, moves with the music: solid lines reach out from the middle (the whole mix, and the bass brighter on top) over a faint one the whole length (or stays a plain line: untick *Moving lip line* in Settings → Notch), and while the focus timer runs, the timer's progress. Set its length and thickness in Settings → Notch (`lip_width`, `lip_height`, in pixels at 100% scaling).
- **Any edge, anywhere along it:** put it on the top, bottom, left or right edge of the main screen, and slide it anywhere along that edge (Settings → Notch → Edge and Position; `notch_edge`, `notch_position` from 0 to 1). On a side edge it works like a sidebar: the lip stands upright, and the notch slides out of the edge like a drawer, rounded on its free side, with its contents the right way up. It keeps clear of taskbars, and near a corner it grows away from the corner.
- **The classic notch:** set Style to *Notch* (`notch_style = notch`) for the black notch that's always there. While music plays it widens to show album art and a visualizer in the art's colours. The visualizer listens to the app that's playing, not everything the PC plays, so a Discord call or a game alongside the music doesn't move it (Windows 10 version 2004 or later; before that, or when the app can't be told, it hears the whole mix). While the focus timer runs it shows a progress ring and the time left.
- **Hover to open.** Click a tab in the tab bar along the bottom (or scroll, or swipe sideways on a touchpad) to switch between three modes:
  - **Media:** title, artist, progress bar and play/pause/skip buttons. With nothing playing it shows the time, date, weather and battery instead; while music plays the battery sits at the left of the buttons. Click the battery's percentage to swap it for how long it will last (Windows only knows that on battery).
  - **Controls:** Wi-Fi and Bluetooth toggles, a notifications button (opens the widgets panel), and volume and brightness sliders. Brightness works on laptop screens and on external monitors that support DDC/CI. Under them, a volume mixer: the output device (click it for the list, and pick another to make it the default, as Windows' own picker does), then each app playing sound with its own slider (click its name to mute it). It's read as Controls opens, not watched (`controls_mixer`). The Wi-Fi tile names the network you're on; its **›** opens the list of networks in range (signal, a lock on secured ones, which are saved, the one you're on first) to join one with a click: a saved or open network joins straight away, and one that needs a password Windows has never had opens Windows' own list to type it in. Under the list, *Wi-Fi settings*, *Phone hotspot* (opens Windows' own network list, where Phone Link's Instant Hotspot for your phone shows with its Connect button; Windows has no way for another program to start that connection), and *Quick settings* for anything Controls doesn't do itself. The Bluetooth tile works the same way: it names the device that's connected, and its **›** opens your paired devices, connected ones first. Click headphones, speakers or earbuds to connect or disconnect them (Cascade asks the Bluetooth audio driver, as Windows' own Connect button does); mice, keyboards and the like connect by themselves, so clicking one opens Windows' Bluetooth settings.
  - **Calendar:** the current month, plus a focus/break timer (25/5 minutes by default). Opening the tab plays a short entrance: the days rise into place one after another, today's circle and the event dots pop in, the agenda slides in, and the timer's ring draws itself round. Give Cascade your calendar's private iCal link (Settings → Focus & weather → Calendar link; Google Calendar calls it the *Secret address in iCal format*, Outlook lets you publish one) and days with events get a dot, the next few events this week are listed under the month, and in the minutes before each one (10 by default, `calendar_soon`) the closed notch counts down to it: the lip's line fills up, the classic notch shows the event and the minutes left, and it says so when it's coming up and when it starts. Repeating events, moved and cancelled ones are followed. The link is read every 15 minutes on a sleeping thread (`calendar_url`).
- **Volume and brightness changes** show in the notch instead of the Windows flyout. Scroll over the closed notch to change volume.
- **Plugging the charger in or out** (on a laptop) shows in the notch too: the battery, a bolt on it while it's plugged in, and how full it is (`charge_osd`). The taskbar's and the notch's battery icons get the same bolt while it's plugged in, and change the moment it goes in or out.
- **USB devices:** plug in a flight controller and the notch says *Flight controller (STM32) on COM5* with an *Open Betaflight* button (it opens the web configurator, which talks to the board over Web Serial). It knows STM32 and AT32 flight controllers (also in DFU mode), ArduPilot boards, EdgeTX/OpenTX radios plugged in as a joystick, the USB-serial chips on ExpressLRS gear, ESP32 boards, Meta Quest headsets, DJI gear and Android phones. Name your own devices and pick what clicking opens in `devices.txt` (Settings → Notch → *Your devices*), set it to announce any device, and choose whether unplugging shows too (`usb_popups = known | all | off`, `usb_unplug`). Windows tells Cascade when a device comes or goes; nothing is polled.
- **Never the thing that lags:** Cascade runs at High priority (Settings > General > Priority, `priority = high | above_normal | normal`), out of Windows 11's efficiency mode, with a floor on its memory so a game doesn't page it out, and its drawing ahead on the GPU where Windows allows. Anything slow (Open boxes, shell menus, copying, permission prompts) runs on threads of its own.
- **Fullscreen apps and games:** the notch and taskbar hide automatically, and come back by themselves if Windows never says the app has gone (every few seconds Cascade checks one is still in front). A see-through, click-through overlay over the whole screen (Claude's computer-use frame, a screen recorder's border) isn't taken for a fullscreen app, so Cascade stays. Want the notch anyway? *Notch over fullscreen apps* (Settings → General, `notch_fullscreen`) keeps it on top of them, games included, while the taskbar still gets out of the way. Pressing Win over a fullscreen app brings the taskbar up over it with the Start menu.
- **Auto ducker** (Settings → General, `ducker`, off by default): while one app is making sound, such as Discord with your friends talking (*When this app makes sound*: click it for the apps that have sound right now and tick one or more; `ducker_trigger`), the apps you pick (*Turn these apps down*, or *Every other app*; `ducker_targets`) fade down to a share of their own volume (`ducker_level`) and come back once it has been quiet for a while (`ducker_release`). How loud counts as making sound is up to you (*Counts as sound above*, -60 to -10 dB, `ducker_threshold`; lower hears quiet voices too). A volume you change by hand while they're down becomes the new normal, and everything is put back when the ducker is turned off or Cascade closes. It looks at Windows' audio sessions a few times a second on a thread of its own, and only while it's turned on.
- **Battery saver** (on a laptop; Settings → General, `battery_saver`, off by default): while the charger is out, the visualizer bars are off and the PC's stats are read at most every 5 seconds. Plugging in brings everything back; your settings file isn't changed.

## Themes

One look styles everything Cascade draws: the notch's lip, the taskbar, the Start menu, the widgets panel, the tray flyout, window previews and Alt+Tab. Pick it in Settings → Personalisation (or `theme =` in the config):

| Theme | Look |
|---|---|
| **Acrylic** (default) | Frosted dark glass with a fine edge, lit along the top, like Windows 11. Neutral grey: any colour in it is your accent's, if you want it. |
| **Liquid Glass** | Apple's liquid glass: clear rather than frosted (what's behind is seen sharp, through a dark tint the Tint slider sets), a fine lit rim, a faint sheen, and an edge that bends what's around it like a lens, its colours split. |

**Refraction** (Settings > Personalisation, `refraction`, Liquid Glass only): within 22 px of a pane's rim, what's around it is bent in by up to 35 px, falling off with the square of the distance from the rim, as the web recipe's displacement map does, with its colours split very slightly. Windows can't show what's under a pane to the pane itself, so Cascade reads the screen just outside each pane's edge and folds it in, in a click-through window between the blur and the pane: a few times a second, only the strips round the edge, only redrawn when what's there changed, less and less often while it keeps still, and never more than a few milliseconds at a time. Where a pane sits against the edge of the screen it fades out. Off, the edge is drawn as a band of light instead, which costs no more than Acrylic. Settings > Personalisation > Edge sets how far it bends.

Everything about the look is on that one page:

- **Accent colour:** the highlights (the app in front, toggles, progress, today) in your Windows accent colour (the default, and followed when you change it), or one picked from Windows' own palette of accent colours (`accent = windows` or `#RRGGBB`).
- **Accent colour on the glass:** tints the taskbar, Start menu, panels and the lip with it too, as Windows' *Show accent colour on Start and taskbar* does (`accent_glass`). Off, the glass is plain grey.
- **Glass:** **Blur**, **Corners**, **Tint** (taskbar, panels and the lip), **Start menu tint**, and **Edge**: how strongly the glass's edge catches the light, from 0% (no outline at all) to 200% (`theme_edge`).

The themes there used to be (Windows Glass, Frosted, Tinted, Translucent, Rosé Pine, Neon, Obsidian, Clean Slate) are gone; a config naming one gets Acrylic.

## Taskbar

A Windows 11-style taskbar drawn by Cascade itself, over a real acrylic blur.

- **Left:** the current weather. **Centre:** Start, a search box, then your pinned and running apps. **Right:** hidden icons, network / volume / battery (with the charge level as a number beside the battery icon if you turn on *Battery percentage* in Settings → Taskbar, `battery_percent`), and the clock.
- **App buttons** work like Windows: a short line marks apps with open windows and a longer blue one the app in front. Hover for a name and live window previews (they rise in from the taskbar, and glide over to the next app as you move along); click a preview to switch to it, or its × to close it. Rest on a preview and only that window shows (the others turn to glass), so you can see where it is, as Windows' own taskbar does (`taskbar_peek`). The name and previews go as soon as the pointer is off them, even when Windows doesn't say so (a menu or another window took the mouse).
- **Drop files on an app** to open them with it; hold them over an app with windows for a moment and its window comes up, so you can drop into it.
- **Click an app** to open it if it isn't running, bring it to the front if it is, minimise it if it's already in front (its button dips as it takes the window in, and hops as one comes back out), or cycle through its windows. **Middle-click** opens a new window. **Right-click** to pin, unpin, close, or force close (Task Manager's *End task*). **Drag** apps, pinned or running, to reorder them: the one you hold follows the pointer and the others slide out of its way. A click goes to the app under the pointer at that moment, even when the buttons have just shifted (an app opened or closed).
- **Tray icons:** apps' notification-area icons show by the clock, the same ones you chose to show in Windows' taskbar settings. The rest are under the **^** button. Click and right-click them as usual. Cascade gets them the way alternative taskbars like RetroBar do: it keeps its own hidden tray window and passes everything on to Explorer, without touching Explorer itself.
- **Clicks on the right:** the status icons pull the notch down on its Controls tab (Wi-Fi, Bluetooth, volume, brightness), so there's one controls area instead of two. It stays open until you click elsewhere, or tucks away after a few seconds if you never move onto it. With the notch turned off, Controls come down on their own and go again once they close; with `status_click = windows` they open Windows' quick settings instead. The clock opens the widgets panel with your notifications. The notch's own clock can count the seconds (Settings > Notch > Seconds on the clock, `clock_seconds`).
- **Look:** full width or floating islands, centred or left-aligned, the Start button with your apps or alone on its own island at the far left, icon size, icon padding, bar padding (the gap from the bar's edge to its buttons on all four sides, which together set the bar's height) and the gap from the screen edge (all three default to the same small gap), and which parts to show, in Settings → Taskbar. The theme, blur, corners and tint are in Settings → Personalisation.
- **Every screen:** with more than one screen, each gets a taskbar of its own with Start, search, your apps and the clock (the widgets, tray and status icons stay on the main screen). Start, the widgets panel and Win+X open on the screen the pointer is on. Each taskbar shows every open window, or only the ones on its own screen (Settings → Taskbar → Other screens; `taskbar_screens`, `taskbar_screen_apps`, `taskbar_screen_clock`). Pinning or reordering on one does the same on all of them, and a fullscreen game only hides the taskbar on its own screen.
- **Auto-hide** (optional): push the mouse to the bottom edge to bring it back.
- **Replaces the Windows taskbar** while it's on (untick *Replace the Windows taskbar* to keep both). The real one comes back when Cascade quits, when the taskbar is turned off, and on the next start if Cascade crashed.

Pinned apps are stored in `%APPDATA%\Cascade\pinned.txt`, one per line.

## Widgets

- **On the taskbar:** the weather, what's playing (with a play / pause button and a little equaliser), CPU, memory, GPU, network speed, your virtual desktops (a dot each, or a numbered box each with `desktops_style = numbers`, the one you're on lit, shown with more than one: click for Task View, scroll to go to the next or previous one) and a grid button that opens the panel. Put each on the left side, the right side or nowhere, and in any order: drag a widget along the taskbar itself (past the apps to the other side), or in Settings → Widgets, drag their tiles between the Left, Right and Hidden lanes, or click a tile to show or hide it (or edit the two lists `taskbar_left` and `taskbar_right` in the config).
- **The panel:** click any of them, or the grid button at the right of the taskbar. It opens above the taskbar's right side with cards for your Windows notifications (click one to open the app it came from, dismiss one with its ×, or *Clear all*), what's playing (art, an equaliser under it, progress, previous / play / next), system stats with live graphs (CPU, memory, GPU, disk, network; click the card for Task Manager), the weather, a calendar (step through the months with its arrows, click its title to come back to this one; days with events have a dot, and clicking a day lists what's on it, as today's are listed when there are any), and (if you add them) the clipboard (Windows' clipboard history: click one to copy it again, or *Clear*) and your phone: the notifications it mirrored through Phone Link and a button to open Phone Link. The Notifications and Clipboard cards show five at a time: **scroll** over one for the rest (a thin bar at its side shows where you are). **Drag a card** (anywhere on it that isn't a button) to move it: the others make room, and the new order is kept (`widgets_panel`); the Notifications card stays on top. Click anywhere else to close it. Clicking the Snipping Tool's notification opens the screenshot you just took in the Snipping Tool.
- **Your phone (Phone Link):** music playing on your Android phone, relayed by Phone Link (its *Audio* feature) or Windows' built-in phone link, shows in the notch and on the Media card like any other player, marked *Phone*. If it doesn't, `cascade.log` lists the media sessions Windows offers (*media sessions: ...*) each time they change. With more than one thing to show (the phone and a browser, say), a switch button at the right of the notch's media controls steps between them.
- **Customising it** (Settings → Widgets): the music visualizer on the music widget and card on or off (`widget_visualizer`), the Notifications card on or off, where each taskbar widget sits and in what order, the panel's cards and their order (drag the tiles; `widgets_panel` in the config), one or two columns (it goes to two by itself if one would be taller than the screen, and then opens that way next time, going by how tall it was when its stats and notifications last loaded, rather than opening narrow and snapping wide once they have), and how often stats are read.
- **Weather location** (Settings → Focus & weather): a town (`Adelaide`), `Town, Country` to pick between places with the same name (`Adelaide, Australia`), or `lat,lon`; leave it empty to go by your IP address. The row underneath says what it found (*Found: Adelaide, South Australia, Australia*) or that it found nothing.
- **NVIDIA cards, each on its own:** with an NVIDIA card (or two), the GPU widget shows a column per card with its load and temperature (*GPU 1 · 54°* over *37%*), and the panel gives each card a row with its name, video memory, power draw, load, temperature and a graph. The readings come from the driver's own `nvml.dll`, loaded only while a GPU reading is on screen; other cards keep the single Windows figure. Settings → Widgets → *Graphics cards* picks which cards show (all, or just GPU 1 or GPU 2; `gpu_cards`), whether the taskbar shows each card's temperature (`gpu_temp`), whether its figure is the load, video memory or power draw (`gpu_value`), and whether cards go by number or name (`gpu_label`).
- **Light by design:** nothing is read until a stats widget is on screen. The readings are taken on a thread of their own, so a slow counter never holds up an animation, and it stops and releases the counters as soon as none is (for example while the taskbar is hidden for a fullscreen game).

## Start menu

Cascade's own Start menu. Windows doesn't let other apps restyle its real one, so Cascade replaces it.

- **Opening it:** tap the **Win key** on its own, or click **Start** or the search box on the taskbar. Shortcuts like Win+E, Win+R and Win+Shift+S work as normal, and so do window-drag tools: holding Win while the mouse drags a window doesn't open it when you let go. The key watcher runs on a thread of its own, so a busy moment elsewhere in Cascade never holds up your typing.
- **Over admin apps** (Task Manager, installers): Windows hides keys typed into apps running as administrator from normal apps. Turn on **Work over admin apps** in Settings → Start menu. It asks for permission once, then a tiny helper with admin rights starts with Windows and watches only the Win key. It has no windows and no timers.
- **Left:** a slim icon rail with your account, Settings, File Explorer, your Downloads, Documents, Pictures, Music and Videos folders, network settings, Cascade's settings and power (lock, sleep, sign out, restart, shut down, in a menu drawn by Cascade). The button at the top opens the full sidebar over the menu; it folds away when the pointer leaves. Untick **Compact sidebar** in Settings for the always-wide one.
- **Middle:** pinned apps (the same as your taskbar's) and recently opened files. Turn both off and the column goes away, leaving All apps.
- **Right:** every installed app A to Z, Store apps included. Scrolling is smooth, and click any letter heading to open a grid of letters and jump straight to one.
- **Searching:** just start typing. Use the arrow keys and Enter, or click. Right-click any app to pin or unpin it. It answers more than apps: sums (`12*4+3`, `2^10`, `sqrt(2)`; Enter copies the answer), unit conversions (`10 km to mi`, `180 lb in kg`, `25 c to f`, `1 gb in mb`; lengths, weights, volumes, speeds, data, time and temperature), quick commands (`lock`, `restart`, `shut down`, `bluetooth`, `display`, `task manager`...) and, last, a web search for what you typed (Google, Bing, DuckDuckGo or none, in Settings → Start menu; `start_web_search`).

**Customising it** (Settings → Start menu): where it opens (auto, centre or left), its height, how many columns and rows of pinned apps, and whether to show the sidebar, recent files and the all-apps list. Its glass tint is in Settings → Personalisation. Turn off the Win-key takeover or the whole Start menu there too.

## Alt+Tab

Cascade's own window switcher, in the current theme, fading and sliding in as it opens and out once you've picked. Hold Alt and press Tab (Shift+Tab goes back) or use the arrow keys, Up and Down too; let go of Alt to switch. Four looks (Settings → General → *Alt+Tab look*, `alt_tab_style`): **Cards** with live previews (the default), **Large** previews, **Icons** (big app icons with the picked window's title under them, as GNOME's switcher) and **List** (a column of icons and titles, as KDE's compact one). Games and fullscreen video keep Windows' own.

## Shortcuts

Cascade's own versions of Windows' shortcut menus, in the current theme and where Windows puts them. Each can be turned off in Settings → Shortcuts, and then the key does what it always did; every other Win+key shortcut is left alone.

- **Win+X** (or right-click **Start**): the quick links menu above the Start button: Installed apps, Power Options, Event Viewer, System, Device Manager, Network Connections, Disk Management, Computer Management, Terminal, Terminal (Admin), Task Manager, Settings, File Explorer, Search, Run, *Shut down or sign out* and Desktop (plus Mobility Center on a laptop). The same keys work as in Windows', shown beside each item: Win+X then A for an admin terminal, or U then R to restart. Arrow keys, Enter and Esc work too. (`shortcut_win_x`)
- **Super drag:** hold **Win** and drag a window by any part of it, not just its title bar (Settings → General, `super_drag`, on by default). The window comes to the front and follows the pointer; the click isn't passed on to the app, and letting go of Win afterwards doesn't open Start. Maximised windows, programs running as administrator (Cascade can't move those), the desktop, the taskbar and Cascade's own windows are left alone, and so is any Win+click that isn't on a movable window.
- **Win+N:** the widgets panel, with your notifications and the calendar (`shortcut_win_n`; needs Cascade's taskbar).
- **Win+A** (off by default): the notch's Controls instead of Windows' quick settings (`shortcut_win_a`).
- **Win+Tab:** Task View in Cascade's look: this desktop's windows as live previews to pick from (click, or the arrow keys and Enter; Esc leaves), and under them your virtual desktops, the one you're on lit: click one to go there, point at one and click its **×** to close it (its windows move to the next desktop), or **+** for a new one (`shortcut_win_tab`). Each desktop in the strip is a small picture of it, as in Windows' own Task View: your wallpaper, with its windows where they are on the screen as little windows (a title bar, a shadow, the app's icon; the one in front edged brighter). Rest the pointer on one, or carry a window card over it, and a list comes up over it of the apps that have a window there. **Drag a window's card onto a desktop** to send that window there: the card is carried along under the pointer, the desktop it would land on lights up, and Esc puts it back. Windows has no public way to move another app's window, so Cascade asks Explorer the way Explorer does; on a Windows build where that doesn't work the card stays put and `cascade.log` says why. It slides and fades in as it opens and out as it closes.
- **Switching desktops** (Ctrl+Win+Left/Right, the desktops widget, Task View) shows your desktops in the middle of the screen for a moment, as GNOME and KDE do: a numbered tile each, the light sliding to the one you're on, its name under it (`desktop_osd`).
- **Win+V:** your clipboard history by the text cursor (or the pointer, in apps that don't say where their cursor is): Windows' own history, so pinned and synced items are there too. Click an item, press its number, or pick it with the arrow keys and Enter, and it's pasted into the app you were in; Delete or its × takes it out, *Clear all* empties it. With clipboard history off in Windows it offers to turn it on. It's only read while the menu is open (`shortcut_win_v`). Windows only shares its history with the app in front, so Cascade brings the menu forward and asks again; if Windows still won't, the menu shows Cascade's own history instead: what was copied while Cascade was running (text and pictures, up to 25, kept in memory only, never what an app marks as private, such as a password manager's copies). Windows says when the clipboard changes; nothing is polled.

## Screens

Turn on **Match the pointer across screens** (Settings → Screens) if you have monitors of different sizes. Windows lines screens up by pixels, so the pointer comes out at a different real-world height after crossing from, say, a 27" screen to a 24" one. Cascade reads each screen's real size (or you can type the diagonal sizes in inches, left to right, like `27,24`) and moves the pointer to where it would be if the screens were lined up the way they sit on your desk. Choose whether they line up at the top, middle or bottom, or leave it on Auto to read it from the Windows arrangement.

It doesn't watch the mouse at all: thin invisible strips along the shared edges let every click through, and Windows only asks them anything while the pointer is inside one. There's nothing running in between, even with a very fast mouse. If the pointer is blocked where your screens really do overlap, drag them so they overlap more in Windows' display settings.

## Shell mode (experimental)

Normally Cascade runs beside Explorer: it hides the Windows taskbar and keeps the Win key from Windows' Start menu. Turn on **Run as the Windows shell** (Settings → General) and Windows starts Cascade *instead of* Explorer. Explorer's taskbar and Start menu are then never made: there's nothing to hide and nothing to beat to the Win key. File Explorer windows open as usual; only Explorer's part as the shell is gone.

**Switching without signing out.** Turning the setting on asks whether to switch now: say yes and Explorer closes (any File Explorer windows with it; its virtual desktops are put together into one first, so no window is left hidden) and Cascade takes over on the spot. Say no and it happens at your next sign-in. Turning it off while Cascade is the shell asks the same the other way round: Explorer comes back now and Cascade starts again beside it.

As the shell, Cascade takes on the jobs Explorer had:

- **The desktop and wallpaper:** Cascade draws Windows' own wallpaper setting (the picture and how it fits each screen; Span across them too), and Wallpaper Engine and Lively put theirs on it as usual. **Desktop icons** as Explorer shows them: what's in your Desktop folder and the public one, and the Windows icons you have turned on (only the Recycle Bin, unless you've turned on This PC, your user folder, Network or Control Panel; Libraries, Linux, a linked phone and the like stay in File Explorer's sidebar, as on Explorer's desktop). Double-click to open, right-click for the item's own menu (the same one Explorer shows, Open with and Send to included), drop files on the desktop to put them there. Drag over the wallpaper to select several, Ctrl+click and Shift+click to add, and the keys: Enter, Delete (to the Recycle Bin; Shift+Delete for good; a Windows icon such as This PC, picked on its own, is taken off the desktop instead), F2 to rename, Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V, the arrows, F5. Right-click the desktop for Windows 11's menu: View (Show desktop icons, and which Windows icons show), Sort by (name, size, item type, date modified; `desktop_sort`), Refresh, Paste, Paste shortcut, New (folder, shortcut, text document; a new one is selected with its name ready to type), Display settings, Personalise, the wallpaper and how it fits, Open in Terminal, Task Manager and Cascade's settings. Cascade's own menu is only on the taskbar and the notch. As Explorer's, the icon layer covers the whole desktop just above the wallpaper, never over apps, so a wallpaper app's window never sets the pointer (no busy cursor over Wallpaper Engine). Icons are arranged by name (not where you drag them). Alt+F4 on the desktop asks what the PC should do.
- **Files**: Cascade's own file manager (Start > Files, Win+E and every folder in shell mode; Settings > General > Open folders in). Tabs; back, forward, up; an address box for paths, `shell:` names and This PC; a search box that filters as you type and on Enter searches every folder below (instantly with Instant file search on); a sidebar with your folders, drives (with free space), the Recycle Bin and Network; details or icons with thumbnails; drag a box, Ctrl/Shift+click; F2 renames in place; the shell's own menus on items (Open with, Send to, 7-Zip...); New, Paste, Sort, View, Open in Terminal and Properties on the background; copy, cut, paste, delete; files dragged in and out; the folder follows changes on disk. Explorer's keys work (Ctrl+T/W/Tab, Alt+arrows, Backspace, Ctrl+L, Ctrl+F, F5, Ctrl+Shift+N...). Its own process, at normal priority, so it can never take the taskbar down with it.
- **Photos**: a picture viewer for shell mode, where Windows' Photos (a Store app) can't open (Settings > General > Open pictures in). Whatever Windows' codecs read; the camera's orientation; zoom at the pointer, drag, fit or 100%; the folder's other pictures with the arrows; rotate and save the rotation; fullscreen; delete, copy, Open with, Set as wallpaper.
- **Instant file search** (Settings > Start menu): every file and folder on your NTFS drives, found as you type in Start and Files, from the drives' own file tables, kept current from their change journals (as Everything does). Asks for permission once (a small helper reads the drives with admin rights); about 45 MB per million files.
- **Disk Usage** (Start, or Settings > System > Storage): what takes the space, as a map of rectangles with the biggest listed beside it; click into folders; open, show in Files or delete. Instant with the file index, otherwise counted on a thread of its own.
- **Debloat** (Settings > Debloat): switches for telemetry, Copilot, Recall, suggested and sponsored apps, lock screen tips, web results in Windows search, feedback requests, typing data, activity history, Edge in the background and sharing updates with other PCs. Each only writes a setting; turning it off puts Windows' default back; nothing is uninstalled.
- **Windows' settings, on Cascade's pages** (Settings, below a rule in the sidebar), as full as Windows' own Settings app, so nothing needs it: **Display** (the screens laid out as they sit on your desk: drag one into place, where it snaps against the others, or click one to choose it; Identify; multiple screens, main screen, scale, resolution and refresh rate, kept only if you say so, orientation, HDR, advanced display details, and a night light of Cascade's own with strength and schedule), **Sound** (output and input; for each device its own volume, balance, mute, format (sample rate and bit depth), default and default-for-calls, turning it off and back on; mono audio; what other sounds do during calls), **Network & internet** (Wi-Fi and its properties, metered connection, airplane mode, joining and forgetting networks; for each adapter its details, IP assignment (DHCP or an address, mask and gateway you set), DNS servers (or a provider such as Cloudflare or Quad9) and DNS over HTTPS; the mobile hotspot with its name, password, band, security, the connection it shares, power saving and connected devices; the proxy (detect, setup script, a server and exceptions); network reset), **Bluetooth & devices** (Bluetooth, adding and removing devices, printers; the mouse's primary button, pointer speed, pointer precision, lines to scroll, scrolling inactive windows, double-click speed; the keyboard's repeat delay and rate, cursor blink and access-key underlines), **Personalisation** (dark mode, transparency, accent colours and title bars, wallpaper, lock screen picture, file extensions and hidden files), **Apps** (uninstall desktop and Store apps, startup apps), **Gaming** (Game Mode, Game Bar, background recording; each app's GPU, hardware-accelerated GPU scheduling, windowed-game optimisations, variable refresh rate, Auto HDR), **Power & battery** (power mode and plan; screen off, sleep and hibernate times; energy saver; power button, sleep button and lid), **Time & language** (set time and time zone automatically, time zone, syncing the clock, 24-hour clock, short date, first day of the week, keyboard layouts), **Accessibility** (text size, pointer size, text cursor thickness, animations, scrollbars, how long notifications stay, high contrast, colour filters, Sticky / Filter / Toggle keys and the Shift-five-times shortcut; Magnifier, Narrator, on-screen keyboard), **Privacy & security** (Do Not Disturb, notifications, advertising ID, app-launch tracking, tailored experiences, online speech, and app permissions: camera, microphone, location, contacts, calendar, calls, account info, Documents, Pictures, Videos, the file system, screenshots, radios) and **System** (this PC and your account, storage and Storage Sense, clipboard history, Remote Desktop, Windows Update, environment variables, System Restore, advanced startup, reset this PC). What only an administrator may change (an adapter's IP, Remote Desktop, the time service, GPU scheduling, the lock screen) asks for permission, as Windows does. Win+I and every Settings link open these. Default apps can't be set by any program but Windows itself: right-click a file > Open with > Always. (Why not Windows' own Settings app? It's a Store app that only runs inside Explorer's shell, so in shell mode it can't open at all.)
- **Menus, Cascade's own**: every menu (the taskbar's, the desktop's, a file's shell menu, the settings pickers) is drawn by Cascade in place of Windows' grey lists: it fades in, its highlight glides, it scrolls, a long list (installed apps, say) gets a filter box to type into, and pickers drop down from their box.
- **Notifications:** a new one pops up at the bottom right in Cascade's theme (click it to open the app); Do Not Disturb keeps them quiet. Apps' own buttons and quick-reply boxes can't be shown (only Windows' notification platform can press them).
- **Virtual desktops of Cascade's own:** going to another one slides, as Windows' own do: the desktop you leave moves off to one side of each screen and the one you go to comes in from the other, under the taskbar and the notch (Windows 10 2004 or later). Task View (Win+Tab) with its strip of desktops (+ for a new one, × to close one, drag a window onto one to move it), the taskbar's desktops widget, Ctrl+Win+Left / Right to switch, Ctrl+Win+D for a new desktop and Ctrl+Win+F4 to close this one. Going to another desktop hides the windows of the one you leave and shows those of the one you go to. Hidden windows are kept in a list: when Cascade closes they all come back, and if it ever stops without closing, the guard shows them.
- **Snapping:** drag a window to a screen's edge for that half, a corner for a quarter, the top to maximise, with a preview (Settings > General > Snap by dragging). Win+arrows snap from the keyboard.
- **Controls** gains quick toggles: airplane mode, night light, mobile hotspot, Do Not Disturb.
- **Screen space, the tray, minimised windows:** maximised windows stop above Cascade's taskbar, minimised ones go to it (Windows shrinks a window into its own button there, as with Explorer's taskbar, and the button dips as it takes it in), apps' tray icons go to Cascade's tray.
- **Shortcuts Explorer answered:** Win+E (File Explorer), Win+R (Run), Win+D (show desktop, and back), Win+M and Win+Shift+M, Win+I (Cascade's Windows settings), Win+L, Win+S and Win+Q (search), Ctrl+Esc (Start), Win+arrows, Win+Shift+Left / Right (to the next screen), Win+1 to Win+9, Win+P (one screen, both, extend), Win+Space (next keyboard language; the taskbar's language widget shows it, in Settings > Widgets), Win+. and Win+; (emoji), Win+H (dictation), Win+Shift+S and Print Screen (snip), Win+Shift+R and Win+Alt+R (record), Win++ (Magnifier). Win+X, Win+V, Win+N, Win+A, Win+Tab, Alt+Tab and the Win key are Cascade's already.
- **Search:** the Start menu also finds files and folders in your user folder (through Windows' search index) and settings.
- **Emoji panel** (Win+.): emoji by category, recently used first, kaomoji and symbols; typed into the app you were in. No search by name.
- **Dictation** (Win+H): what you say is typed into the app in front (needs Online speech recognition, on the Privacy & security page). No voice commands.
- **Snipping and recording, as the Snipping Tool** (whose app needs Explorer): **Win+Shift+S** or **Print Screen** freezes the screens with a bar (it drops in from the top, with a hint under it of what to do) to pick a **rectangle** (where every snip starts), a **window**, a **full screen** or a **freeform** shape, and a **delay** (3, 5 or 10 s). The selection is outlined with handles and its size in pixels. A snip is copied to the clipboard (the notch says so), saved in Pictures\Screenshots, and opens in an **editor**: pen and highlighter (six colours, three sizes), eraser, crop, undo and redo, **copy the text** out of it (Windows' own text recognition, offline), copy, and save as PNG or JPEG. **Win+Shift+R** (or the bar's Record) **records** an area or a screen, with the PC's sound if you like, to an MP4 in Videos\Screen Recordings; a small bar (left out of the video) shows the time and stops it, as does Win+Shift+R again. **Win+Alt+R** records the screen you're on straight away, and stops it. Not as the Snipping Tool: no microphone track, and games in exclusive fullscreen record as black.
- **Startup apps, volume and media keys, fullscreen apps:** started a moment after you sign in (less those turned off), the keyboard's keys do what they did, and Cascade gets out of a fullscreen game's way.

**Apps from the Microsoft Store, and Windows' Settings.** Windows opens Store-style apps through Explorer, so as the shell Cascade tries the other ways in, in turn: Windows' activation itself (in case it answers), the app's own command (its execution alias, as a terminal would), and for desktop programs that come in a package (the Claude app, Windows Terminal and many others) the program itself, straight from its package. Apps that are Store apps through and through (Calculator, Photos, Windows' own **Settings**) can only be opened by Explorer: the notch says so. Settings links (Win+I, the Start menu, Controls' Wi-Fi and Bluetooth links) open the nearest classic Control Panel page instead.

**The safety net.** What Windows starts is a tiny second copy of Cascade (the guard) with no windows. It starts Cascade proper and starts it again if it ever goes away. If Cascade fails three times in a row, the guard turns shell mode off and starts Explorer, and Cascade says so the next time it runs. The first sign-in (or switch) with shell mode on is also a trial: a box asks whether to keep it, and if nobody says yes within two minutes (because nothing can be seen or clicked, say), shell mode turns itself off and Explorer comes back.

If you're ever left with a blank screen anyway: **Ctrl+Shift+Esc** opens Task Manager → *Run new task* → type `%LOCALAPPDATA%\Programs\Cascade\cascade.exe --shell-off` → OK. That turns shell mode off and starts Explorer. (By hand, it's the `Shell` value under `HKEY_CURRENT_USER\Software\Microsoft\Windows NT\CurrentVersion\Winlogon`: delete it and sign in again.) Shell mode is set for your account only, so another account on the PC always has Explorer.

**Getting Explorer back.** Right-click the taskbar or notch → **Quit to Explorer** hands over for the rest of the session (shell mode stays on for the next sign-in). Or turn the setting off, and say yes to going back now.

**What doesn't work without Explorer.** These are parts of Explorer itself, not things Cascade hides:

- **Store apps through and through**, and Windows' **Settings**, as above.
- **The Win key over admin apps** (Task Manager, installers) does nothing unless *Work over admin apps* is on (Settings → Start menu): beside Explorer, Windows' own Start menu at least opened there.
- **Win+K (casting) and Win+G (Xbox Game Bar).** Game Bar is a Store app; NVIDIA's and AMD's own overlays (Alt+Z, Alt+R) and Steam show FPS and keep instant replays without Explorer. (Recording is Cascade's own: Win+Alt+R.)
- **Snap layouts** (the grid on the maximise button) and Win+Z.
- **A file manager of Cascade's own**: File Explorer windows still open (and work), in Windows' look.
- Apps that start with Windows through the Store's own startup list.

It's experimental: `cascade.log` (Settings → About → Data folder) records what shell mode did, in lines beginning *shell mode:*, *shell guard:*, *desktops:*, *wallpaper:* and *snip:*.

## Lighting

RGB for every brand, right in Cascade's settings (**Settings > Personalisation > Lighting**, or **Lighting** in Start), so iCUE, Synapse, G Hub, Armoury Crate, RGB Fusion and the rest can go.

- **Profiles** down the side: make, rename, duplicate and delete them; link programs to one and it comes on while they're in front.
- **Lighting**: a profile is layers of effects, the top one over the rest: static colours, gradients, rainbows and waves, visor, rain, marquee, twinkle, strobe, heartbeat, fire, plasma, temperature (CPU or graphics card), audio visualiser and pulse, screen ambience, and keys that light as you type. Each layer covers all devices or the ones you pick, with its own colours, speed, size, direction, opacity and blend. A live preview shows every device lit.
- **Layout**: drag your devices to where they sit on your desk and resize them, so a wave runs across all of them as one. Fans can be rings, and any device can be flipped or left out.
- **Devices**: leave a device on the profile's layers, or give it one of its own hardware modes (with its colours, speed, brightness and direction, saved to the device so it keeps it with Cascade closed). Fan and strip headers take their LED counts here.
- **Settings**: on or off, brightness, frame rate, switching with programs, off while the PC is locked or its screens are off.

It drives the hardware through [OpenRGB](https://openrgb.org) (free and open source), which supports Corsair, ASUS Aura, Gigabyte RGB Fusion, Razer, Logitech, HyperX, MSI, NZXT, SteelSeries and hundreds of other devices. The first time Lighting opens it fetches OpenRGB into Cascade's data folder by itself (or use your own copy), starts it out of sight and stays connected; Cascade's lighting engine (`cascade.exe --rgb-engine`, at normal priority) feeds it. Keyboards, mice, headsets and USB controllers work as they are; **RAM, motherboard and graphics card lighting** need *Administrator rights* (Lighting > Settings), which installs the [PawnIO](https://pawnio.eu) driver and sets OpenRGB to start with the rights, under one Windows prompt. Close the vendors' own apps (Lighting lists the ones running) and stop them starting with Windows: they hold their devices and fight for them.

Everything on the page lights under the pointer and moves smoothly; the settings window widens to fit it when it's narrow.

## Apps

Cascade brings its own versions of Windows' everyday apps, each in a process of its own and its own app on the taskbar (they're in Start; in shell mode, where Store apps can't run, text, music and video files open in them too):

- **Files** (`--files`): tabs in the title bar, Explorer's command bar, coloured sidebar, folder sizes with TreeSize-style bars, drive bars.
- **Photos** (`--view`), **Disk Usage** (`--disk`: a TreeSize-style tree that fills in live, or a map), **Snip** (`--snip`).
- **Calculator** (`--calc`): Standard and Scientific, the answer as you type, history.
- **Camera** (`--camera`): photos and video (with sound) to Pictures\Camera Roll, mirror, self-timer, more than one camera.
- **Notepad** (`--notepad`), **Clock** (`--clock`: timers, stopwatch, alarms, world clock), **Media Player** (`--player`), **Sound Recorder** (`--soundrec`) and **Paint** (`--paint`).

## Pen tablet

Settings > Bluetooth & devices > Pen tablet: a driver built in for 10moon, Vinsa and their relatives (USB 08F2:6811, 0B57:902E), so their own driver can go. Cascade switches the tablet into tablet mode and reads the pen directly (as OpenTabletDriver does for the 10moon 1060N), and hands it to Windows as a real pen through Windows' synthetic pointer input (pressure in Krita, Photoshop, OneNote...) or as a mouse. It maps to every screen or the main one, keeps the screen's proportions if asked, turns round for the left hand, and learns where each pen rests (so it's never stuck pressed). A pressure graph sets where the pen starts drawing, how soft or firm it feels and how hard a full press is (your pen's pressure shows on it live), and a pad tries it out. The pen's two buttons can be right-click, middle-click, eraser or nothing. Close the tablet's own driver (`TabletService.exe`) first: Settings says if it's running.

## Settings

Search **Cascade Settings** in Cascade's Start menu or the Windows one, or right-click the notch, taskbar or tray icon and choose **Settings…**. The box at the top of the sidebar searches every page by a setting's name, description or page; a result works just like the setting on its own page. Every option has a toggle, slider, choice or text box, and changes apply immediately. A ↺ button appears next to anything you've changed; it puts that setting back to its default. Settings that wouldn't do anything as things are set up stay out of the way: the lip's length shows only in Lip style, the notch's width only in Notch style, the custom colour only with Custom picked, the Start menu's parts only with Cascade's Start menu on, and so on. They come back as soon as they'd make a difference. When one needs something on another page (Win+N needs Cascade's taskbar), its description says so, in amber. Starting Cascade again while it's already running also opens Settings.

## Running it

### Install

Download **CascadeSetup.exe** from the [latest release](https://github.com/CornyBirdy/Cascade/releases/latest), run it and click **Yes**. It installs Cascade to `%LOCALAPPDATA%\Programs\Cascade` and starts it. Running Setup again updates Cascade.

Cascade isn't code-signed and it listens to the keyboard for the Win key, so Windows Security can quarantine it. Setup asks Windows once for permission to exclude Cascade's folders from Windows Security's scans (the same thing as Settings → About → **Add exclusion**), and does it before writing any files. Your browser or SmartScreen may warn about the download itself, because it's unsigned: choose **Keep**, then **Run anyway**.

### Using it

Cascade is a single `cascade.exe`: put it anywhere and double-click it. Right-click the notch, an empty part of the taskbar or the tray icon for:

- Settings… / Open Start menu
- Show notch / Show taskbar / Auto-hide taskbar / Replace the Windows taskbar
- Start with Windows (a sign-in task, so it starts as you sign in; if it's up before Explorer it waits for Explorer's taskbar before hiding it). If it was turned on before the sign-in task existed (or its permission prompt was declined), Windows' own startup list runs Cascade late, often well after the desktop is up: the notch says so once, and **Start with Windows sooner…** in this menu moves it to the sign-in task (Windows asks for permission once; Settings → General says the same). `cascade.log` notes how long after Windows started Cascade came up, and how it was started.
- Hide Windows volume flyout
- Open / reload config (`%APPDATA%\Cascade\config.ini`; focus-timer lengths, weather location, sizes and more)
- Restart Cascade (a fresh copy starts as this one exits, admin key helper included; handy if anything gets stuck)
- Full restart (force): puts the Windows taskbar back, stops the admin key helper, ends every Cascade process that is left, hung or not, and starts a fresh one a couple of seconds later
- Quit

## Updating

Right-click the notch, taskbar or tray icon and choose **Update Cascade**. One click checks GitHub for a newer build, downloads it, swaps the exe and restarts. Cascade also checks every few hours and shows a note in the notch when an update is out.

Every push to the default branch is published as a GitHub release (`build-N`), and that's where the updater looks. While the repo is **private**, GitHub only serves releases to signed-in users. In that case, create a fine-grained token with read-only *Contents* access to this repo and paste it after `github_token =` in the config file. If the repo is public, no token is needed.

**Updating from a branch.** Every other branch keeps one pre-release (`branch-<name>`), replaced on each push. Settings > Updates > **Update from** lists *Stable* and each branch with its newest build number. Picking one only keeps the choice; nothing is installed until you click *Update now* (or the update notice), which then installs that branch's build (even an older one, when you're switching). From then on, Update Cascade and the automatic check follow that branch, until you pick Stable again.

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
| Themes | — | A DWM blur style and a few gradients per panel, painted once and drawn from a bitmap while the panel stays the same; nothing extra runs |
| Visualizer | Runs whenever media plays | Runs only while the bars are on screen, and redraws only the music widget, not the whole taskbar |
| Stats | — | Read on their own thread, only while a stats widget is on screen |
| Weather | — | One HTTPS request per 30 min on a sleeping thread |
| Idle loop | Several polling threads | Blocks in `GetMessage`: 0% CPU |

Frames are only drawn while something is animating; the taskbar clock costs one redraw a minute. To see what each surface costs on your machine, set `perf_log = true` in the config: while something animates, `cascade.log` gets each surface's average render time and frame rate about once a second. Rendering uses Direct2D's software rasteriser, because the surfaces are small and this keeps the GPU asleep. The acrylic blur is done by Windows' own compositor, the same way the real taskbar gets it, so it costs Cascade nothing.

Cascade also never injects code into Explorer or patches Windows files, unlike Explorer mods. It draws its own windows next to Explorer, so Windows updates have nothing of it to break.

## Layout

| File | What it does |
|---|---|
| `src/ui.rs` | App core: windows, message loop, notch states, layout, input and drawing |
| `src/launcher.rs` | Start menu: places, pinned, recent files, all apps, search, power |
| `src/smart.rs` | Start search's sums, unit conversions, commands and web search |
| `src/keyhook.rs` | Lone Win key → Start menu |
| `src/settings.rs` | Settings window |
| `src/dock.rs` | Taskbar: window tracking, layout, drawing, drag, launch/focus |
| `src/backdrop.rs` | Acrylic blur behind the taskbar and Start menu |
| `src/systray.rs` | Tray icons from apps, and the hidden-icons flyout |
| `src/mouseedge.rs` | Pointer matching across screens of different sizes |
| `src/usage.rs` | Cascade's own memory and CPU use, for Settings → About |
| `src/helper.rs` | Optional admin helper for the Win key over admin apps |
| `src/sysmon.rs` | CPU, memory, network, GPU and disk readings for the stats widgets |
| `src/nvml.rs` | Each NVIDIA card's load, temperature, memory and power (the driver's NVML) |
| `src/widgets.rs` | The widgets panel |
| `src/quickmenu.rs` | The Win+X menu |
| `src/deskosd.rs` | The pop-up showing your desktops as you switch |
| `src/deskslide.rs` | Shell mode: the slide from one of Cascade's desktops to another |
| `src/wifi.rs` | Controls' Wi-Fi list: networks in range, joining one |
| `src/desktops.rs` | Windows' virtual desktops: reading them, watching them, switching, moving a window to one |
| `src/clipboard.rs` | The Win+V menu: Windows' clipboard history (or Cascade's own when Windows won't share it), and pasting from it |
| `src/preview.rs` | Window previews (DWM thumbnails) |
| `src/gfx.rs` | Direct2D/DirectWrite drawing into layered windows |
| `src/spring.rs` | Spring physics behind every animation |
| `src/media.rs` | Now playing + album art (event-driven, own thread) |
| `src/devices.rs` | USB devices plugged in or out, and the device list |
| `src/audio.rs` | Volume callbacks, loopback visualizer + FFT |
| `src/mixer.rs` | The Controls tab's per-app volumes and output device |
| `src/brightness.rs` | Brightness events (WMI) and setting (WMI + DDC/CI) |
| `src/radios.rs` | Wi-Fi / Bluetooth toggles |
| `src/weather.rs` | Open-Meteo over WinHTTP |
| `src/pomodoro.rs` | Focus timer |
| `src/calendar.rs`, `src/ics.rs` | Your calendar's events, from its iCal link |
| `src/icons.rs` | App icons and names |
| `src/update.rs` | One-click updates from GitHub releases, from Stable or a branch |
| `src/http.rs` | Tiny WinHTTP client |
| `src/shell.rs` | Tray, autostart, fullscreen, taskbar, flyout, menus |
| `src/config.rs` | `config.ini` |
