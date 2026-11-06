//! Every button, toolbar item and menu item funnels through one Objective-C
//! target, carrying an [`Action`] in its `tag`.

use super::controller::with_ctl;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSMenuItem, NSMenuItemValidation, NSView};
use objc2_foundation::{MainThreadMarker, NSObject, NSString};

macro_rules! actions {
    ($($name:ident = $tag:literal),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        #[repr(isize)]
        pub enum Action { $($name = $tag),* }

        impl Action {
            pub fn from_tag(tag: isize) -> Option<Self> {
                match tag { $($tag => Some(Self::$name),)* _ => None }
            }

            #[cfg(feature = "devtools")]
            pub fn from_name(name: &str) -> Option<Self> {
                match name { $(stringify!($name) => Some(Self::$name),)* _ => None }
            }
        }
    };
}

actions! {
    ColimaStart = 1,
    ColimaStop = 2,
    ColimaRestart = 3,
    ColimaResources = 4,
    ContainerStart = 10,
    ContainerStop = 11,
    ContainerRestart = 12,
    Remove = 13,
    ShowLogs = 14,
    Pull = 15,
    Refresh = 16,
    CopyId = 17,
    Prune = 18,
    FocusSearch = 20,
    Filter = 21,
    ShowColima = 30,
    ShowContainers = 31,
    ShowImages = 32,
    ShowVolumes = 33,
    ShowStorage = 34,
    ShowNetworks = 35,
    LogsFollow = 40,
    LogsClear = 41,
    LogsClose = 42,
    LogsFind = 43,
    ContainerPause = 50,
    ContainerResume = 51,
    ShowDetails = 52,
    OpenTerminal = 53,
    OpenPort = 54,
    RevealMount = 55,
    SelectProject = 56,
    RunningOnly = 57,
    ToggleStats = 58,
    ColimaProfile = 60,
    ColimaNewProfile = 61,
    ColimaDeleteProfile = 62,
    ColimaConfig = 63,
    ComposeUp = 70,
    RunImage = 71,
    CheckUpdates = 80,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DPActions"]
    pub struct Actions;

    unsafe impl NSObjectProtocol for Actions {}

    impl Actions {
        #[unsafe(method(dpPerform:))]
        fn perform(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            if let Some(action) = Action::from_tag(tag) {
                with_ctl(|ctl| ctl.perform(action));
            }
        }

        /// The disclosure arrow on a project's header row.
        #[unsafe(method(dpToggleRow:))]
        fn toggle_row(&self, sender: &NSView) {
            with_ctl(|ctl| ctl.arrow_clicked(sender));
        }

        /// Submenu entries carry a URL or a host path in `representedObject`.
        #[unsafe(method(dpOpen:))]
        fn open(&self, sender: &NSMenuItem) {
            let location = sender.representedObject().and_then(|object| object.downcast::<NSString>().ok());
            if let Some(location) = location {
                with_ctl(|ctl| ctl.open_location(&location.to_string()));
            }
        }
    }

    unsafe impl NSMenuItemValidation for Actions {
        #[unsafe(method(validateMenuItem:))]
        fn validate(&self, item: &NSMenuItem) -> bool {
            Action::from_tag(item.tag())
                .and_then(|action| with_ctl(|ctl| ctl.can(action)))
                .unwrap_or(true)
        }
    }
);

impl Actions {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}
