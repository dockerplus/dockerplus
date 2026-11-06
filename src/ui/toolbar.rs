//! The window toolbar. Items are ordinary `NSToolbarItem`s, which macOS draws on
//! Liquid Glass and groups by itself, so nothing here styles them.

use super::actions::{Action, Actions};
use super::util::*;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

struct Item {
    id: &'static str,
    action: Action,
    label: &'static str,
    icon: &'static str,
}

const ITEMS: [Item; 11] = [
    Item { id: "dp.start", action: Action::ContainerStart, label: "Start", icon: "play.fill" },
    Item { id: "dp.stop", action: Action::ContainerStop, label: "Stop", icon: "stop.fill" },
    Item { id: "dp.restart", action: Action::ContainerRestart, label: "Restart", icon: "arrow.clockwise" },
    Item { id: "dp.logs", action: Action::ShowLogs, label: "Logs", icon: "text.alignleft" },
    Item { id: "dp.compose-up", action: Action::ComposeUp, label: "Compose Up", icon: "folder.badge.plus" },
    Item { id: "dp.pull", action: Action::Pull, label: "Pull Image", icon: "arrow.down.circle" },
    Item { id: "dp.remove", action: Action::Remove, label: "Remove", icon: "trash" },
    Item { id: "dp.running", action: Action::RunningOnly, label: "Running Only", icon: "line.3.horizontal.decrease.circle" },
    Item { id: "dp.stats", action: Action::ToggleStats, label: "CPU and Memory", icon: "chart.bar" },
    Item { id: "dp.prune", action: Action::Prune, label: "Remove Unused", icon: "sparkles" },
    Item { id: "dp.refresh", action: Action::Refresh, label: "Refresh", icon: "arrow.trianglehead.2.clockwise" },
];

const SEARCH_ID: &str = "dp.search";

pub struct ToolbarIvars {
    actions: Retained<Actions>,
    items: RefCell<HashMap<Action, Retained<NSToolbarItem>>>,
    search: OnceCell<Retained<NSSearchToolbarItem>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DPToolbar"]
    #[ivars = ToolbarIvars]
    pub struct ToolbarCtl;

    unsafe impl NSObjectProtocol for ToolbarCtl {}

    unsafe impl NSToolbarDelegate for ToolbarCtl {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn item_for(
            &self,
            _toolbar: &NSToolbar,
            identifier: &NSString,
            _inserted: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            self.item_named(identifier)
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            identifiers()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_items(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            identifiers()
        }
    }
);

fn identifiers() -> Retained<NSArray<NSString>> {
    let mut ids: Vec<Retained<NSString>> = unsafe {
        vec![
            NSToolbarToggleSidebarItemIdentifier.copy(),
            NSToolbarSidebarTrackingSeparatorItemIdentifier.copy(),
            NSToolbarFlexibleSpaceItemIdentifier.copy(),
        ]
    };
    ids.extend(ITEMS.iter().map(|item| ns(item.id)));
    ids.push(ns(SEARCH_ID));
    NSArray::from_retained_slice(&ids)
}

impl ToolbarCtl {
    fn item_named(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        let id = identifier.to_string();
        if id == SEARCH_ID {
            return Some(self.make_search(identifier));
        }
        let spec = ITEMS.iter().find(|item| item.id == id)?;
        Some(self.make_item(identifier, spec))
    }

    fn make_item(&self, identifier: &NSString, spec: &Item) -> Retained<NSToolbarItem> {
        let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), identifier);
        item.setLabel(&ns(spec.label));
        item.setToolTip(Some(&ns(spec.label)));
        item.setImage(symbol(spec.icon, spec.label).as_deref());
        item.setBordered(true);
        item.setTag(spec.action as NSInteger);
        item.setAutovalidates(false);
        unsafe {
            item.setTarget(Some(&self.ivars().actions));
            item.setAction(Some(sel!(dpPerform:)));
        }
        self.ivars().items.borrow_mut().insert(spec.action, item.clone());
        item
    }

    fn make_search(&self, identifier: &NSString) -> Retained<NSToolbarItem> {
        let item = NSSearchToolbarItem::initWithItemIdentifier(NSSearchToolbarItem::alloc(self.mtm()), identifier);
        item.setPreferredWidthForSearchField(190.0);
        let field = item.searchField();
        field.setPlaceholderString(Some(&ns("Filter")));
        field.setSendsSearchStringImmediately(true);
        field.setTag(Action::Filter as NSInteger);
        unsafe {
            field.setTarget(Some(&self.ivars().actions));
            field.setAction(Some(sel!(dpPerform:)));
        }
        self.ivars().search.set(item.clone()).ok();
        Retained::into_super(item)
    }
}

pub struct Toolbar {
    ctl: Retained<ToolbarCtl>,
    pub toolbar: Retained<NSToolbar>,
}

impl Toolbar {
    pub fn new(mtm: MainThreadMarker, actions: &Retained<Actions>) -> Self {
        let ctl = ToolbarCtl::alloc(mtm).set_ivars(ToolbarIvars {
            actions: actions.clone(),
            items: RefCell::default(),
            search: OnceCell::new(),
        });
        let ctl: Retained<ToolbarCtl> = unsafe { msg_send![super(ctl), init] };
        let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &ns("main"));
        toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
        toolbar.setAllowsUserCustomization(false);
        toolbar.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*ctl)));
        Self { ctl, toolbar }
    }

    pub fn set(&self, action: Action, visible: bool, enabled: bool) {
        if let Some(item) = self.ctl.ivars().items.borrow().get(&action) {
            item.setHidden(!visible);
            item.setEnabled(enabled);
        }
    }

    pub fn set_icon(&self, action: Action, icon: &str, tip: &str) {
        if let Some(item) = self.ctl.ivars().items.borrow().get(&action) {
            item.setImage(symbol(icon, tip).as_deref());
            item.setToolTip(Some(&ns(tip)));
        }
    }

    pub fn set_search_visible(&self, visible: bool) {
        if let Some(item) = self.ctl.ivars().search.get() {
            item.setHidden(!visible);
        }
    }

    pub fn search_text(&self) -> String {
        self.ctl.ivars().search.get().map_or(String::new(), |item| item.searchField().stringValue().to_string())
    }

    pub fn focus_search(&self) {
        if let Some(item) = self.ctl.ivars().search.get() {
            item.beginSearchInteraction();
        }
    }

    #[cfg(feature = "devtools")]
    pub fn set_search_text(&self, text: &str) {
        if let Some(item) = self.ctl.ivars().search.get() {
            item.searchField().setStringValue(&ns(text));
        }
    }

    #[cfg(feature = "devtools")]
    pub fn item(&self, action: Action) -> Option<Retained<NSToolbarItem>> {
        self.ctl.ivars().items.borrow().get(&action).cloned()
    }
}
