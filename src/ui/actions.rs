//! Every button, toolbar item and menu item funnels through one Objective-C
//! target, carrying an [`Action`] in its `tag`.

use super::controller::with_ctl;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSMenuItem, NSMenuItemValidation};
use objc2_foundation::{MainThreadMarker, NSObject};

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
    ContainerStart = 10,
    ContainerStop = 11,
    ContainerRestart = 12,
    Remove = 13,
    ShowLogs = 14,
    Pull = 15,
    Refresh = 16,
    CopyId = 17,
    FocusSearch = 20,
    Filter = 21,
    ShowColima = 30,
    ShowContainers = 31,
    ShowImages = 32,
    ShowVolumes = 33,
    LogsFollow = 40,
    LogsClear = 41,
    LogsClose = 42,
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
