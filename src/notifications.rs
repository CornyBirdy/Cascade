//! Windows notifications for the widgets panel's Notifications card, read
//! through Windows' notification listener. Runs on its own thread and only
//! does anything when the panel asks (while it's open).

use std::sync::mpsc::{channel, Receiver, Sender};

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

pub struct Notif {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub body: String,
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
    let app = n
        .AppInfo()
        .and_then(|a| a.DisplayInfo())
        .and_then(|d| d.DisplayName())
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
    Some(Notif { id, app, title, body })
}
