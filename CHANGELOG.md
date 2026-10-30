# Changelog

What changed in Cascade, newest first. Every push to the main branch is built and
published as a release (`build-N`), and its notes are the top section of this file.
Settings > Updates > What's new opens the list.

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
