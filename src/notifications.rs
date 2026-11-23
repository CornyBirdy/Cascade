//! Windows notifications for the widgets panel's Notifications card, read
//! through Windows' notification listener. Runs on its own thread and only
//! does anything when the panel asks (while it's open).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use windows::core::Result;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

use crate::msg;

pub enum NotifCmd {
    Refresh,
    Remove(u32),
    Clear,
}

#[derive(Clone)]
pub struct Notif {
    pub id: u32,
    /// The sending app's AppUserModelID, to open it when the notification is
    /// clicked (empty if Windows didn't say).
    pub aumid: String,
    pub app: String,
    pub title: String,
    pub body: String,
    /// For Cascade's own notifications (see `add_own`): the file that opens
    /// when it's clicked. Empty for Windows'.
    pub file: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Access {
    /// Not read yet.
    Unknown,
    Allowed,
    /// Turned off in Windows' privacy settings, or not available here.
    Denied,
}

pub struct Snapshot {
    pub access: Access,
    /// Newest first.
    pub items: Vec<Notif>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { access: Access::Unknown, items: Vec::new() }
    }
}

/// Cascade's own notifications (a screenshot or recording saved...), newest
/// first. They're not Windows toasts, so Windows' list never has them: the
/// panel's Notifications card adds them to what it reads (`merge_own`).
static OWN: Mutex<Vec<Notif>> = Mutex::new(Vec::new());
/// Their ids start here, clear of Windows' own.
const OWN_FIRST: u32 = 0x8000_0000;
static OWN_ID: AtomicU32 = AtomicU32::new(OWN_FIRST);
/// The most kept.
const OWN_MAX: usize = 30;

/// Whether `id` is one of Cascade's own notifications.
pub fn is_own(id: u32) -> bool {
    id >= OWN_FIRST
}

/// Adds a notification of Cascade's own to the panel's list: `app` says who
/// it's from, and clicking it opens `file` (if there is one).
pub fn add_own(app: &str, title: &str, body: &str, file: &str) {
    let id = OWN_ID.fetch_add(1, Ordering::Relaxed).max(OWN_FIRST);
    if let Ok(mut own) = OWN.lock() {
        own.insert(
            0,
            Notif {
                id,
                aumid: String::new(),
                app: app.to_string(),
                title: title.to_string(),
                body: body.to_string(),
                file: file.to_string(),
            },
        );
        own.truncate(OWN_MAX);
    }
}

pub fn remove_own(id: u32) {
    if let Ok(mut own) = OWN.lock() {
        own.retain(|n| n.id != id);
    }
}

pub fn clear_own() {
    if let Ok(mut own) = OWN.lock() {
        own.clear();
    }
}

/// `snapshot` with Cascade's own notifications first, newest first.
pub fn merge_own(snapshot: &mut Snapshot) {
    if let Ok(own) = OWN.lock() {
        if !own.is_empty() {
            snapshot.items.splice(0..0, own.iter().cloned());
        }
    }
}

pub fn spawn() -> Sender<NotifCmd> {
    let (tx, rx) = channel();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        run(rx);
    });
    tx
}

fn send(s: Snapshot) {
    let ptr = Box::into_raw(Box::new(s));
    if !msg::post(msg::NOTIFS, 0, ptr as isize) {
        drop(unsafe { Box::from_raw(ptr) });
    }
}

fn run(rx: Receiver<NotifCmd>) {
    let listener = UserNotificationListener::Current().ok();
    let mut asked = false;
    while let Ok(mut cmd) = rx.recv() {
        // Only the latest refresh matters; removals all count.
        loop {
            if let Some(l) = &listener {
                match &cmd {
                    NotifCmd::Remove(id) => {
                        let _ = l.RemoveNotification(*id);
                    }
                    NotifCmd::Clear => {
                        let _ = l.ClearNotifications();
                    }
                    NotifCmd::Refresh => {}
                }
            }
            match rx.try_recv() {
                Ok(next) => cmd = next,
                Err(_) => break,
            }
        }
        let snapshot = match &listener {
            Some(l) => read(l, &mut asked).unwrap_or(Snapshot { access: Access::Denied, items: Vec::new() }),
            None => Snapshot { access: Access::Denied, items: Vec::new() },
        };
        send(snapshot);
    }
}

fn read(l: &UserNotificationListener, asked: &mut bool) -> Result<Snapshot> {
    let mut status = l.GetAccessStatus()?;
    if status == UserNotificationListenerAccessStatus::Unspecified && !*asked {
        *asked = true;
        status = l.RequestAccessAsync()?.get()?;
    }
    if status != UserNotificationListenerAccessStatus::Allowed {
        return Ok(Snapshot { access: Access::Denied, items: Vec::new() });
    }
    let list = l.GetNotificationsAsync(NotificationKinds::Toast)?.get()?;
    let mut items = Vec::new();
    for i in 0..list.Size()? {
        if let Ok(n) = list.GetAt(i) {
            if let Some(item) = describe(&n) {
                items.push(item);
            }
        }
    }
    items.reverse();
    Ok(Snapshot { access: Access::Allowed, items })
}

fn describe(n: &UserNotification) -> Option<Notif> {
    let id = n.Id().ok()?;
    let info = n.AppInfo().ok();
    let aumid = info.as_ref().and_then(|a| a.AppUserModelId().ok()).map(|s| s.to_string()).unwrap_or_default();
    let app = info
        .as_ref()
        .and_then(|a| a.DisplayInfo().ok())
        .and_then(|d| d.DisplayName().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let binding =
        n.Notification().ok()?.Visual().ok()?.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()?;
    let texts = binding.GetTextElements().ok()?;
    let mut lines = Vec::new();
    for i in 0..texts.Size().unwrap_or(0) {
        if let Ok(t) = texts.GetAt(i).and_then(|t| t.Text()) {
            let t = t.to_string();
            if !t.trim().is_empty() {
                lines.push(t.replace(['\r', '\n'], " "));
            }
        }
    }
    let title = lines.first().cloned().unwrap_or_else(|| app.clone());
    let body = lines.get(1..).map(|l| l.join(" ")).unwrap_or_default();
    Some(Notif { id, aumid, app, title, body, file: String::new() })
}
