//! The Colima page: what the VM is, the controls for it, and the live output of
//! the last start or stop.

use super::actions::{Action, Actions};
use super::glass;
use super::util::*;
use crate::backend::colima::ColimaInfo;
use crate::backend::model::{EngineVersion, human_gib};
use objc2::rc::Retained;
use objc2::sel;
use objc2_app_kit::*;
use objc2_foundation::*;

const KEY_COLUMN_WIDTH: f64 = 110.0;
const FIELDS: [&str; 10] = [
    "Status", "CPU", "Memory", "Memory in use", "Disk", "Runtime", "Virtual machine", "Mount", "Docker engine",
    "Docker socket",
];

pub struct ColimaPage {
    pub view: Retained<NSView>,
    headline: Retained<NSTextField>,
    values: Vec<Retained<NSTextField>>,
    pub start: Retained<NSButton>,
    pub stop: Retained<NSButton>,
    pub restart: Retained<NSButton>,
    pub resources: Retained<NSButton>,
    pub console: Retained<NSTextView>,
}

impl ColimaPage {
    pub fn new(mtm: MainThreadMarker, actions: &Actions) -> Self {
        let view = NSView::new(mtm);

        let headline = secondary_label(mtm, "", 13.0);

        let start = action_button(mtm, actions, "Start", "play.fill", Action::ColimaStart);
        let stop = action_button(mtm, actions, "Stop", "stop.fill", Action::ColimaStop);
        let restart = action_button(mtm, actions, "Restart", "arrow.clockwise", Action::ColimaRestart);
        let resources = action_button(mtm, actions, "Resources…", "slider.horizontal.3", Action::ColimaResources);
        glass::button(&start, true);
        glass::button(&stop, false);
        glass::button(&restart, false);
        glass::button(&resources, false);
        let buttons = hstack(mtm, 8.0, &[&start, &stop, &restart, &resources]);

        let grid = NSGridView::gridViewWithNumberOfColumns_rows(2, 0, mtm);
        grid.setRowSpacing(7.0);
        grid.setColumnSpacing(16.0);
        let mut values = Vec::new();
        for field in FIELDS {
            let key = secondary_label(mtm, field, 12.0);
            let value = label(mtm, "—", 12.0, unsafe { NSFontWeightRegular });
            value.setSelectable(true);
            let row: [&NSView; 2] = [&key, &value];
            grid.addRowWithViews(&NSArray::from_slice(&row));
            values.push(value);
        }
        let keys = grid.columnAtIndex(0);
        keys.setXPlacement(NSGridCellPlacement::Trailing);
        // A fixed width, or this column takes all the spare room and pushes the
        // values to the far edge of the window.
        keys.setWidth(KEY_COLUMN_WIDTH);
        grid.columnAtIndex(1).setXPlacement(NSGridCellPlacement::Leading);

        let output = secondary_label(mtm, "Output", 12.0);
        let (scroll, console_text) = console(mtm);
        console_text.setTextContainerInset(NSSize::new(6.0, 6.0));
        let frame = NSBox::new(mtm);
        frame.setBoxType(NSBoxType::Custom);
        frame.setCornerRadius(8.0);
        frame.setBorderColor(&NSColor::separatorColor());
        frame.setFillColor(&NSColor::controlBackgroundColor());
        frame.setContentViewMargins(NSSize::ZERO);
        frame.setContentView(Some(&scroll));
        frame.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
        frame.setContentCompressionResistancePriority_forOrientation(200.0, NSLayoutConstraintOrientation::Vertical);

        let stack = vstack(mtm, 6.0, &[&headline, &buttons, &grid, &output, &frame]);
        stack.setCustomSpacing_afterView(14.0, &headline);
        stack.setCustomSpacing_afterView(20.0, &buttons);
        stack.setCustomSpacing_afterView(20.0, &grid);
        view.addSubview(&stack);
        stack.setTranslatesAutoresizingMaskIntoConstraints(false);
        let safe = view.safeAreaLayoutGuide();
        activate(&[
            stack.topAnchor().constraintEqualToAnchor_constant(&safe.topAnchor(), 16.0),
            stack.bottomAnchor().constraintEqualToAnchor_constant(&view.bottomAnchor(), -18.0),
            stack.leadingAnchor().constraintEqualToAnchor_constant(&view.leadingAnchor(), 24.0),
            stack.trailingAnchor().constraintEqualToAnchor_constant(&view.trailingAnchor(), -24.0),
            frame.trailingAnchor().constraintEqualToAnchor(&stack.trailingAnchor()),
            frame.heightAnchor().constraintGreaterThanOrEqualToConstant(90.0),
        ]);

        Self { view, headline, values, start, stop, restart, resources, console: console_text }
    }

    pub fn update(
        &self,
        info: &ColimaInfo,
        engine: Option<&EngineVersion>,
        memory_in_use: Option<String>,
        headline: &str,
        status: &str,
    ) {
        self.headline.setStringValue(&ns(headline));
        let profile = info.profile.as_ref();
        let live = info.status.as_ref();
        let number = |live: Option<i64>, saved: Option<i64>| live.or(saved).filter(|n| *n > 0);

        let cpu = number(live.map(|s| s.cpu), profile.map(|p| p.cpus)).map(|n| n.to_string());
        let memory = number(live.map(|s| s.memory), profile.map(|p| p.memory)).map(human_gib);
        let disk = number(live.map(|s| s.disk), profile.map(|p| p.disk)).map(human_gib);
        let runtime = live.map(|s| s.runtime.clone()).or(profile.map(|p| p.runtime.clone()));
        let vm = live.map(|s| format!("{} ({})", s.driver, s.arch));
        let mount = live.map(|s| s.mount_type.clone());
        let docker = engine.map(|e| format!("{} (API {}, kernel {})", e.version, e.api_version, e.kernel_version));
        let socket = live.map(|s| s.docker_socket.trim_start_matches("unix://").to_string());

        let entries = [Some(status.to_string()), cpu, memory, memory_in_use, disk, runtime, vm, mount, docker, socket];
        for (field, value) in self.values.iter().zip(entries) {
            let value = value.filter(|v| !v.is_empty()).unwrap_or_else(|| "—".into());
            field.setStringValue(&ns(&value));
        }
    }

    pub fn log_line(&self, line: &str) {
        append_text(&self.console, &format!("{line}\n"));
        scroll_to_end(&self.console);
    }
}

fn action_button(
    mtm: MainThreadMarker,
    actions: &Actions,
    title: &str,
    icon: &str,
    action: Action,
) -> Retained<NSButton> {
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&ns(title), Some(actions), Some(sel!(dpPerform:)), mtm)
    };
    button.setImage(symbol(icon, title).as_deref());
    button.setImagePosition(NSCellImagePosition::ImageLeading);
    button.setTag(action as NSInteger);
    button
}
