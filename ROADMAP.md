# Roadmap

Features agreed on but not built yet. Tick them off (and move the notes to `CHANGELOG.md`) as they land.

## Quick fixes

- [ ] **Flashing app buttons:** when an app asks for attention, its taskbar button flashes, as on Windows' own taskbar. The shell hook is already there for windows opening and closing (`HSHELL_FLASH` is the missing message).
- [ ] **Win+1 … Win+9:** open or switch to the 1st … 9th app on Cascade's taskbar. Also Win+T (step through the taskbar's apps) and Win+B (the tray).
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
