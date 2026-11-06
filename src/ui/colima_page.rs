//! The Colima page: what the VM is, the controls for it, and the live output of
//! the last start or stop.

use super::actions::{Action, Actions};
use super::glass;
use super::util::*;
use crate::backend::colima::{ColimaInfo, DEFAULT_PROFILE};
use crate::backend::model::{EngineVersion, human_gib};
use objc2::rc::Retained;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::RefCell;

const CARD_RADIUS: f64 = 12.0;
const RESOURCE_FIELDS: [&str; 6] = ["Status", "CPU", "Memory", "Memory in use", "Disk", "Disk in use"];
const ENGINE_FIELDS: [&str; 6] = ["Colima", "Runtime", "Virtual machine", "Mount", "Docker engine", "Docker socket"];

/// What the profile menu was just used for.
pub enum ProfilePick {
    Profile(String),
    New,
    Delete,
}

pub struct ColimaPage {
    pub view: Retained<NSView>,
    headline: Retained<NSTextField>,
    pub profile: Retained<NSPopUpButton>,
    /// The names `profile` lists now, so it is only rebuilt when they change:
    /// rebuilding would disturb a menu someone has open.
    profiles: RefCell<Vec<String>>,
    values: Vec<Retained<NSTextField>>,
    output: Retained<NSTextField>,
    frame: Retained<NSBox>,
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
        let profile = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        unsafe {
            profile.setTarget(Some(actions));
            profile.setAction(Some(sel!(dpPerform:)));
        }
        profile.setTag(Action::ColimaProfile as NSInteger);
        profile.setToolTip(Some(&ns("Colima profile")));
        let header = hstack(mtm, 10.0, &[&profile, &headline]);

        let start = action_button(mtm, actions, "Start", "play.fill", Action::ColimaStart);
        let stop = action_button(mtm, actions, "Stop", "stop.fill", Action::ColimaStop);
        let restart = action_button(mtm, actions, "Restart", "arrow.clockwise", Action::ColimaRestart);
        let resources = action_button(mtm, actions, "VM Settings…", "slider.horizontal.3", Action::ColimaResources);
        glass::button(&profile, false);
        glass::button(&start, true);
        glass::button(&stop, false);
        glass::button(&restart, false);
        glass::button(&resources, false);
        let buttons = hstack(mtm, 8.0, &[&start, &stop, &restart, &resources]);

        let mut values = Vec::new();
        let resources_card = card(mtm, &RESOURCE_FIELDS, &mut values);
        let engine_card = card(mtm, &ENGINE_FIELDS, &mut values);
        let cards = hstack(mtm, 12.0, &[&resources_card, &engine_card]);
        cards.setDistribution(NSStackViewDistribution::FillEqually);
        cards.setAlignment(NSLayoutAttribute::Top);

        let output = secondary_label(mtm, "Output", 12.0);
        let (scroll, console_text) = console(mtm);
        console_text.setTextContainerInset(NSSize::new(8.0, 8.0));
        let frame = NSBox::new(mtm);
        frame.setBoxType(NSBoxType::Custom);
        frame.setCornerRadius(CARD_RADIUS);
        frame.setBorderWidth(0.0);
        frame.setFillColor(&NSColor::quaternarySystemFillColor());
        frame.setContentViewMargins(NSSize::ZERO);
        frame.setContentView(Some(&scroll));
        frame.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
        frame.setContentCompressionResistancePriority_forOrientation(200.0, NSLayoutConstraintOrientation::Vertical);
        // Nothing to show until Colima has said something.
        output.setHidden(true);
        frame.setHidden(true);

        let stack = vstack(mtm, 6.0, &[&header, &buttons, &cards, &output, &frame]);
        stack.setCustomSpacing_afterView(14.0, &header);
        stack.setCustomSpacing_afterView(20.0, &buttons);
        stack.setCustomSpacing_afterView(20.0, &cards);
        view.addSubview(&stack);
        stack.setTranslatesAutoresizingMaskIntoConstraints(false);
        let safe = view.safeAreaLayoutGuide();
        // Weak, so the cards stay at the top while there is no output to fill the rest.
        let fill = stack.bottomAnchor().constraintEqualToAnchor_constant(&view.bottomAnchor(), -18.0);
        fill.setPriority(240.0);
        activate(&[
            fill,
            stack.topAnchor().constraintEqualToAnchor_constant(&safe.topAnchor(), 16.0),
            stack.bottomAnchor().constraintLessThanOrEqualToAnchor_constant(&view.bottomAnchor(), -18.0),
            stack.leadingAnchor().constraintEqualToAnchor_constant(&safe.leadingAnchor(), 24.0),
            stack.trailingAnchor().constraintEqualToAnchor_constant(&safe.trailingAnchor(), -24.0),
            cards.trailingAnchor().constraintEqualToAnchor(&stack.trailingAnchor()),
            frame.trailingAnchor().constraintEqualToAnchor(&stack.trailingAnchor()),
            frame.heightAnchor().constraintGreaterThanOrEqualToConstant(90.0),
        ]);

        Self {
            view,
            headline,
            profile,
            profiles: RefCell::default(),
            values,
            output,
            frame,
            start,
            stop,
            restart,
            resources,
            console: console_text,
        }
    }

    /// Lists `names` and the entries that act on them, with `current` chosen.
    pub fn set_profiles(&self, names: &[String], current: &str, enabled: bool) {
        if *self.profiles.borrow() != names {
            self.profile.removeAllItems();
            for name in names {
                self.profile.addItemWithTitle(&ns(name));
            }
            if let Some(menu) = self.profile.menu() {
                menu.addItem(&NSMenuItem::separatorItem(self.profile.mtm()));
            }
            for (title, action) in [("New Profile…", Action::ColimaNewProfile), ("Delete Profile…", Action::ColimaDeleteProfile)] {
                self.profile.addItemWithTitle(&ns(title));
                if let Some(item) = self.profile.lastItem() {
                    item.setTag(action as NSInteger);
                }
            }
            *self.profiles.borrow_mut() = names.to_vec();
        }
        self.profile.selectItemWithTitle(&ns(current));
        self.profile.setEnabled(enabled);
    }

    pub fn picked_profile(&self) -> ProfilePick {
        let Some(item) = self.profile.selectedItem() else { return ProfilePick::Profile(DEFAULT_PROFILE.into()) };
        match Action::from_tag(item.tag()) {
            Some(Action::ColimaNewProfile) => ProfilePick::New,
            Some(Action::ColimaDeleteProfile) => ProfilePick::Delete,
            _ => ProfilePick::Profile(item.title().to_string()),
        }
    }

    pub fn update(
        &self,
        info: &ColimaInfo,
        engine: Option<&EngineVersion>,
        memory_in_use: Option<String>,
        disk_in_use: Option<String>,
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
        // A stopped VM still has the type and mount its config fixes.
        let settings = info.settings.as_ref();
        let vm = live.map(|s| format!("{} ({})", s.driver, s.arch)).or(settings.map(|s| s.vm_type.clone()));
        let mount = live.map(|s| s.mount_type.clone()).or(settings.map(|s| s.mount_type.clone()));
        let docker = engine.map(|e| format!("{} (API {}, kernel {})", e.version, e.api_version, e.kernel_version));
        let socket = live.map(|s| s.docker_socket.trim_start_matches("unix://").to_string());

        let entries = [
            Some(status.to_string()), cpu, memory, memory_in_use, disk, disk_in_use,
            Some(info.version.clone()), runtime, vm, mount, docker, socket,
        ];
        for (field, value) in self.values.iter().zip(entries) {
            let value = value.filter(|v| !v.is_empty()).unwrap_or_else(|| "—".into());
            field.setStringValue(&ns(&value));
            // Long values are cut in the middle to fit their card.
            field.setToolTip(Some(&ns(&value)));
        }
    }

    pub fn clear_output(&self) {
        clear_text(&self.console);
        self.output.setHidden(true);
        self.frame.setHidden(true);
    }

    pub fn log_line(&self, line: &str) {
        self.output.setHidden(false);
        self.frame.setHidden(false);
        append_text(&self.console, &format!("{line}\n"));
        scroll_to_end(&self.console);
    }
}

/// Rows of label and value in a rounded group, as System Settings draws them.
/// Cards are content, so they stay plain: glass is for what floats above it.
fn card(mtm: MainThreadMarker, fields: &[&str], values: &mut Vec<Retained<NSTextField>>) -> Retained<NSView> {
    let rows = vstack(mtm, 0.0, &[]);
    rows.setAlignment(NSLayoutAttribute::CenterX);
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            let line = NSBox::new(mtm);
            line.setBoxType(NSBoxType::Separator);
            rows.addArrangedSubview(&line);
            activate(&[line.widthAnchor().constraintEqualToAnchor_constant(&rows.widthAnchor(), -28.0)]);
        }
        let key = secondary_label(mtm, field, 12.0);
        let value = label(mtm, "—", 12.0, unsafe { NSFontWeightRegular });
        value.setSelectable(true);
        value.setAlignment(NSTextAlignment::Right);
        value.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        value.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
        value.setContentCompressionResistancePriority_forOrientation(200.0, NSLayoutConstraintOrientation::Horizontal);
        let row = hstack(mtm, 12.0, &[&key, &value]);
        row.setEdgeInsets(NSEdgeInsets { top: 7.0, left: 14.0, bottom: 7.0, right: 14.0 });
        rows.addArrangedSubview(&row);
        activate(&[row.widthAnchor().constraintEqualToAnchor(&rows.widthAnchor())]);
        values.push(value);
    }
    let fill = NSBox::new(mtm);
    fill.setBoxType(NSBoxType::Custom);
    fill.setCornerRadius(CARD_RADIUS);
    fill.setBorderWidth(0.0);
    fill.setFillColor(&NSColor::quaternarySystemFillColor());
    let holder = NSView::new(mtm);
    holder.addSubview(&fill);
    holder.addSubview(&rows);
    pin(&fill, &holder);
    pin(&rows, &holder);
    holder
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
