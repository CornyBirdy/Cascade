# Changelog

What changed in Cascade, newest first. Every push to the main branch is built and
published as a release (`build-N`), and its notes are the top section of this file.
Settings > Updates > What's new opens the list.

## 2026-11-18 (later)

**New**
- **Appearance, made over**: two themes, **Acrylic** and a rebuilt **Liquid Glass** that's drawn rather than simulated (gradients painted once per pane size, nothing read from the screen, so it costs what Acrylic does). The other themes, the colour/hue/intensity controls and Refraction are gone; a config naming an old theme gets Acrylic.
- **Accent colour, as Windows does it**: your Windows accent by default (followed when it changes), or one of Windows' own palette, picked from a grid of colour dots; *Accent colour on the glass* tints the glass with it. The glass itself is neutral grey: no more purple tone.
- **Notch over fullscreen apps** (Settings > General): the notch stays on top of games and fullscreen apps while the taskbar still hides.
- **Auto ducker, picked not typed**: choose the app that ducks and the apps that go down from a list of the apps with sound (or *Every other app*), and set how loud counts as sound (-60 to -10 dB).
- **Scrolling** through the Notifications and Clipboard cards in the widgets panel.
- **Animations**: shell mode's desktops slide from one to the next, to the side you're going; minimised windows shrink into their taskbar button (shell mode) and the button dips as it takes them in; window previews rise in from the taskbar and glide from app to app; the snip bar drops in.
- **Task View's desktops** show your wallpaper with little windows on it (title bars, shadows, icons), in bigger boxes.

**Snipping**
- Every snip starts on **Rectangle**.
- A snip is **really copied** to the clipboard now (it was put there after its window had closed, which Windows refuses), and the notch says so.
- No more **flicker** while dragging out a rectangle: each frame is drawn off screen and put up in one go.
- A proper bar: icons and labels in groups, the picked ones in the accent colour, a hint underneath, and a selection with handles and its size.

**Fixed**
- **Cascade hiding while Claude uses the computer** (or under any see-through, click-through overlay): an overlay over the whole screen is no longer taken for a fullscreen app.
- **The taskbar not coming up with Start over a fullscreen app**: it's brought over the app now.
- **"Windows wouldn't share" the clipboard history**: Cascade brings its menu forward and asks again, and if Windows still says no, shows its own history of what was copied while it ran.
- **Picking a branch to update from** no longer offers to install it straight away: the choice is kept, and *Update now* installs it when you want.

## 2026-11-18

**New**
- **Settings, rebuilt** after Windows 11's: bigger and resizable, a sidebar with an icon for each page in two groups (Cascade, Windows) that scrolls, roomier cards with group headings, Windows-style buttons, drop-downs and On/Off toggles. And it moves: pages slide in, the sidebar's marker glides to the page, scrolling eases, toggles slide, hovers fade.
- **Menus, Cascade's own** everywhere (taskbar, desktop, a file's menu, every picker): animated, scrolling, with a filter box for long lists (the installed apps) and keyboard control; pickers drop down from their box.
- **Desktop**: drag to select icons, Ctrl/Shift+click, Delete, F2 rename, Ctrl+A/C/X/V, arrows, F5, the menu key; Paste on the desktop's menu; one menu for several selected icons.
- **Power plan** picker (Power & battery).
- **Update from a branch** (Settings > Updates > Update from).

**Fixed**
- Desktop icons coming up over apps (and staying there): they now stay just above the wallpaper whatever happens.
- The busy cursor over Wallpaper Engine's wallpaper.
- Power mode not changing on a High performance or Ultimate plan: picking a mode moves to Balanced first, the only plan Windows applies modes on.

## 2026-11-17

**New**
- **Shell mode (experimental)**: Settings > General > *Run as the Windows shell*. From your next sign-in Windows starts Cascade in Explorer's place, so the Windows taskbar and Start menu are never made at all, where until now Cascade hid the one and kept the Win key from the other. Cascade then does the shell's jobs itself: the desktop and wallpaper, the space under the taskbar, the tray, the programs that start with Windows, the keyboard's volume and media keys, new notifications in the notch, and the shortcuts Explorer answered (Win+E, Win+R, Win+D, Win+M, Win+I, Win+L, Win+S, Win+arrows to snap, Win+Shift+arrows to change screen, Win+1 to 9 for the taskbar's apps, Win+Shift+S, Ctrl+Esc). A small guard process starts Cascade again if it ever goes away, and turns shell mode off and brings Explorer back if Cascade fails three times in a row. The first sign-in is a trial: a box asks whether to keep it, and no answer within two minutes brings Explorer back. `cascade.exe --shell-off` (from Task Manager's *Run new task*) is the way out if anything goes wrong. *Quit to Explorer* in the right-click menu hands over for the rest of the session. Off by default. Without Explorer, Store-style apps (Windows' own Settings among them) may not open, and there are no virtual desktops, quick settings or desktop icons: see the README's *Shell mode* section before turning it on.

**Updates**
- **Update from a branch**: Settings > Updates > *Update from* picks Stable or any branch, and Cascade updates itself to that branch's newest build (each branch's build is kept as a pre-release), so testing a branch no longer means downloading an exe.

**Shell mode: as if Explorer never existed**
- **Windows' settings on Cascade's pages**: Display, Sound, Network & Bluetooth, Personalisation, Apps, Power & battery, Time & language, Privacy & access and System, in Cascade's Settings. Win+I and Settings links open them in shell mode. Includes a night light of Cascade's own.
- **Notification pop-ups** in Cascade's theme, with Do Not Disturb.
- **Desktop icons**, with each item's own shell menu, New folder, and dropping files on the desktop.
- **Snapping by dragging** to an edge or corner, with a preview.
- **Start search finds files and settings**.
- **Win+P**, **Win+Space** (and a keyboard-language taskbar widget), **Win+.** (an emoji panel), **Win+H** (dictation), and **Alt+F4 on the desktop**.
- **Controls quick toggles**: airplane mode, night light, mobile hotspot, Do Not Disturb.

**Shell mode, after its first try**
- **Wallpaper**: Cascade draws Windows' wallpaper itself (it stayed black), fitted to each screen as Windows is set to. Right-click the desktop to change the picture or how it fits. The desktop is laid out as Explorer's, so **Wallpaper Engine** and **Lively** can put their wallpaper on it.
- **Win+I froze Cascade** and opened nothing: Windows' Settings can't open without Explorer, and asking for it hung Cascade. Settings links now open the nearest classic Control Panel page straight away, and everything shell mode opens is opened without holding Cascade up.
- **Apps from the Microsoft Store**, like the Claude app, did nothing when clicked (or, run as administrator, gave an error). Cascade now tries Windows' activation, then the app's own command, then the program straight from its package; apps that only Explorer can open say so in the notch.
- **Virtual desktops of Cascade's own**: Task View's desktops, the desktops widget, Ctrl+Win+Left / Right / D / F4. (Making a desktop opened Control Panel before: that was Win+I's Settings fallback, landing late.)
- **Switching without signing out**: turning the setting on offers to switch now, closing Explorer and taking over on the spot; turning it off offers to bring Explorer back now.
- **Minimised windows** left a stub (a small title bar) along the bottom of the screen: Cascade now tells Windows it's the taskbar, and moves any stub Windows leaves anyway off the screen.
- **A busy pointer over Wallpaper Engine's wallpaper**: the desktop now sets the arrow, as Explorer's does.
- **The Snipping Tool, Cascade's own** (Windows' is a Store app): rectangle, window, full-screen and freeform snips with a delay (Win+Shift+S, Print Screen); an editor with pen, highlighter, eraser, crop, undo, copying text out of the picture, copy and save as; and **screen recording** to MP4 with the PC's sound (Win+Shift+R, Win+Alt+R).

## 2026-11-09

**New**
- **Closing a desktop from Task View**: point at a desktop in the strip and click the × in its corner. Its windows move to the desktop next to it, as Windows does, and you stay on the desktop you were on. The only desktop left has no ×.

**Fixed**
- **Super drag opening Start when Win is let go**: a drag takes a while, long enough for the key tap that keeps Start shut to be forgotten, so the release of Win now goes out with a fresh tap in front of it, the way a long lone Win hold already did. It also works with the Win-key launcher and every Win+key shortcut turned off. When a Win+click isn't taken as a drag, `cascade.log` now says why (the window's kind, admin rights, maximised...).
- **A new desktop made from Task View's +**: Task View stays open, and Cascade waits for the desktop to exist and then jumps straight back to the one you were on, instead of stepping back one desktop at a time. Windows still flashes the new desktop for a moment (it has no way to make one without going there).

## 2026-11-04

**New**
- **Phone hotspot** button in the Wi-Fi list: opens Windows' network list, where your phone's Instant Hotspot has its Connect button.
- **Seconds on the notch's clock** (Settings > Notch).

**Fixed**
- **A new desktop made from Task View's +** no longer takes you into it: you stay on the desktop you were on.
- **Switching desktops** from the desktops widget or Task View goes one step at a time, so a jump of several desktops lands where you clicked instead of stopping short.
- **Super drag**: a short Win+drag no longer counts as a lone tap of Win, so letting go of it can't open the Start menu.
- **Restoring a minimised app** from the taskbar tries again with Windows' other restore routes when the first one is ignored, instead of needing another click.

## 2026-10-30

**New**
- **Auto ducker** (Settings > General): while one app is making sound, such as Discord with your friends talking, the apps you pick (or every other app) fade down, and come back a moment after it goes quiet. Volumes you change by hand while they're down are taken as the new normal, and everything is put back when it's turned off or Cascade closes.
- **Settings search**: a box at the top of the sidebar finds settings from every page by name, description or page; a result works just like the setting on its own page, and picking a page leaves the search.
- **Battery percentage on the taskbar** (Settings > Taskbar): the charge level as a number beside the battery icon.
- **Battery time left**: click the battery's percentage in the notch (Home, or beside the media controls) to swap it for how long it will last.
- The **battery now shows in the notch's Media tab** while music is playing, not only on Home.
- **Start menu search box**: Ctrl+A selects everything you typed, then typing replaces it, Backspace or Delete clears it, and Ctrl+C / Ctrl+X copy or cut it.
- The **Widgets panel's calendar** animates: the month's title, weekdays and days come in one after another (from the side the month came from), today's circle pops, and the picked day's events slide in.

**Fixed**
- If Win+D (Show desktop) ever hides the taskbar or the notch, Cascade now notices and shows them again.

## 2026-10-28

**New**
- **Super drag**: hold Win and drag a window by any part of it, not only its title bar.
- Widgets panel: drag the cards to reorder them; a Clipboard card; it opens with the layout it had last time.

## 2026-10-27

**New**
- Task View shows a picture of each desktop and lists the apps open on one while you hover it; windows can be dragged to other desktops.
- Right-click menu: Restart Cascade, and a Full restart (force).
- Battery saver: on battery, refraction and visualizers are off and stats are read less often.
- The Calendar tab's days, events and timer animate in.
- Lighter Liquid Glass and a steadier Widgets panel.

**Fixed**
- The notch comes back if a fullscreen app's exit was never announced.
- Win key: the release is replaced after a long hold, and each Win press is logged to help track down the Start-key bug.

## 2026-10-26

**New**
- CascadeSetup.exe, a one-click installer, is attached to every release.
- Run as administrator from the menus.

**Fixed**
- A stuck Win key frees itself.

## 2026-10-25

**New**
- Search ranks what you open most often first; Cascade starts faster; a Defender exclusion can be added from Settings.

**Fixed**
- Win key without swallowing releases; the notch recovers after the PC wakes.

## 2026-10-21 and 22

**New**
- Taskbar: drag widgets and apps, a hover that lets go, a charging bolt.

**Fixed**
- Shortcuts work again; dragged apps and widgets follow the pointer; the Win key runs on its own thread.

## 2026-10-20

**New**
- A new icon (the glass C), and the Rack Suite design system.
- Animations are paced to the screen's refresh rate.

## 2026-10-19

**New**
- A Bluetooth list in Controls to connect and disconnect your devices.
- Start with Windows at sign-in, Controls without the notch, Win+drag and a calendar card.
- The visualizer listens to the app that's playing, not everything.

## 2026-10-18

**New**
- Alt+Tab in four looks, with arrow keys in every direction.
- Phone Link: your phone's music and notifications in Cascade.
- Controls: pick a Wi-Fi network; Desktops pop-up and numbered widget; GPU widget options.
- Liquid Glass with a real blur; visualizer bars on the beat.

## 2026-10-16

**New**
- Virtual desktops (Win+Tab Task View), Win+V clipboard history in Cascade's look, and your calendar's events in the notch.
- Start search does sums, unit conversions, commands and web searches.
- Controls has a volume mixer and an output picker; drop files on a taskbar app to open them with it.
- A performance pass: panels paint once and draw from a bitmap; stats are read on their own thread.

## 2026-10-12 to 15

**New**
- The Start menu: a collapsible sidebar, go-to-letter grid, smooth scrolling, a power pop-up.
- Themes that light passes through, with colour, intensity and edge; an Appearance gallery; the notch's lip and tab bar; widgets boards; refraction for clear glass; the music visualizer.
- A themed Win+X menu, Win+N and Win+A shortcuts, taskbars on every screen.
