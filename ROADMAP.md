# Roadmap

Features agreed on but not built yet. Tick them off (and move the notes to `CHANGELOG.md`) as they land.

## Quick fixes

- [ ] **Flashing app buttons:** when an app asks for attention, its taskbar button flashes, as on Windows' own taskbar. The shell hook is already there for windows opening and closing (`HSHELL_FLASH` is the missing message).
- [ ] **Win+1 … Win+9:** open or switch to the 1st … 9th app on Cascade's taskbar. Also Win+T (step through the taskbar's apps) and Win+B (the tray). (Win+1 … 9 already work in shell mode, where the keys are Cascade's own; beside Explorer they still go to the hidden Windows taskbar.)
- [ ] **Installed apps entry:** Setup writes an uninstall entry, so Cascade shows in Settings → Apps → Installed apps and can be removed from there.

## What the real taskbar has

- [ ] **Pin syncing:** follow Windows' own pinned-taskbar folder (`%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar`), so *Pin to taskbar* anywhere in Windows pins to Cascade. Bring the existing pins in on first run.
- [ ] **Jump lists:** right-click an app for its recent and frequent files and its own tasks (*New incognito window* and the like).
- [ ] **More on the right-click menu:** Run as administrator, Open file location, Properties.
- [ ] **File search in Start:** find documents through Windows' search index, not only apps, sums, commands and the web.
- [ ] **Start from the Windows setup on first run:** alignment, auto-hide, seconds on the clock and pins as they were on Windows' taskbar.

## Following Windows' settings

- [ ] **Animation and transparency switches:** when animations or transparency effects are turned off in Windows, Cascade's go off too. Text size follows Windows' text-size setting.
- [ ] **Do Not Disturb:** Cascade's own pop-ups (USB devices, charger, updates) stay quiet while it's on, and Controls gets a button for it.
- [ ] **Narrator and keyboard access:** Cascade's taskbar, Start menu and panels can be read by screen readers and reached from the keyboard. The largest job here.

## Not possible as things stand

- **Progress bars, overlay icons and badge counts on app buttons.** Apps hand these to Explorer directly; another program can't read them without running code inside Explorer.

## Shell mode (Cascade in Explorer's place)

Tried once on a real PC (taskbar, Start, shortcuts and tray worked). Fixed since: wallpaper, Win+I freezing, packaged apps not opening, virtual desktops, switching without signing out, snips. Still to do:

- [ ] **Check the fixes on a real PC**: the wallpaper (and Wallpaper Engine on it), the Claude app opening, switching now both ways, desktops (windows hide and come back, nothing lost after a restart of Cascade).
- [ ] **Desktop icons** on Cascade's desktop window.
- [ ] **Win+P, Win+K, Win+. and Win+Space** stand-ins.
- [ ] **Store apps' startup tasks** (the ones on Task Manager's Startup page that aren't in the registry or the Startup folder).
- [ ] **Per-screen and slideshow wallpapers** (Cascade shows Windows' one picture on every screen).

## Shell mode: as if Explorer never existed

The goal before merging to `main`: nothing in daily use falls back to Windows' own UI, or needs Explorer.

**First (gaps hit every day)**
- [ ] **Cascade Settings for Windows**: Cascade's own pages for what Windows' Settings did. Display (resolution, scale, refresh rate, screen layout, night light), Sound (devices, levels), Network (Wi-Fi with password entry, Ethernet, VPN, mobile hotspot), Bluetooth (pairing new devices), Personalisation (wallpaper, accent colour, light/dark mode), Apps (installed list, uninstall), Power and battery, Date and time, About.
- [ ] **Real notification pop-ups**: banners with the app's buttons and quick reply, plus Do Not Disturb.
- [ ] **Window snapping**: drag to an edge or corner to snap, and a layout picker (Explorer did both).
- [ ] **Win+P**: duplicate / extend / one screen only.
- [ ] **Start searches files and settings** (Windows' search index works without Explorer).
- [ ] **Keyboard language switcher** (Win+Space, Alt+Shift) with an indicator on the taskbar.

**Next**
- [ ] **More Controls toggles**: airplane mode, night light, mobile hotspot, battery saver, Do Not Disturb.
- [ ] **Desktop icons**, and right-click New folder / drop files on the desktop.
- [ ] **Emoji panel** (Win+.).
- [ ] **Shut-down box** on Alt+F4 at the desktop.
- [ ] **Stand-ins for Store-only tools**: Game Bar (Win+G), screen recording, dictation (Win+H), casting (Win+K).

**Big**
- [ ] **Cascade's own file manager** (File Explorer windows still open, but they're Windows' look).

**Not possible**: the lock screen, sign-in, UAC prompts and the Ctrl+Alt+Del screen run on Windows' secure desktop, which no app can draw on. Pure Store apps (Calculator, Photos, Windows' Settings) only open through Explorer.
