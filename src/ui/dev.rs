//! Development-only remote control, compiled in with `--features devtools`.
//!
//! macOS will not let an unattended process use Accessibility or Screen
//! Recording without a human granting them, so checks drive the real controls
//! through this socket instead: toolbar items go through `NSApp sendAction:`,
//! buttons through `performClick:`, and rows through the table's own selection.
//! Set `DOCKERPLUS_DEV_SOCK` to a path to enable it.

use super::actions::Action;
use super::controller::{Controller, with_ctl};
use super::pages::Page;
use super::table::Table;
use super::util::ns;
use dispatch2::DispatchQueue;
use objc2::encode::{Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::{AnyThread, msg_send};
use objc2_app_kit::*;
use objc2_foundation::*;
use serde_json::json;
use std::ffi::{CString, c_char, c_void};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// `kCGWindowListOptionOnScreenAboveWindow`.
const ON_SCREEN_ABOVE_WINDOW: u32 = 1 << 1;

/// Leaves a line beside the dev socket saying what ended the app.
pub fn note_exit(mtm: MainThreadMarker) {
    let Some(mut path) = std::env::var_os("DOCKERPLUS_DEV_SOCK") else { return };
    path.push(".exit");
    let event = NSApplication::sharedApplication(mtm).currentEvent();
    // Only the type: `subtype` raises on mouse events, and that killed the app
    // inside this very note.
    let cause = event.map_or("no event".to_string(), |e| format!("event type {}", e.r#type().0));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let line = format!("{now} terminating, {cause}\n");
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path);
    let _ = file.and_then(|mut file| file.write_all(line.as_bytes()));
}

pub fn start() {
    let Some(path) = std::env::var_os("DOCKERPLUS_DEV_SOCK") else { return };
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("dev socket path is writable");
    thread::spawn(move || {
        for mut connection in listener.incoming().flatten() {
            let mut line = String::new();
            let Ok(clone) = connection.try_clone() else { continue };
            if BufReader::new(clone).read_line(&mut line).is_err() {
                continue;
            }
            let (tx, rx) = mpsc::channel();
            DispatchQueue::main().exec_async(move || {
                let reply = with_ctl(|ctl| ctl.dev_command(line.trim())).unwrap_or_default();
                let _ = tx.send(reply);
            });
            let reply = rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|_| "timeout".into());
            let _ = writeln!(connection, "{reply}");
        }
    });
}

impl Controller {
    /// Records what caused a page change: a real mouse or key event shows up
    /// here as the event AppKit was dispatching at the time.
    pub fn note_page_change(&self, page: Page) {
        let app = NSApplication::sharedApplication(self.mtm);
        let cause = match app.currentEvent() {
            Some(event) => {
                let point = event.locationInWindow();
                format!("event type {:?} at {:.0},{:.0}", event.r#type().0, point.x, point.y)
            }
            None => "no event".to_string(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        self.page_log.borrow_mut().push(format!("{now:.2} {} <- {cause}", page.title()));
    }

    fn dev_command(&self, line: &str) -> String {
        let (command, argument) = line.split_once(' ').unwrap_or((line, ""));
        match command {
            "state" => self.dev_state(),
            "page" => self.dev_page(argument),
            "select" => self.dev_table().map_or("no table".into(), |t| t.select_key(argument).to_string()),
            "rows" => self.dev_table().map_or("no table".into(), |t| json!(t.visible_text()).to_string()),
            "do" => self.dev_toolbar(argument),
            "action" => Action::from_name(argument).map_or("unknown".into(), |action| {
                self.perform(action);
                "ok".into()
            }),
            "click" => self.dev_click(argument),
            "sheet" => self.dev_sheet(argument),
            "profiles" => self.dev_profiles(),
            "profile" => self.dev_profile(argument),
            "filter" => {
                self.toolbar.set_search_text(argument);
                self.perform(Action::Filter);
                "ok".into()
            }
            "sort" => {
                let (column, direction) = argument.split_once(' ').unwrap_or((argument, "asc"));
                match (self.dev_table(), column.parse()) {
                    (Some(table), Ok(column)) => {
                        table.sort_by(column, direction == "asc");
                        "ok".into()
                    }
                    _ => "bad sort".into(),
                }
            }
            "front" => {
                let app = NSApplication::sharedApplication(self.mtm);
                #[allow(deprecated)]
                app.activateIgnoringOtherApps(true);
                self.window.makeKeyAndOrderFront(None);
                format!("active={} key={}", app.isActive(), self.window.isKeyWindow())
            }
            "key" => self.dev_key(argument),
            "clickrow" => self.dev_click_row(argument),
            "clickarrow" => self.dev_click_arrow(argument),
            "clicksidebar" => self.dev_click_sidebar(argument),
            "clickheader" => self.dev_click_header(argument),
            "clickbutton" => self.dev_click_button(argument),
            "clicktool" => self.dev_click_tool(argument),
            "rightclick" => self.dev_right_click(argument),
            "sortstate" => self.dev_table().map_or("no table".into(), |table| {
                table.view.sortDescriptors().iter().map(|d| {
                    format!("{}:{}", d.key().map_or(String::new(), |k| k.to_string()), d.ascending())
                }).collect::<Vec<_>>().join(",")
            }),
            "fileicon" => {
                let (from, to) = argument.split_once(' ').unwrap_or((argument, ""));
                let icon = NSWorkspace::sharedWorkspace().iconForFile(&ns(from));
                icon.setSize(NSSize::new(512.0, 512.0));
                match icon.TIFFRepresentation().and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff)) {
                    Some(rep) => {
                        let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
                        png.map_or("no png".into(), |d| d.writeToFile_atomically(&ns(to), true).to_string())
                    }
                    None => "no image".into(),
                }
            }
            "responder" => self.window.firstResponder().map_or("none".into(), |r| r.class().name().to_string_lossy().to_string()),
            // `bench <n>`: microseconds per table render and per full render.
            "bench" => {
                let runs: u32 = argument.parse().unwrap_or(100);
                let start = std::time::Instant::now();
                for _ in 0..runs {
                    self.render_tables();
                }
                let tables = start.elapsed().as_micros() / runs as u128;
                let start = std::time::Instant::now();
                for _ in 0..runs {
                    self.render();
                }
                json!({"tables_us": tables, "render_us": start.elapsed().as_micros() / runs as u128}).to_string()
            }
            "fetches" => {
                let counts: Vec<usize> = crate::backend::docker::FETCHES.iter().map(|c| c.load(std::sync::atomic::Ordering::Relaxed)).collect();
                json!({"containers": counts[0], "images": counts[1], "volumes": counts[2], "networks": counts[3]}).to_string()
            }
            // Replacement characters in the log view: text damaged by decoding.
            "logbad" => self.logs.text.string().to_string().matches('\u{fffd}').count().to_string(),
            "loglen" => self.logs.text.string().length().to_string(),
            "modal" => self.dev_modal(argument),
            "close" => {
                self.window.performClose(None);
                "closing".into()
            }
            "pagelog" => self.page_log.borrow().join("\n"),
            "selectall" => self.dev_table().map_or("no table".into(), |table| {
                unsafe { table.view.selectAll(None) };
                "ok".into()
            }),
            "appearance" => {
                let name = if argument == "dark" { unsafe { NSAppearanceNameDarkAqua } } else { unsafe { NSAppearanceNameAqua } };
                self.window.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
                "ok".into()
            }
            "scroll" => self.dev_table().map_or("no table".into(), |table| {
                table.view.scrollRowToVisible(argument.parse().unwrap_or(0));
                "ok".into()
            }),
            "logs" => self.logs.text.string().to_string(),
            "loglast" => self.logs.text.string().to_string().lines().last().unwrap_or("").to_string(),
            "console" => self.colima_page.console.string().to_string(),
            "menu" => self.dev_menu(),
            "menupick" => self.dev_menu_pick(argument),
            "snapshot" => self.dev_snapshot(argument),
            // A sheet will not capture on its own, so take the window and
            // everything stacked above it.
            "sheetshot" => self.dev_capture(self.window.windowNumber(), argument, ON_SCREEN_ABOVE_WINDOW),
            "views" => {
                let mut out = String::new();
                if let Some(root) = self.window.contentView().and_then(|view| unsafe { view.superview() }) {
                    dump(&root, 0, &mut out);
                }
                out
            }
            "resize" => {
                let (w, h) = argument.split_once(' ').unwrap_or(("1120", "720"));
                let size = NSSize::new(w.parse().unwrap_or(1120.0), h.parse().unwrap_or(720.0));
                self.window.setContentSize(size);
                "ok".into()
            }
            _ => format!("unknown command: {command}"),
        }
    }

    fn dev_table(&self) -> Option<&Table> {
        match self.st.borrow().page {
            Page::Colima => None,
            Page::Containers => Some(&self.containers),
            Page::Images => Some(&self.images),
            Page::Volumes => Some(&self.volumes),
            Page::Networks => Some(&self.networks),
            Page::Storage => Some(&self.storage),
        }
    }

    fn dev_state(&self) -> String {
        let st = self.st.borrow();
        let (summary, activity, stats) = self.hud.text();
        let mut enabled = serde_json::Map::new();
        for name in [
            "ContainerStart", "ContainerStop", "ContainerRestart", "ShowLogs", "Pull", "Remove", "RunningOnly",
            "ToggleStats", "Prune", "Refresh", "ComposeUp",
        ] {
            if let Some(item) = Action::from_name(name).and_then(|a| self.toolbar.item(a)) {
                enabled.insert(name.into(), json!({"hidden": item.isHidden(), "enabled": item.isEnabled()}));
            }
        }
        json!({
            "page": st.page.title(),
            "title": self.window.title().to_string(),
            "subtitle": self.window.subtitle().to_string(),
            "engine_up": st.engine_up,
            "colima_installed": st.info.as_ref().map(|i| i.installed),
            "colima_running": st.info.as_ref().map(|i| i.running),
            "colima_version": st.info.as_ref().map(|i| i.version.clone()),
            "colima_profile": st.profile,
            "containers": st.containers.iter().map(|c| c.name()).collect::<Vec<_>>(),
            "images": st.images.len(),
            "volumes": st.volumes.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            "networks": st.networks.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            "selected": self.dev_table().map(Table::selected_keys),
            "hud": {"summary": summary, "activity": activity, "stats": stats},
            "stats_on": st.stats_on,
            "toolbar": enabled,
            "logs_visible": !self.logs.view.isHidden(),
            "sheet": self.window.attachedSheet().is_some(),
            "running_only": st.running_only,
        })
        .to_string()
    }

    /// The container context menu as it would open now: titles, whether each
    /// item is enabled, and what its submenu entries open.
    fn dev_menu(&self) -> String {
        let Some(menu) = self.containers.view.menu() else { return "no menu".into() };
        self.menu_will_open(&menu);
        let mut out = Vec::new();
        for item in menu.itemArray().iter() {
            if item.isSeparatorItem() {
                continue;
            }
            let entries: Vec<serde_json::Value> = item.submenu().map_or_else(Vec::new, |submenu| {
                submenu
                    .itemArray()
                    .iter()
                    .map(|entry| {
                        let location = entry.representedObject().and_then(|o| o.downcast::<NSString>().ok());
                        json!({"title": entry.title().to_string(), "opens": location.map(|l| l.to_string())})
                    })
                    .collect()
            });
            let enabled = item.hasSubmenu() || Action::from_tag(item.tag()).is_none_or(|action| self.can(action));
            out.push(json!({"title": item.title().to_string(), "enabled": enabled, "submenu": entries}));
        }
        json!(out).to_string()
    }

    /// Picks a context menu item, or a submenu entry, by its title. Goes
    /// through the menu's own target-action, refusing disabled items.
    fn dev_menu_pick(&self, title: &str) -> String {
        let Some(menu) = self.containers.view.menu() else { return "no menu".into() };
        self.menu_will_open(&menu);
        for (index, item) in menu.itemArray().iter().enumerate() {
            if item.title().to_string() == title {
                if Action::from_tag(item.tag()).is_some_and(|action| !self.can(action)) {
                    return "refused: disabled".into();
                }
                menu.performActionForItemAtIndex(index as NSInteger);
                return "performed".into();
            }
            if let Some(submenu) = item.submenu() {
                for (index, entry) in submenu.itemArray().iter().enumerate() {
                    if entry.title().to_string() == title {
                        submenu.performActionForItemAtIndex(index as NSInteger);
                        return "performed".into();
                    }
                }
            }
        }
        "no such menu item".into()
    }

    fn dev_page(&self, name: &str) -> String {
        let action = match name {
            "colima" => Action::ShowColima,
            "containers" => Action::ShowContainers,
            "images" => Action::ShowImages,
            "volumes" => Action::ShowVolumes,
            "networks" => Action::ShowNetworks,
            "storage" => Action::ShowStorage,
            _ => return "unknown page".into(),
        };
        self.perform(action);
        "ok".into()
    }

    /// Fires a toolbar item the way a click does, refusing when it is disabled.
    fn dev_toolbar(&self, name: &str) -> String {
        let Some(item) = Action::from_name(name).and_then(|a| self.toolbar.item(a)) else {
            return "no such toolbar item".into();
        };
        if item.isHidden() || !item.isEnabled() {
            return format!("refused: hidden={} enabled={}", item.isHidden(), item.isEnabled());
        }
        let app = NSApplication::sharedApplication(self.mtm);
        let sent = unsafe { app.sendAction_to_from(item.action().unwrap(), item.target().as_deref(), Some(&item)) };
        format!("sent={sent}")
    }

    fn dev_click(&self, name: &str) -> String {
        let button = match name {
            "colima-start" => &self.colima_page.start,
            "colima-stop" => &self.colima_page.stop,
            "colima-restart" => &self.colima_page.restart,
            "colima-resources" => &self.colima_page.resources,
            "sidebar" => &self.sidebar.button,
            "empty-start" => &self.empty.button,
            _ => return "no such button".into(),
        };
        if button.isHiddenOrHasHiddenAncestor() || !button.isEnabled() {
            return "refused: hidden or disabled".into();
        }
        unsafe { button.performClick(None) };
        "clicked".into()
    }

    /// The profile menu as shown, and what is known about the current profile.
    fn dev_profiles(&self) -> String {
        let popup = &self.colima_page.profile;
        let titles: Vec<String> = popup.itemTitles().iter().map(|t| t.to_string()).collect();
        let st = self.st.borrow();
        let settings = st.info.as_ref().and_then(|i| i.settings.as_ref());
        json!({
            "current": st.profile,
            "menu": titles,
            "selected": popup.titleOfSelectedItem().map(|t| t.to_string()),
            "enabled": popup.isEnabled(),
            "listed": st.info.as_ref().map(|i| i.profiles.clone()),
            "settings": settings.map(|s| json!({
                "vm_type": s.vm_type, "mount_type": s.mount_type, "rosetta": s.rosetta, "kubernetes": s.kubernetes,
            })),
        })
        .to_string()
    }

    /// Picks a profile menu entry, "New Profile…" included, the way a click does.
    fn dev_profile(&self, title: &str) -> String {
        let popup = &self.colima_page.profile;
        if popup.isHiddenOrHasHiddenAncestor() || !popup.isEnabled() {
            return "refused: hidden or disabled".into();
        }
        if popup.itemWithTitle(&ns(title)).is_none() {
            return "no such profile".into();
        }
        popup.selectItemWithTitle(&ns(title));
        let app = NSApplication::sharedApplication(self.mtm);
        let sent = unsafe { app.sendAction_to_from(popup.action().unwrap(), popup.target().as_deref(), Some(popup)) };
        format!("sent={sent}")
    }

    /// With no argument, lists the attached sheet; otherwise presses a button.
    fn dev_sheet(&self, argument: &str) -> String {
        let Some(sheet) = self.window.attachedSheet() else { return "no sheet".into() };
        let Some(root) = sheet.contentView() else { return "empty sheet".into() };
        let mut buttons = Vec::new();
        let mut texts = Vec::new();
        collect(&root, &mut buttons, &mut texts);
        if let Some(rest) = argument.strip_prefix("choose ") {
            let (index, title) = rest.split_once(' ').unwrap_or((rest, ""));
            let popups: Vec<Retained<NSPopUpButton>> =
                buttons.iter().filter_map(|b| b.clone().downcast::<NSPopUpButton>().ok()).collect();
            return match index.parse::<usize>().ok().and_then(|i| popups.get(i)) {
                Some(popup) => {
                    popup.selectItemWithTitle(&ns(title));
                    popup.titleOfSelectedItem().map_or_else(String::new, |t| t.to_string())
                }
                None => "no such popup".into(),
            };
        }
        if let Some(text) = argument.strip_prefix("type ") {
            return match texts.iter().find(|field| field.isEditable()) {
                Some(field) => {
                    field.setStringValue(&ns(text));
                    "typed".into()
                }
                None => "no editable field".into(),
            };
        }
        if argument.is_empty() {
            let titles: Vec<String> = buttons.iter().map(|b| b.title().to_string()).collect();
            let checked: Vec<String> = buttons
                .iter()
                .filter(|b| (*b).clone().downcast::<NSPopUpButton>().is_err() && b.state() == NSControlStateValueOn)
                .map(|b| b.title().to_string())
                .collect();
            let labels: Vec<String> = texts.iter().map(|t| t.stringValue().to_string()).collect();
            let mut documents = Vec::new();
            collect_documents(&root, &mut documents);
            return json!({"buttons": titles, "checked": checked, "text": labels, "documents": documents}).to_string();
        }
        match buttons.iter().find(|b| b.title().to_string() == argument) {
            Some(button) => {
                unsafe { button.performClick(None) };
                "pressed".into()
            }
            None => "no such sheet button".into(),
        }
    }

    // Synthetic input. These build real NSEvents and hand them to `NSApp
    // sendEvent:`, so menu key equivalents, hit testing, click counts and table
    // tracking all run as usual. Only the hardware and WindowServer are skipped.

    fn dev_event(&self, kind: NSEventType, at: NSPoint, clicks: NSInteger) -> Retained<NSEvent> {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            at,
            NSEventModifierFlags::empty(),
            NSProcessInfo::processInfo().systemUptime(),
            self.window.windowNumber(),
            None,
            0,
            clicks,
            1.0,
        )
        .expect("mouse event")
    }

    /// Queues the mouse-up first, because controls track the mouse in a loop
    /// inside `mouseDown:` and would otherwise wait for it forever.
    fn dev_click_at(&self, at: NSPoint, count: NSInteger) {
        let app = NSApplication::sharedApplication(self.mtm);
        for click in 1..=count {
            app.postEvent_atStart(&self.dev_event(NSEventType::LeftMouseUp, at, click), true);
            app.sendEvent(&self.dev_event(NSEventType::LeftMouseDown, at, click));
        }
    }

    fn dev_key(&self, spec: &str) -> String {
        let mut parts: Vec<&str> = spec.split('+').collect();
        let Some(key) = parts.pop() else { return "no key".into() };
        let mut flags = NSEventModifierFlags::empty();
        for part in parts {
            flags |= match part {
                "cmd" => NSEventModifierFlags::Command,
                "shift" => NSEventModifierFlags::Shift,
                "ctrl" => NSEventModifierFlags::Control,
                "opt" => NSEventModifierFlags::Option,
                _ => return format!("unknown modifier {part}"),
            };
        }
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::ZERO,
            flags,
            NSProcessInfo::processInfo().systemUptime(),
            self.window.windowNumber(),
            None,
            &ns(key),
            &ns(key),
            false,
            0,
        )
        .expect("key event");
        NSApplication::sharedApplication(self.mtm).sendEvent(&event);
        "sent".into()
    }

    fn dev_row_point(&self, needle: &str) -> Option<NSPoint> {
        let table = self.dev_table()?;
        let rect = table.view.rectOfRow(table.index_of(needle)? as NSInteger);
        let center = NSPoint::new(rect.origin.x + 40.0, rect.origin.y + rect.size.height / 2.0);
        Some(table.view.convertPoint_toView(center, None))
    }

    /// `clickrow <name> [count]`
    fn dev_click_row(&self, argument: &str) -> String {
        let (needle, count) = match argument.rsplit_once(' ') {
            Some((needle, count)) if count.parse::<NSInteger>().is_ok() => (needle, count.parse().unwrap_or(1)),
            _ => (argument, 1),
        };
        let Some(point) = self.dev_row_point(needle) else { return "no such row".into() };
        self.dev_click_at(point, count);
        "clicked".into()
    }

    /// Clicks the first column of a row, where a project's arrow is.
    fn dev_click_arrow(&self, needle: &str) -> String {
        let Some(table) = self.dev_table() else { return "no table".into() };
        let Some(index) = table.index_of(needle) else { return "no such row".into() };
        let rect = table.view.rectOfRow(index as NSInteger);
        let at = NSPoint::new(rect.origin.x + 10.0, rect.origin.y + rect.size.height / 2.0);
        self.dev_click_at(table.view.convertPoint_toView(at, None), 1);
        "clicked".into()
    }

    fn dev_click_sidebar(&self, argument: &str) -> String {
        let rect = self.sidebar.table.rectOfRow(argument.parse().unwrap_or(0));
        let center = NSPoint::new(rect.origin.x + 40.0, rect.origin.y + rect.size.height / 2.0);
        self.dev_click_at(self.sidebar.table.convertPoint_toView(center, None), 1);
        "clicked".into()
    }

    fn dev_click_header(&self, argument: &str) -> String {
        let Some(table) = self.dev_table() else { return "no table".into() };
        let Some(header) = table.view.headerView() else { return "no header".into() };
        let rect = header.headerRectOfColumn(argument.parse().unwrap_or(0));
        let center = NSPoint::new(rect.origin.x + rect.size.width / 2.0, rect.origin.y + rect.size.height / 2.0);
        self.dev_click_at(header.convertPoint_toView(center, None), 1);
        "clicked".into()
    }

    fn dev_click_button(&self, name: &str) -> String {
        let button = match name {
            "colima-resources" => &self.colima_page.resources,
            "colima-restart" => &self.colima_page.restart,
            "sidebar" => &self.sidebar.button,
            _ => return "no such button".into(),
        };
        let frame = button.bounds();
        let center = NSPoint::new(frame.size.width / 2.0, frame.size.height / 2.0);
        self.dev_click_at(button.convertPoint_toView(center, None), 1);
        "clicked".into()
    }

    /// Clicks the nth visible toolbar item, counting from the left.
    fn dev_click_tool(&self, argument: &str) -> String {
        let Some(root) = self.window.contentView().and_then(|view| unsafe { view.superview() }) else {
            return "no frame view".into();
        };
        let mut viewers = Vec::new();
        collect_toolbar_viewers(&root, &mut viewers);
        viewers.sort_by(|a, b| a.0.total_cmp(&b.0));
        let Some(index) = argument.parse::<usize>().ok() else {
            return format!("{} items", viewers.len());
        };
        let Some((_, view)) = viewers.get(index) else { return "no such item".into() };
        let frame = view.bounds();
        let center = NSPoint::new(frame.size.width / 2.0, frame.size.height / 2.0);
        self.dev_click_at(view.convertPoint_toView(center, None), 1);
        "clicked".into()
    }

    /// Builds the menu for a real right-click, without opening the blocking
    /// menu loop, and returns what ended up selected.
    fn dev_right_click(&self, needle: &str) -> String {
        let Some(point) = self.dev_row_point(needle) else { return "no such row".into() };
        let table = &self.containers;
        let event = self.dev_event(NSEventType::RightMouseDown, point, 1);
        let Some(menu) = table.view.menuForEvent(&event) else { return "no menu".into() };
        let _: () = unsafe { msg_send![&*table.ctl, menuNeedsUpdate: &*menu] };
        json!({"selected": table.selected_keys().len(), "items": menu.numberOfItems()}).to_string()
    }

    /// Like `sheet`, for an app-modal alert.
    fn dev_modal(&self, argument: &str) -> String {
        let app = NSApplication::sharedApplication(self.mtm);
        let Some(root) = app.modalWindow().and_then(|window| window.contentView()) else {
            return "no modal".into();
        };
        let (mut buttons, mut texts) = (Vec::new(), Vec::new());
        collect(&root, &mut buttons, &mut texts);
        if argument.is_empty() {
            let titles: Vec<String> = buttons.iter().map(|b| b.title().to_string()).collect();
            let labels: Vec<String> = texts.iter().map(|t| t.stringValue().to_string()).collect();
            return json!({"buttons": titles, "text": labels}).to_string();
        }
        match buttons.iter().find(|b| b.title().to_string() == argument) {
            Some(button) => {
                unsafe { button.performClick(None) };
                "pressed".into()
            }
            None => "no such modal button".into(),
        }
    }

    /// Captures this app's own window, which needs no Screen Recording grant.
    /// The function is hidden from the current SDK headers, hence `dlsym`.
    fn dev_snapshot(&self, path: &str) -> String {
        self.dev_capture(self.window.windowNumber(), path, 0)
    }

    fn dev_capture(&self, window_number: NSInteger, path: &str, extra_options: u32) -> String {
        #[repr(C)]
        struct CGImage {
            _private: [u8; 0],
        }
        unsafe impl RefEncode for CGImage {
            const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Rect(f64, f64, f64, f64);
        unsafe extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
        const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
        const INCLUDING_WINDOW: u32 = 1 << 3;
        const BOUNDS_IGNORE_FRAMING: u32 = 1;

        unsafe {
            let name = CString::new("CGWindowListCreateImage").unwrap();
            let symbol = dlsym(RTLD_DEFAULT, name.as_ptr());
            if symbol.is_null() {
                return "CGWindowListCreateImage is gone".into();
            }
            let capture: extern "C" fn(Rect, u32, u32, u32) -> *mut CGImage = std::mem::transmute(symbol);
            let null_rect = Rect(f64::INFINITY, f64::INFINITY, 0.0, 0.0);
            let image = capture(null_rect, INCLUDING_WINDOW | extra_options, window_number as u32, BOUNDS_IGNORE_FRAMING);
            if image.is_null() {
                return "capture failed".into();
            }
            let bitmap: Retained<NSBitmapImageRep> =
                msg_send![NSBitmapImageRep::alloc(), initWithCGImage: image];
            let properties = NSDictionary::new();
            match bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties) {
                Some(data) if data.writeToFile_atomically(&ns(path), true) => "saved".into(),
                _ => "write failed".into(),
            }
        }
    }
}

fn collect_toolbar_viewers(view: &NSView, out: &mut Vec<(f64, Retained<NSView>)>) {
    for child in view.subviews().iter() {
        let name = child.class().name().to_string_lossy().to_string();
        if name.contains("ToolbarItemViewer") && !child.isHiddenOrHasHiddenAncestor() {
            let at = child.convertPoint_toView(NSPoint::ZERO, None);
            out.push((at.x, child.clone()));
        }
        collect_toolbar_viewers(&child, out);
    }
}

fn collect(view: &NSView, buttons: &mut Vec<Retained<NSButton>>, texts: &mut Vec<Retained<NSTextField>>) {
    for child in view.subviews().iter() {
        if let Ok(button) = child.clone().downcast::<NSButton>() {
            buttons.push(button);
        } else if let Ok(text) = child.clone().downcast::<NSTextField>() {
            texts.push(text);
        }
        collect(&child, buttons, texts);
    }
}

/// The text of every text view under `view`, such as the details sheet's.
fn collect_documents(view: &NSView, out: &mut Vec<String>) {
    for child in view.subviews().iter() {
        if let Ok(text) = child.clone().downcast::<NSTextView>() {
            out.push(text.string().to_string());
        }
        collect_documents(&child, out);
    }
}

fn dump(view: &NSView, depth: usize, out: &mut String) {
    let frame = view.frame();
    out.push_str(&format!(
        "{:indent$}{}{} {:.0},{:.0} {:.0}x{:.0}\n",
        "",
        view.class().name().to_string_lossy(),
        if view.isHidden() { " [hidden]" } else { "" },
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
        indent = depth,
    ));
    if depth < 28 {
        for child in view.subviews().iter() {
            dump(&child, depth + 1, out);
        }
    }
}
