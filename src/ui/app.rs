//! Application delegate and the main menu.

use super::actions::{Action, Actions};
use super::controller::{self, Controller, with_ctl};
use super::util::ns;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::*;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DPAppDelegate"]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            let actions = Actions::new(mtm);
            let app = NSApplication::sharedApplication(mtm);
            app.setMainMenu(Some(&main_menu(mtm, &actions)));
            controller::launch(mtm, actions);
            #[cfg(feature = "devtools")]
            super::dev::start();
            app.activate();
        }

        // Colima keeps running after the window closes; it is its own service.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn quit_after_last_window(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            #[cfg(feature = "devtools")]
            super::dev::note_exit(self.mtm());
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            with_ctl(Controller::quit_reply).unwrap_or(NSApplicationTerminateReply::TerminateNow)
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("the app starts on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
    unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), action, &ns(key)) }
}

fn action_item(mtm: MainThreadMarker, actions: &Actions, title: &str, action: Action, key: &str) -> Retained<NSMenuItem> {
    let entry = item(mtm, title, Some(sel!(dpPerform:)), key);
    unsafe { entry.setTarget(Some(actions)) };
    entry.setTag(action as NSInteger);
    entry
}

fn submenu(mtm: MainThreadMarker, bar: &NSMenu, title: &str, items: &[Retained<NSMenuItem>]) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(title));
    for entry in items {
        menu.addItem(entry);
    }
    let holder = item(mtm, title, None, "");
    holder.setSubmenu(Some(&menu));
    bar.addItem(&holder);
    menu
}

fn main_menu(mtm: MainThreadMarker, actions: &Actions) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let separator = || NSMenuItem::separatorItem(mtm);

    let hide_others = item(mtm, "Hide Others", Some(sel!(hideOtherApplications:)), "h");
    hide_others.setKeyEquivalentModifierMask(NSEventModifierFlags::Option | NSEventModifierFlags::Command);
    submenu(mtm, &bar, "DockerPlus", &[
        item(mtm, "About DockerPlus", Some(sel!(orderFrontStandardAboutPanel:)), ""),
        separator(),
        item(mtm, "Hide DockerPlus", Some(sel!(hide:)), "h"),
        hide_others,
        item(mtm, "Show All", Some(sel!(unhideAllApplications:)), ""),
        separator(),
        item(mtm, "Quit DockerPlus", Some(sel!(terminate:)), "q"),
    ]);

    // Text fields only get copy and paste through these menu items.
    submenu(mtm, &bar, "Edit", &[
        item(mtm, "Cut", Some(sel!(cut:)), "x"),
        item(mtm, "Copy", Some(sel!(copy:)), "c"),
        item(mtm, "Paste", Some(sel!(paste:)), "v"),
        item(mtm, "Select All", Some(sel!(selectAll:)), "a"),
        separator(),
        action_item(mtm, actions, "Filter", Action::FocusSearch, "f"),
    ]);

    let toggle_sidebar = item(mtm, "Toggle Sidebar", Some(sel!(toggleSidebar:)), "s");
    toggle_sidebar.setKeyEquivalentModifierMask(NSEventModifierFlags::Control | NSEventModifierFlags::Command);
    submenu(mtm, &bar, "View", &[
        action_item(mtm, actions, "Colima", Action::ShowColima, "1"),
        action_item(mtm, actions, "Containers", Action::ShowContainers, "2"),
        action_item(mtm, actions, "Images", Action::ShowImages, "3"),
        action_item(mtm, actions, "Volumes", Action::ShowVolumes, "4"),
        separator(),
        action_item(mtm, actions, "Show Logs", Action::ShowLogs, "l"),
        action_item(mtm, actions, "Refresh", Action::Refresh, "r"),
        separator(),
        toggle_sidebar,
    ]);

    let window = submenu(mtm, &bar, "Window", &[
        item(mtm, "Close", Some(sel!(performClose:)), "w"),
        item(mtm, "Minimize", Some(sel!(performMiniaturize:)), "m"),
        item(mtm, "Zoom", Some(sel!(performZoom:)), ""),
    ]);
    NSApplication::sharedApplication(mtm).setWindowsMenu(Some(&window));
    bar
}
