//! The navigation sidebar. It is a plain source-list table inside an
//! `NSSplitViewItem` sidebar, which is what earns it the system's Liquid Glass;
//! adding glass or a visual effect view here would only obscure that.

use super::actions::{Action, Actions};
use super::controller::with_ctl;
use super::glass;
use super::pages::Page;
use super::util::*;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

enum Entry {
    Header(&'static str),
    Item(Page, &'static str),
}

const ENTRIES: [Entry; 7] = [
    Entry::Header("Colima"),
    Entry::Item(Page::Colima, "speedometer"),
    Entry::Header("Docker"),
    Entry::Item(Page::Containers, "shippingbox"),
    Entry::Item(Page::Images, "square.stack.3d.up"),
    Entry::Item(Page::Volumes, "externaldrive"),
    Entry::Item(Page::Storage, "internaldrive"),
];

pub struct SidebarIvars {
    table: OnceCell<Retained<NSTableView>>,
    counts: RefCell<HashMap<Page, Retained<NSTextField>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DPSidebar"]
    #[ivars = SidebarIvars]
    pub struct SidebarCtl;

    unsafe impl NSObjectProtocol for SidebarCtl {}
    unsafe impl NSControlTextEditingDelegate for SidebarCtl {}

    unsafe impl NSTableViewDataSource for SidebarCtl {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn row_count(&self, _table: &NSTableView) -> NSInteger {
            ENTRIES.len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for SidebarCtl {
        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(ENTRIES[row as usize], Entry::Header(_))
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(ENTRIES[row as usize], Entry::Item(..))
        }

        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn cell_view(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            Some(self.make_row(row as usize))
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, _notification: &NSNotification) {
            let Some(table) = self.ivars().table.get() else { return };
            let selected = table.selectedRow();
            if selected < 0 {
                return;
            }
            if let Entry::Item(page, _) = ENTRIES[selected as usize] {
                with_ctl(|ctl| ctl.show_page(page));
            }
        }
    }
);

impl SidebarCtl {
    fn make_row(&self, row: usize) -> Retained<NSView> {
        let mtm = self.mtm();
        let view = match ENTRIES[row] {
            Entry::Header(title) => header_cell(mtm, title),
            Entry::Item(page, icon) => {
                let (cell, count) = item_cell(mtm, page.sidebar_title(), icon);
                self.ivars().counts.borrow_mut().insert(page, count);
                cell
            }
        };
        Retained::into_super(view)
    }
}

fn header_cell(mtm: MainThreadMarker, title: &str) -> Retained<NSTableCellView> {
    let cell = NSTableCellView::new(mtm);
    let field = label(mtm, title, 11.0, unsafe { NSFontWeightSemibold });
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    cell.addSubview(&field);
    field.setTranslatesAutoresizingMaskIntoConstraints(false);
    activate(&[
        field.leadingAnchor().constraintEqualToAnchor_constant(&cell.leadingAnchor(), 4.0),
        field.centerYAnchor().constraintEqualToAnchor(&cell.centerYAnchor()),
    ]);
    cell
}

fn item_cell(mtm: MainThreadMarker, title: &str, icon: &str) -> (Retained<NSTableCellView>, Retained<NSTextField>) {
    let cell = NSTableCellView::new(mtm);
    let image = icon_view(mtm, icon, title);
    let name = label(mtm, title, 13.0, unsafe { NSFontWeightRegular });
    let count = secondary_label(mtm, "", 12.0);
    count.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })));
    for view in [&*image as &NSView, &name, &count] {
        cell.addSubview(view);
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
    }
    unsafe {
        cell.setImageView(Some(&image));
        cell.setTextField(Some(&name));
    }
    activate(&[
        image.leadingAnchor().constraintEqualToAnchor_constant(&cell.leadingAnchor(), 4.0),
        image.centerYAnchor().constraintEqualToAnchor(&cell.centerYAnchor()),
        image.widthAnchor().constraintEqualToConstant(18.0),
        name.leadingAnchor().constraintEqualToAnchor_constant(&image.trailingAnchor(), 7.0),
        name.centerYAnchor().constraintEqualToAnchor(&cell.centerYAnchor()),
        count.trailingAnchor().constraintEqualToAnchor_constant(&cell.trailingAnchor(), -8.0),
        count.centerYAnchor().constraintEqualToAnchor(&cell.centerYAnchor()),
        count.leadingAnchor().constraintGreaterThanOrEqualToAnchor_constant(&name.trailingAnchor(), 6.0),
    ]);
    (cell, count)
}

/// How Colima is doing, as shown at the foot of the sidebar.
pub struct StatusView {
    pub title: String,
    pub detail: String,
    pub dot: Retained<NSColor>,
    pub busy: bool,
    pub button: Option<(&'static str, &'static str, Action)>,
}

pub struct Sidebar {
    ctl: Retained<SidebarCtl>,
    pub table: Retained<NSTableView>,
    pub scroll: Retained<NSScrollView>,
    pub accessory: Retained<NSView>,
    dot: Retained<NSTextField>,
    title: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    spinner: Retained<NSProgressIndicator>,
    pub button: Retained<NSButton>,
}

impl Sidebar {
    pub fn new(mtm: MainThreadMarker, actions: &Actions) -> Self {
        let ctl = SidebarCtl::alloc(mtm).set_ivars(SidebarIvars {
            table: OnceCell::new(),
            counts: RefCell::default(),
        });
        let ctl: Retained<SidebarCtl> = unsafe { msg_send![super(ctl), init] };

        let table = NSTableView::new(mtm);
        table.setStyle(NSTableViewStyle::SourceList);
        table.setHeaderView(None);
        table.setRowSizeStyle(NSTableViewRowSizeStyle::Custom);
        table.setRowHeight(28.0);
        table.setFloatsGroupRows(false);
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns("main"));
        table.addTableColumn(&column);
        table.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*ctl)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
        }
        ctl.ivars().table.set(table.clone()).ok();

        let scroll = alloc_scroll(mtm);
        scroll.setHasHorizontalScroller(false);
        scroll.setDrawsBackground(false);
        scroll.setDocumentView(Some(&table));

        let dot = label(mtm, "●", 11.0, unsafe { NSFontWeightRegular });
        let title = label(mtm, "Colima", 12.0, unsafe { NSFontWeightSemibold });
        let detail = secondary_label(mtm, "", 11.0);
        let text = vstack(mtm, 1.0, &[&title, &detail]);
        let spinner = NSProgressIndicator::new(mtm);
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(NSControlSize::Small);
        spinner.setDisplayedWhenStopped(false);
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(&ns(""), Some(actions), Some(sel!(dpPerform:)), mtm)
        };
        glass::button(&button, false);
        button.setBorderShape(NSControlBorderShape::Circle);
        button.setImagePosition(NSCellImagePosition::ImageOnly);

        let spacer = NSView::new(mtm);
        spacer.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
        let row = hstack(mtm, 7.0, &[&dot, &text, &spacer, &spinner, &button]);
        row.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 14.0, bottom: 10.0, right: 12.0 });

        Self { ctl, table, scroll, accessory: Retained::into_super(row), dot, title, detail, spinner, button }
    }

    pub fn select(&self, page: Page) {
        let index = ENTRIES.iter().position(|entry| matches!(entry, Entry::Item(p, _) if *p == page));
        if let Some(index) = index {
            self.table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(index), false);
        }
    }

    pub fn set_count(&self, page: Page, count: Option<usize>) {
        if let Some(field) = self.ctl.ivars().counts.borrow().get(&page) {
            field.setStringValue(&ns(&count.map_or(String::new(), |c| c.to_string())));
        }
    }

    pub fn set_status(&self, status: StatusView) {
        self.title.setStringValue(&ns(&status.title));
        self.detail.setStringValue(&ns(&status.detail));
        self.detail.setHidden(status.detail.is_empty());
        self.dot.setTextColor(Some(&status.dot));
        if status.busy {
            unsafe { self.spinner.startAnimation(None) };
        } else {
            unsafe { self.spinner.stopAnimation(None) };
        }
        match status.button {
            Some((icon, description, action)) => {
                self.button.setImage(symbol(icon, description).as_deref());
                self.button.setToolTip(Some(&ns(description)));
                self.button.setTag(action as NSInteger);
                self.button.setHidden(false);
            }
            None => self.button.setHidden(true),
        }
    }
}
