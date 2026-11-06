//! A dense, sortable, filterable table used by the Containers, Images and
//! Volumes pages. The same data flows through all three: a list of [`Row`]s
//! keyed by a stable id, so selection survives every refresh.

use super::actions::{Action, Actions};
use super::controller::with_ctl;
use super::util::{alloc_scroll, ns};
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::{Cell as StdCell, OnceCell, RefCell};
use std::cmp::Ordering;

const ROW_HEIGHT: f64 = 24.0;
const FONT_SIZE: f64 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    pub title: &'static str,
    pub width: f64,
    pub min_width: f64,
    pub mono: bool,
    pub fixed: bool,
}

impl Column {
    pub const fn new(title: &'static str, width: f64, min_width: f64) -> Self {
        Self { title, width, min_width, mono: false, fixed: false }
    }

    pub const fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub const fn fixed(mut self) -> Self {
        self.fixed = true;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tone {
    Normal,
    Dim,
    Green,
    Orange,
    Red,
    Gray,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub text: String,
    /// Sorts numerically by this value instead of by text.
    pub sort: Option<i64>,
    pub tone: Tone,
}

impl Cell {
    pub fn text(text: &str) -> Self {
        Self { text: text.to_string(), sort: None, tone: Tone::Normal }
    }

    pub fn dim(text: &str) -> Self {
        Self { text: text.to_string(), sort: None, tone: Tone::Dim }
    }

    pub fn sorted_by(mut self, value: i64) -> Self {
        self.sort = Some(value);
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub cells: Vec<Cell>,
}

/// Keeps rows with any cell containing `query`, ignoring case.
pub fn filter_rows(rows: Vec<Row>, query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter(|row| row.cells.iter().any(|cell| cell.text.to_lowercase().contains(&query)))
        .collect()
}

pub fn sort_rows(rows: &mut [Row], column: usize, ascending: bool) {
    rows.sort_by(|a, b| {
        let (a, b) = (&a.cells[column], &b.cells[column]);
        let order = match (a.sort, b.sort) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => compare_text(&a.text, &b.text),
        };
        if ascending { order } else { order.reverse() }
    });
}

fn compare_text(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

pub struct TableIvars {
    columns: &'static [Column],
    source: RefCell<Vec<Row>>,
    rows: RefCell<Vec<Row>>,
    sort: StdCell<Option<(usize, bool)>>,
    table: OnceCell<Retained<NSTableView>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DPTable"]
    #[ivars = TableIvars]
    pub struct TableCtl;

    unsafe impl NSObjectProtocol for TableCtl {}
    unsafe impl NSControlTextEditingDelegate for TableCtl {}

    unsafe impl NSTableViewDataSource for TableCtl {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn row_count(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rows.borrow().len() as NSInteger
        }

        #[unsafe(method(tableView:sortDescriptorsDidChange:))]
        fn sort_changed(&self, table: &NSTableView, _old: &NSArray<NSSortDescriptor>) {
            let first = table.sortDescriptors().firstObject();
            let sort = first.and_then(|descriptor| {
                let key = descriptor.key()?.to_string();
                Some((key.parse().ok()?, descriptor.ascending()))
            });
            self.ivars().sort.set(sort);
            self.apply();
        }
    }

    unsafe impl NSTableViewDelegate for TableCtl {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn cell_view(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            self.cell_for(table, column, row)
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, _notification: &NSNotification) {
            with_ctl(|ctl| ctl.selection_changed());
        }
    }

    unsafe impl NSMenuDelegate for TableCtl {
        /// A right click acts on the clicked row even when it was not selected.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, _menu: &NSMenu) {
            let Some(table) = self.ivars().table.get() else { return };
            let clicked = table.clickedRow();
            if clicked >= 0 && !table.isRowSelected(clicked) {
                let row = NSIndexSet::indexSetWithIndex(clicked as usize);
                table.selectRowIndexes_byExtendingSelection(&row, false);
            }
        }
    }
);

impl TableCtl {
    fn cell_for(&self, table: &NSTableView, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
        let identifier = column?.identifier();
        let index: usize = identifier.to_string().parse().ok()?;
        let rows = self.ivars().rows.borrow();
        let cell = rows.get(row as usize)?.cells.get(index)?;
        let spec = self.ivars().columns[index];

        let view = unsafe { table.makeViewWithIdentifier_owner(&identifier, None) }
            .and_then(|view| view.downcast::<NSTableCellView>().ok())
            .unwrap_or_else(|| make_cell(&identifier, spec, self.mtm()));
        let field = unsafe { view.textField() }?;
        field.setStringValue(&ns(&cell.text));
        field.setTextColor(Some(&color(cell.tone)));
        Some(Retained::into_super(view))
    }

    fn rows_after_sort(&self) -> Vec<Row> {
        let mut rows = self.ivars().source.borrow().clone();
        if let Some((column, ascending)) = self.ivars().sort.get() {
            sort_rows(&mut rows, column, ascending);
        }
        rows
    }

    fn apply(&self) {
        let next = self.rows_after_sort();
        if *self.ivars().rows.borrow() == next {
            return;
        }
        let Some(table) = self.ivars().table.get() else { return };
        let selected = self.selected_keys();
        *self.ivars().rows.borrow_mut() = next;
        table.reloadData();

        let indexes = NSMutableIndexSet::new();
        for (index, row) in self.ivars().rows.borrow().iter().enumerate() {
            if selected.contains(&row.key) {
                indexes.addIndex(index);
            }
        }
        table.selectRowIndexes_byExtendingSelection(&indexes, false);
    }

    fn selected_keys(&self) -> Vec<String> {
        let Some(table) = self.ivars().table.get() else { return Vec::new() };
        let rows = self.ivars().rows.borrow();
        let selection = table.selectedRowIndexes();
        let mut keys = Vec::new();
        let mut index = selection.firstIndex();
        while index != NSNotFound as NSUInteger {
            if let Some(row) = rows.get(index) {
                keys.push(row.key.clone());
            }
            index = selection.indexGreaterThanIndex(index);
        }
        keys
    }
}

fn color(tone: Tone) -> Retained<NSColor> {
    match tone {
        Tone::Normal => NSColor::labelColor(),
        Tone::Dim => NSColor::secondaryLabelColor(),
        Tone::Green => NSColor::systemGreenColor(),
        Tone::Orange => NSColor::systemOrangeColor(),
        Tone::Red => NSColor::systemRedColor(),
        Tone::Gray => NSColor::tertiaryLabelColor(),
    }
}

/// Cells use plain frames and autoresizing: tables build many of them, and they
/// have no layout that needs the constraint solver.
fn make_cell(identifier: &NSString, spec: Column, mtm: MainThreadMarker) -> Retained<NSTableCellView> {
    let frame = NSRect::new(NSPoint::ZERO, NSSize::new(100.0, ROW_HEIGHT));
    let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), frame);
    cell.setIdentifier(Some(identifier));

    let field = NSTextField::labelWithString(&ns(""), mtm);
    field.setFrame(NSRect::new(NSPoint::new(2.0, 4.0), NSSize::new(96.0, 16.0)));
    field.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    field.setTranslatesAutoresizingMaskIntoConstraints(true);
    field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    let font = if spec.mono {
        NSFont::monospacedDigitSystemFontOfSize_weight(FONT_SIZE, unsafe { NSFontWeightRegular })
    } else {
        NSFont::systemFontOfSize(FONT_SIZE)
    };
    field.setFont(Some(&font));
    if spec.fixed {
        field.setAlignment(NSTextAlignment::Center);
    }
    cell.addSubview(&field);
    unsafe { cell.setTextField(Some(&field)) };
    cell
}

pub struct Table {
    pub ctl: Retained<TableCtl>,
    #[cfg_attr(not(feature = "devtools"), allow(dead_code))]
    pub view: Retained<NSTableView>,
    pub scroll: Retained<NSScrollView>,
}

impl Table {
    pub fn new(
        mtm: MainThreadMarker,
        columns: &'static [Column],
        actions: &Actions,
        menu_items: &[Option<(&str, Action)>],
    ) -> Self {
        let ctl = TableCtl::alloc(mtm).set_ivars(TableIvars {
            columns,
            source: RefCell::default(),
            rows: RefCell::default(),
            sort: StdCell::default(),
            table: OnceCell::new(),
        });
        let ctl: Retained<TableCtl> = unsafe { msg_send![super(ctl), init] };

        let view = NSTableView::new(mtm);
        view.setStyle(NSTableViewStyle::FullWidth);
        view.setRowSizeStyle(NSTableViewRowSizeStyle::Custom);
        view.setRowHeight(ROW_HEIGHT);
        view.setIntercellSpacing(NSSize::new(6.0, 0.0));
        view.setUsesAlternatingRowBackgroundColors(true);
        view.setAllowsMultipleSelection(true);
        view.setAllowsColumnReordering(false);
        view.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::LastColumnOnlyAutoresizingStyle);
        for (index, spec) in columns.iter().enumerate() {
            let key = ns(&index.to_string());
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &key);
            column.setTitle(&ns(spec.title));
            column.setWidth(spec.width);
            column.setMinWidth(spec.min_width);
            if spec.fixed {
                column.setMaxWidth(spec.width);
            }
            let prototype = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&key), true);
            column.setSortDescriptorPrototype(Some(&prototype));
            view.addTableColumn(&column);
        }

        unsafe {
            view.setDataSource(Some(ProtocolObject::from_ref(&*ctl)));
            view.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
            view.setTarget(Some(actions));
            view.setDoubleAction(Some(sel!(dpPerform:)));
        }
        view.setTag(Action::ShowLogs as NSInteger);

        let menu = NSMenu::new(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
        for item in menu_items {
            match item {
                Some((title, action)) => {
                    let entry = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &ns(title),
                            Some(sel!(dpPerform:)),
                            &ns(""),
                        )
                    };
                    unsafe { entry.setTarget(Some(actions)) };
                    entry.setTag(*action as NSInteger);
                    menu.addItem(&entry);
                }
                None => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            }
        }
        unsafe { view.setMenu(Some(&menu)) };

        let scroll = alloc_scroll(mtm);
        scroll.setDocumentView(Some(&view));

        ctl.ivars().table.set(view.clone()).ok();
        Self { ctl, view, scroll }
    }

    /// Replaces the rows, keeping the sort and selection. Does nothing when the
    /// data is unchanged, so a refresh never disturbs scrolling or a click.
    pub fn set_rows(&self, rows: Vec<Row>) {
        *self.ctl.ivars().source.borrow_mut() = rows;
        self.ctl.apply();
    }

    pub fn selected_keys(&self) -> Vec<String> {
        self.ctl.selected_keys()
    }

    #[cfg(feature = "devtools")]
    pub fn visible_text(&self) -> Vec<Vec<String>> {
        let rows = self.ctl.ivars().rows.borrow();
        rows.iter().map(|r| r.cells.iter().map(|c| c.text.clone()).collect()).collect()
    }

    #[cfg(feature = "devtools")]
    pub fn select_key(&self, needle: &str) -> bool {
        let rows = self.ctl.ivars().rows.borrow();
        let found = rows.iter().position(|row| {
            row.key.contains(needle) || row.cells.iter().any(|cell| cell.text == needle)
        });
        drop(rows);
        let Some(index) = found else { return false };
        self.view.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(index), false);
        true
    }

    #[cfg(feature = "devtools")]
    pub fn sort_by(&self, column: usize, ascending: bool) {
        let prototype = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&ns(&column.to_string())), ascending);
        self.view.setSortDescriptors(&NSArray::from_retained_slice(&[prototype]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, name: &str, size: i64) -> Row {
        Row { key: key.into(), cells: vec![Cell::text(name), Cell::text(&size.to_string()).sorted_by(size)] }
    }

    #[test]
    fn filtering_matches_any_cell_case_insensitively() {
        let rows = vec![row("1", "Nginx", 5), row("2", "postgres", 10)];
        assert_eq!(filter_rows(rows.clone(), "  NGI ").len(), 1);
        assert_eq!(filter_rows(rows.clone(), "10")[0].key, "2");
        assert_eq!(filter_rows(rows, "").len(), 2);
    }

    #[test]
    fn numeric_cells_sort_by_value_not_text() {
        let mut rows = vec![row("a", "a", 9), row("b", "b", 100), row("c", "c", 20)];
        sort_rows(&mut rows, 1, true);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["a", "c", "b"]);
        sort_rows(&mut rows, 1, false);
        assert_eq!(rows[0].key, "b");
    }

    #[test]
    fn text_sorts_ignoring_case() {
        let mut rows = vec![row("1", "beta", 0), row("2", "Alpha", 0)];
        sort_rows(&mut rows, 0, true);
        assert_eq!(rows[0].key, "2");
    }
}
