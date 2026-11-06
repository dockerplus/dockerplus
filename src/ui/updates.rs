//! The AppKit side of "Check for Updates…". It runs only when the menu item is
//! chosen: git works on a background thread, then the answer comes back to the
//! main thread as an alert on the window.

use super::controller::{Controller, with_ctl};
use super::util::ns;
use crate::backend::update::{self, Notice, SOURCE_KEY};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2_app_kit::*;
use objc2_foundation::*;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

/// Keeps the menu item disabled while a fetch is out, so clicks do not stack up.
static CHECKING: AtomicBool = AtomicBool::new(false);

pub fn checking() -> bool {
    CHECKING.load(Ordering::Relaxed)
}

pub fn check() {
    if CHECKING.swap(true, Ordering::Relaxed) {
        return;
    }
    let source = NSUserDefaults::standardUserDefaults()
        .stringForKey(&ns(SOURCE_KEY))
        .map(|dir| dir.to_string())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    thread::spawn(move || {
        let notice = update::notice(&update::check(source.as_deref()));
        DispatchQueue::main().exec_async(move || {
            CHECKING.store(false, Ordering::Relaxed);
            with_ctl(|ctl| show(ctl, notice));
        });
    });
}

fn show(ctl: &Controller, notice: Notice) {
    let alert = NSAlert::new(ctl.mtm);
    alert.setAlertStyle(if notice.warning { NSAlertStyle::Warning } else { NSAlertStyle::Informational });
    alert.setMessageText(&ns(&notice.title));
    alert.setInformativeText(&ns(&notice.detail));
    let offer = notice.update;
    if offer.is_some() {
        alert.addButtonWithTitle(&ns("Update"));
        alert.addButtonWithTitle(&ns("Later"));
    }
    let respond = move |response: NSModalResponse| {
        if let (true, Some(dir)) = (response == NSAlertFirstButtonReturn, &offer) {
            with_ctl(|ctl| open_update(ctl, dir));
        }
    };
    ctl.window.makeKeyAndOrderFront(None);
    if ctl.window.attachedSheet().is_some() {
        respond(alert.runModal());
    } else {
        alert.beginSheetModalForWindow_completionHandler(&ctl.window, Some(&RcBlock::new(respond)));
    }
}

/// Pulls and reinstalls in Terminal, where the user can watch the build and
/// answer anything git asks. A .command file needs no Automation permission.
fn open_update(ctl: &Controller, dir: &Path) {
    let script = update::update_script(&update::git_tool(), dir);
    // Unique per request: the script deletes itself when it starts.
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let path = env::temp_dir().join(format!("dockerplus-update-{stamp}.command"));
    let written = fs::write(&path, script).and_then(|()| fs::set_permissions(&path, fs::Permissions::from_mode(0o700)));
    let url = NSURL::fileURLWithPath(&ns(&path.to_string_lossy()));
    if written.is_err() || !NSWorkspace::sharedWorkspace().openURL(&url) {
        let detail = path.display().to_string();
        show(ctl, Notice { title: "Could not open Terminal".into(), detail, warning: true, update: None });
    }
}
