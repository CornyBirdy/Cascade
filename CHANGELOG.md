# Changelog

What changed in Cascade, newest first. Every push to the main branch is built and
published as a release (`build-N`), and its notes are the top section of this file.
Settings > Updates > What's new opens the list.

## 2026-11-26

**New**
- **Camera settings**: a gear button opens a menu with the picture's size (Quality: every size the camera offers, up to 4K, or Automatic), the self-timer's length (off, 3, 5 or 10 seconds; the timer button steps through them too), a rule-of-thirds grid, mirroring, and photos as PNG or smaller JPEG files.
- **Uninstall an app, reworked** (Settings > Apps): the list shows who made each app and how big it is, Store apps are marked, and it's one column that scrolls with the filter box (hundreds of apps no longer spill into three columns). Components Windows won't let you remove aren't listed, installers that say "install" are turned into uninstalls, and Cascade tells you when the app is gone.
- **Startup apps** stay open after a switch, so several can be turned on or off in one go.

- **Files, a View menu like Explorer's**: Extra large, Large, Medium and Small icons or Details, Group by, **Compact view** (tighter rows), and Show: **Navigation pane** (hide the sidebar), **Details pane**, **File name extensions** and Hidden items.
- **Files, the Details pane** (View > Show, or Alt+Shift+P): a big picture of what's selected (a thumbnail for pictures and videos) with its type, size, date and where it is; with several selected, how many and how big; with none, the folder.
- **Files, the arrows in the address bar**: click the arrow after any part of the path to see the folders inside it and jump to one (the drives, for This PC).
- **Files, history**: right-click Back or Forward for the places they'd go, and pick one to jump straight there.
- **Files, undo and redo**: Ctrl+Z takes back a rename or a new folder or file, Ctrl+Y does it again. *Invert selection* is in More.

**Fixed**
- **Win+Shift+S and Print Screen over any app** (shell mode): the snip's shortcuts are now taken by Cascade's keyboard watcher as well as registered with Windows, so they open the snip whichever app is in front (Discord, Opera, Steam, Claude...) and whatever else has claimed them. If it still doesn't open, `cascade.log` says why ("snip: ..." lines).

## 2026-11-23

**New**
- **Files remembers**: each folder keeps how you left it (details or icons, what it's sorted by and which way, Group by), as Explorer does, so Downloads grouped by date modified is still grouped the next time you open it. Hidden items and the columns' widths are kept too.
- **Widgets, edit mode**: cards glide to their new places as you move them, and the + and - buttons are gone: drag the dot on a card's top left corner to make it bigger or smaller.
- **Taskbar**: an app with two or more windows open shows a long dash and a short one after it under its icon, as Windows does; one window is still a single dash.
- **Menu animation** (Settings > Personalisation): Cascade's menus, in Files and everywhere, can open at once instead of fading in.
- **Screenshots and recordings in the Notifications card**: the preview that pops up when a snip or recording is saved now stays in the widgets panel's Notifications card too (click it to open the file).

- **Files, hotkeys**: Ctrl+wheel (and Ctrl plus and minus) change the size of icons, going from Details to Icons and back; Ctrl+Shift+1 to 4 pick extra large to small icons and Ctrl+Shift+6 Details, as in Explorer; Ctrl+1 to 9 jump to a tab; Ctrl+R and F5 refresh, Ctrl+E search, Alt+D the address, Alt+Enter properties, Ctrl+D delete, Esc clears the selection and Ctrl+Shift+. shows hidden items.
- **Files, a sidebar you can edit**: right-click a pinned folder to unpin it or move it up or down, or to restore the usual six; *Pin to sidebar* is in the folder's background menu and in More (it pins the selected folder, or the one you're in).
- **Files, refreshing shows**: the refresh button turns, a bar runs under the command bar and the status says *Refreshing...* until the folder has loaded.
- **Paint**: Ctrl+V pastes a picture from the clipboard as a piece you drag into place (Enter puts it down, Esc drops it), Ctrl+C copies the picture, and a new menu at the end of the bar has *Resize canvas* (cut off or add white, or stretch the picture), rotate and flip, and clear. The Fill tool is a bucket, with a drop of the colour you're using. [ and ] change the pen size; Ctrl plus, minus and 0 zoom.

- **Menus, more room and more colour**: rows are tighter, and a list too long for the screen (a file's menu with all the shell's extras) runs on into a second and third column instead of scrolling a tall strip. The usual commands (Open, Copy, Paste, Delete, Rename, Refresh, Properties, New, Pin, Sort, View...) have an icon, each in a colour of its own: red for what removes, green for what makes, blue for the clipboard, amber for edits and info.
- **The theme previews** (Settings > Personalisation) show Cascade's own taskbar in each material over a desktop with a window behind it, so frosted and clear glass can be told apart, instead of coloured blobs.

- **One accent colour** (Settings > Personalisation): the second palette is gone. The colour picked is Cascade's accent and Windows' own too (highlights, buttons and links in its windows); the first dot still follows Windows' colour and leaves it as it is.

- **Liquid Glass, reworked** to look like Apple's: a light blur you can still see the page through (it was clear and sharp, and read as a dark flat panel), a cool light wash so white text reads, a lens round the whole edge that bends and smears what's outside it (softened first, so print and edges don't turn up as stray bits inside the glass), and a thicker, brighter rim with a glow inside it. The preview shows it.

**Fixed**
- **A snip over an app that keeps the front**: the snip bar and the preview card now take the top of the always-on-top windows even when the app in front won't give up focus (an administrator's, a game, a video), and the snip asks Windows and the admin helper for the front too. `cascade.log` now says what a snip opened over and whether it got the front, to track down any app it still can't.
- **A flicker when restoring a window from the taskbar**: the window showed in its place for a moment before the animation grew it out of its button; it now comes back out of sight and is put in place as the animation lands. (Windows that were maximised still flash.)

## 2026-11-22

**New**
- **Quick Share, built in** (Settings > Bluetooth & devices > Quick Share): send files to Android phones and PCs nearby and take what they send, over your Wi-Fi, with no app of Google's. This PC shows up in a phone's Quick Share list; when something comes, a card says who it's from and what, with the PIN to match on their screen, Accept or Decline, then how far it's got and Open or Show in folder (links open, texts are copied and saved). To send: **Share with Quick Share** in right-click menus (and Send to), the new **Share** button in Files, Quick Share in Start, or Settings; a window lists what's nearby (Cascade wakes phones with a Bluetooth advert), shows the PIN and sends. Name, where files go, and asking or accepting at once can be set. The first time, Windows asks to let Cascade through its firewall: allow it on private networks.
- **The tablet's own buttons** (Settings > Bluetooth & devices > Tablet buttons): each of the 12 set to a shortcut (ctrl+z, space, shift held...), a right or middle click, or the eraser while it's held; Settings shows which one you're pressing. The pen's buttons can press a shortcut too.
- **Win+arrows, as Windows 11 has them** (shell mode): left and right halves (on to the next screen past the edge), up from a half to its top quarter and then maximised, down to the bottom quarter and then back where it was; Win+Shift+Left/Right moves a window to the next screen, Win+Shift+Up makes it as tall as the screen. Taken by Cascade's own keyboard hook, so they work even when Windows won't hand the shortcuts over.
- **Press and hold to right-click** can be turned off for the pen (Windows' gesture: holding the pen still right-clicks).
- **Pressure graph and test pad for the pen tablet** (Settings > Bluetooth & devices > Pen tablet): drag the curve's points for where the pen starts drawing, how soft or firm it feels and how hard a full press is, with a dot showing your pen live; draw on the pad to try it. The tablet's settings only show while the built-in driver is on.
- **Disk Usage as a tree**, as TreeSize shows it (the map's still there, Tree / Map at the top): open folders to see what's in them, biggest first, each with its size, files, folders, a bar for its share of the folder above, and when it last changed; loose files together as "[n Files]". Without the file index the tree fills in as it's worked out: folders appear as they're found and their sizes climb.
- **Snap layouts**: drag a window to the top of the screen and Windows 11's layouts drop down (halves, thirds, two thirds and one, a wide middle, a half and two quarters, quarters); let go over a part to put it there.
- **Screenshots pop up as a preview** in the corner, as recordings do (Edit opens the editor, Folder, Copy, Delete), instead of the whole editor every time.
- **Own icons for Cascade's apps**: Files, Photos, Calculator, Camera, Notepad, Clock, Media Player, Sound Recorder, Paint, Disk Usage, Lighting, Snip and Settings each have their own, in Start and on the taskbar (they were Cascade's logo or Windows' stock pictures). Snip is its own app on the taskbar and in Start too.
- **Check for updates** (Settings > Updates): looks first and says what it found; Update now appears once there's a newer build.
- **Any accent colour**: the + after the palette opens a colour picker.
- **Profile picture** (Settings > Start menu): a picture for Start's account button; your Windows account's picture until you pick one.
- **Files**: the path as parts to click (This PC › C: › Users ›...; click the bar to type one), columns that drag wider or narrower, **Group by** type, date modified or size (View), and phones and cameras plugged in listed in the sidebar, as Explorer does.
- **Wobbly windows settings**: how much they wobble and how quickly they settle.

**Improved**
- **Settings in Windows 11's look**: the wallpaper blurred and darkened behind it as Windows' Mica is, plain cards with a hairline edge, no glow or gradients. Pages that repeated each other are one now: Appearance is part of Personalisation, Screens of Display, About of System; the auto ducker is in Sound, the default apps in Apps, battery saver in Power & battery, Windows Update in Updates; one sidebar in Windows' order. Settings that a switch brings in grow open instead of popping in.
- **Liquid Glass is clear glass now**: no blur and no white haze (it was foggy), a fine rim instead of the white border, and a stronger lens bending what's around each edge, its colours split.
- **The Start menu's acrylic** is as see-through as the taskbar's.
- **Start's search learns**: what you open for a search comes first next time ("vs", then Visual Studio Code). Something that's both a setting and a command (Device Manager) is listed once, and settings, commands and files show their own icons.

**Fixed**
- **Files: the Name column couldn't be widened** (its edge was the one that didn't drag), and nothing showed a column's edge could be dragged: every edge drags now, with the resize pointer over it.
- **Disk Usage's tree works like a tool**: it reads folders on several threads at once, level by level, so every folder's size climbs together (it read one folder at a time, the rest showing nothing); folders still being worked out spin; a toolbar (Rescan, Stop, Expand one to three levels, Show in Files); click a column's title to order by it; clear blue bars whatever the accent; each size on a bar beside its name, as TreeSize has it; bold top folders, every other row shaded; lines from each open folder to what's in it, and Windows' own folder and document icons.
- **Cascade's app icons are the things themselves**: a folder, a picture, a calculator, a camera, a page, a clock, a play button, a microphone, a palette, a bulb, scissors, a gear, in flat colours, with no plate behind each and no gradients.
- **No animation when a window came back** from the taskbar: Windows doesn't animate a restore for its shell, so Cascade always does now, the window waiting until its picture has grown out of the button.
- **Wobbly windows didn't work on some apps** (browsers, Electron and WinUI apps, which draw on the GPU) **and had a black border** (Windows' invisible resize frame, printed black): the window's hidden another way that works for every app, and its picture is cut to what's seen of it, with round corners.
- **Lighting looked pasted into Settings**: it sits on the same backdrop now, with Settings' cards.
- **Gradients in Settings**: the backdrop is Windows' grey with only a hint of the wallpaper, and the theme samples are plain.
- **The pen was always pressed**: the driver took the pen's resting pressure from OpenTabletDriver's figure, and pens rest differently. It learns each pen's rest as it comes into reach, and has a threshold before it draws (with a little give, so it doesn't flicker).
- **Wobbly windows tore**: each piece of the window was bent as a slanted rectangle, which can't meet its neighbours; they're bent as triangles that share their edges.
- **No minimise animation for Cascade's own windows** (Settings, Files...), and none when restoring: the restore flew to where the window was parked off screen.
- **Cascade Snip not opening**: a snip that ended without tidying up blocked every one after it; that's cleared now, and the log says when one is refused.
- **Files**: the address and search boxes showed a box inside a box.

## 2026-11-21

**New**
- **Windows' everyday apps, Cascade's way** (in Start, and for text, music and video in shell mode, where the Store versions can't run): **Calculator** (Standard and Scientific, the answer as you type, history, the keyboard), **Camera** (photos to Camera Roll, video with the microphone's sound, switch cameras, mirror, self-timer), **Notepad** (open, save, word wrap, zoom, line and column), **Clock** (timers with rings, stopwatch with laps, alarms, world clock), **Media Player** (video and music, seek, volume, the folder as a playlist, repeat, fullscreen), **Sound Recorder** (the microphone to WAV with a level meter, play, show, delete) and **Paint** (pencil, brush, eraser, fill, picker, lines, rectangles, ellipses, palette, sizes, undo, zoom, PNG and JPEG). Each is its own app on the taskbar, with its own icon.
- **Minimise animation** (Settings > Personalisation > Windows): windows fly into their taskbar button when minimised and back out when restored, drawn from their own live picture, wherever Windows doesn't animate it itself. In shell mode Windows' own animation now gets to finish before a minimised window is tidied off the screen (it was cut short).
- **Wobbly windows** (Settings > Personalisation > Windows, off to start with): a window you drag bends like jelly, the part you hold following the pointer and the rest catching up, and wobbles as it settles. Works with super drag too.
- **A built-in pen tablet driver** (Settings > Bluetooth & devices > Pen tablet): 10moon, Vinsa and their relatives (USB 08F2:6811 and 0B57:902E) switched into tablet mode and read directly, as their own driver does, handed to Windows as a real pen (pressure in drawing apps) or as a mouse. Screen, proportions, left-handed, pressure firmness and the pen's two buttons can be set.
- **Recording with Cascade Snip**: a 3, 2, 1 countdown first (Esc cancels, click starts now), a new bar in Snip's look (a pulsing dot, the time, Stop; drag it anywhere) and, once saved, a preview card at the bottom right as Snip & Sketch's: the last frame, its length and size, Play, Folder, Copy and Delete.
- **Widgets' edit mode** (the pencil on the panel): each card made smaller or bigger and its corners rounded, square or round; every card drags into place, Notifications too.
- **Files, more like Explorer**: no separate title bar (the window's buttons sit in the tab row, drag by its empty part), Explorer's command bar (New, then Cut, Copy, Paste, Rename and Delete as icons, Sort, View, More), Windows' coloured icons in the sidebar, drive bars that turn red when nearly full, **folder sizes** in the Size column with TreeSize-style bars (at once from the instant-search index, else added up in the background), and its own icon on the taskbar (Photos and Disk Usage too).

**Improved**
- **Liquid Glass, visibly**: the web recipe's white tint, a shine along every edge (lit at the top left and bottom right), the light in the curved edge always drawn (the taskbar meets the screen's edge, where bending has nothing to show), a stronger sheen.
- **Settings look the part**: the accent glowing in behind the page, the open page lit in the sidebar, an icon by each page's title, cards with a light along their top and the accent along their edge under the pointer, headings marked with the accent.
- **The snip editor** is drawn as the rest of Cascade: icon buttons, round colour swatches, hover fades, a shadow round the snip.
- **Lighting is part of Cascade's settings**: Settings > Personalisation > Lighting (Lighting in Start opens it there too), no separate window. It keeps its first look (profiles and devices down the side, Lighting / Layout / Devices / Settings along the top), and now every button, toggle, slider, swatch and row lights up under the pointer and when pressed; toggles slide, selections glide, tabs slide in, and the live preview glows like the real LEDs. A narrow settings window widens to fit it.
- **OpenRGB sets itself up**: the first time Lighting opens it gets OpenRGB (with a progress bar), unpacks it, starts it and connects, and devices appear as OpenRGB finds them. It stays connected, reconnects if OpenRGB restarts, and says plainly what's wrong if it can't, with Try again and Use my own OpenRGB.
- **Administrator rights in one step**: turning them on installs the PawnIO driver (if it isn't there), sets OpenRGB to start with the rights and restarts it, all under one Windows permission prompt.

**Fixed**
- **The settings window smeared after opening Lighting**: widening it for Lighting moved it left each time and its new size never reached it (the black strip and leftover toggles on the right).
- **Snip editor: marks could be drawn over the toolbar**; they start below it now and stay on the snip.
- **Lighting couldn't find OpenRGB**: nothing fetched it unless Get OpenRGB was clicked, a dropped download failed without saying why, and Lighting stopped waiting for OpenRGB after ten seconds. Downloads now resume from scratch up to three times, are checked for size, and every step goes to `cascade.log`.

## 2026-11-20

**New**
- **Files**: Cascade's own file manager, so folders open with no Explorer at all: tabs, an address and search box, folders and drives in a sidebar, details or icons with thumbnails, rename in place, the shell's own menus, copy, cut, paste, delete, drag and drop, live updates, Explorer's shortcuts. In Start, on Win+E and for every folder in shell mode.
- **Photos**: a picture viewer for shell mode (where Windows' Photos can't open): zoom, pan, the folder's other pictures, rotate and save, fullscreen, delete, copy, set as wallpaper.
- **Instant file search** (Settings > Start menu): Start and Files find any file on your drives as you type, read straight from the drives' file tables and kept current from their change journals.
- **Disk Usage**: what's taking the space, as a map you can click into, with the biggest items listed.
- **Debloat** (Settings): switches for telemetry, Copilot, Recall, ads and suggestions, web search, feedback prompts, typing data, activity history, Edge in the background and update sharing. Every one can be turned back off; nothing is uninstalled.
- **Lighting**: RGB for every brand in one place, instead of iCUE, Synapse, G Hub, Armoury Crate, RGB Fusion and NGENUITY. Profiles (each can come on by itself while a program is in front), effect layers stacked over each other (Static, Gradient, Spectrum, Rainbow wave, Spiral, Colour shift, Pulse, Wave, Visor, Rain, Marquee, Sequential, Twinkle, Strobe, Heartbeat, Fire, Plasma, Temperature (CPU or graphics card), Audio visualiser and pulse, Screen ambience, Type key and Type ripple), each over chosen devices with its own colours, speed, size, direction, opacity and blend; a live preview; a layout canvas so effects run across all your devices as one, with fans as rings; each device's own hardware modes, saved to it if you like; fan and strip LED counts; off while the PC is locked. It drives the hardware through OpenRGB (free and open source, fetched for you), running out of sight. In Start, and Settings > Bluetooth & devices.
- **Liquid Glass, as the web makes it**: a light blur, a soft sheen from the top left, a rim of inset lines, and **Refraction** back: the edge bends what's around it like a lens (24 px of rim, up to 30 px of bend, falling off as the web recipe's displacement map does). Turn it off in Settings > Appearance for a drawn edge that costs nothing.

**Fixed**
- **Updates failing with "try again later"**: after an earlier update, a Cascade process still running from the previous copy (cascade.old.exe) kept that file locked, so the next update couldn't move the running exe aside. It now uses a fresh name when the old one is busy, tries the download three times, says which step failed, and writes why to `cascade.log`.
- **Super drag didn't move maximised windows or admin apps**: a maximised window now comes out of maximise under the pointer and follows it, and windows running as administrator (Task Manager and the like) move through Cascade's helper, which has the rights to. Cascade's own Settings window drags too.

## 2026-11-19

**New**
- **Windows' settings, in full**: every page goes as deep as Windows' own Settings app. **Network & internet**: each adapter's details, its IP (automatic, or an address, mask and gateway you set), DNS servers (or a provider such as Cloudflare or Quad9) and DNS over HTTPS; Wi-Fi properties, metered connection, the proxy, network reset. **Sound**: each device's own volume, balance, mute, format (sample rate and bit depth), default and default for calls, turning devices off and on; mono audio; what happens to other sounds during calls. **Power**: hibernate, energy saver, the power and sleep buttons and the lid. **Time & language**: set the time and time zone automatically, 24-hour clock, short date, first day of the week. **Privacy & security**: advertising ID, launch tracking, tailored experiences and ten more app permissions. **System**: your account, Storage Sense, clipboard history, Remote Desktop, environment variables, System Restore, advanced startup, reset this PC. **Personalisation**: lock screen picture, file extensions, hidden files. Changes only an administrator may make ask for permission, as Windows does.
- **New pages**: **Bluetooth & devices** (remove paired devices; the mouse's primary button, speed, precision, wheel, inactive-window scrolling and double-click speed; keyboard repeat, cursor blink, access-key underlines), **Gaming** (Game Mode, Game Bar, background recording, each app's GPU, hardware-accelerated GPU scheduling, windowed-game optimisations, variable refresh rate, Auto HDR) and **Accessibility** (text and pointer size, text cursor, animations, scrollbars, notification time, high contrast, Sticky / Filter / Toggle keys and the Shift-five-times shortcut).
- **Priority** (Settings > General): Cascade runs at High priority by default, out of Windows' efficiency mode, kept in memory under a game, and ahead on the GPU, so the taskbar never lags behind a busy app.
- **The desktop mirrors the Desktop folder**: icons are read straight from your Desktop folder and the public one (hidden files as Explorer is set to show them), and follow renames, new and deleted files, sizes and dates as they happen.

**Fixed**
- **Maximised windows going under the taskbar** (shell mode): when something resets a screen's work area (a game changing the resolution, a screen waking, a scale change), the taskbar's space is put back within a couple of seconds, and windows maximised meanwhile are fitted above it.
- **Apps couldn't be clicked after switching to shell mode with them open**: the desktop's icon layer came up over every open window and took their clicks. It now always stays under apps.
- **Dragging a selection on the desktop was choppy**: only what changed is redrawn, not every screen's worth of pixels.
- **Lag spikes**: the Open box (Change wallpaper, choosing an app), setting the wallpaper, a file's right-click menu and its commands, pasting, and anything that asks for administrator permission now run off Cascade's window thread, so the taskbar never stalls with them.

## 2026-11-18 (later)

**New**
- **The desktop's menu, as Windows 11's**: View (show icons, and which of This PC, your user folder, Network, the Recycle Bin and Control Panel are on the desktop), Sort by, Refresh, Paste, Paste shortcut, New (folder, shortcut, text document, ready to name), Display settings, Personalise, the wallpaper, Open in Terminal, Task Manager.
- **Arrange your screens** (Settings > Display): drag them to where they sit on your desk (they snap together), click one to choose it, and Identify shows each one's number.
- **Mobile hotspot, in full** (Settings > Network & Bluetooth): its name and password, band, security, which connection it shares, power saving, and the devices connected.
- **Known networks**: forget a Wi-Fi network Windows remembers.
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
- Right-clicking the desktop showed Cascade's menu instead of the desktop's (or the icon's own); Cascade's menu now only comes up on the taskbar and the notch.
- Deleting desktop icons did nothing: Delete (and the menu's Delete) now sends them to the Recycle Bin.
- The desktop showed everything under Desktop in File Explorer's sidebar (Libraries, Linux, a linked phone, two Control Panels...): now only what Explorer's desktop shows.
- Cascade's snip editor didn't show on the taskbar.
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
