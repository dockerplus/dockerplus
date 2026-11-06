//! The floating status capsules over the tables, and the empty state shown
//! beneath them when there is no engine to talk to.

use super::actions::{Action, Actions};
use super::glass;
use super::util::*;
use crate::{mark, rex};
use objc2::rc::Retained;
use objc2::sel;
use objc2_app_kit::*;
use objc2_foundation::*;

const CAPSULE_HEIGHT: f64 = 32.0;
/// Room the tables keep clear so their last row can scroll past the capsules.
pub const HUD_CLEARANCE: f64 = 50.0;

/// Capsules in a glass container, shown only while there is something to report.
/// They sit above scrolling content, which is the one place custom glass belongs.
pub struct Hud {
    pub view: Retained<NSGlassEffectContainerView>,
    activity: Retained<NSTextField>,
    activity_capsule: Retained<NSGlassEffectView>,
    spinner: Retained<NSProgressIndicator>,
    stats: Retained<NSTextField>,
    stats_capsule: Retained<NSGlassEffectView>,
}

impl Hud {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let spinner = NSProgressIndicator::new(mtm);
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(NSControlSize::Small);
        let activity = label(mtm, "", 12.0, unsafe { NSFontWeightRegular });
        let activity_capsule = glass::capsule(mtm, &padded(mtm, &[&spinner, &activity]), CAPSULE_HEIGHT / 2.0, false);
        activity_capsule.setHidden(true);

        let stats = label(mtm, "", 12.0, unsafe { NSFontWeightRegular });
        stats.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })));
        let stats_capsule = glass::capsule(mtm, &padded(mtm, &[&stats]), CAPSULE_HEIGHT / 2.0, false);
        stats_capsule.setHidden(true);

        let row = hstack(mtm, 8.0, &[&stats_capsule, &activity_capsule]);
        let view = glass::container(mtm, &row, 16.0);
        for capsule in [&stats_capsule, &activity_capsule] {
            activate(&[capsule.heightAnchor().constraintEqualToConstant(CAPSULE_HEIGHT)]);
        }
        Self { view, activity, activity_capsule, spinner, stats, stats_capsule }
    }

    pub fn set_stats(&self, text: Option<&str>) {
        self.stats_capsule.setHidden(text.is_none());
        if let Some(text) = text {
            self.stats.setStringValue(&ns(text));
        }
    }

    pub fn set_activity(&self, text: Option<&str>) {
        self.activity_capsule.setHidden(text.is_none());
        match text {
            Some(text) => {
                self.activity.setStringValue(&ns(text));
                unsafe { self.spinner.startAnimation(None) };
            }
            None => unsafe { self.spinner.stopAnimation(None) },
        }
    }

    #[cfg(feature = "devtools")]
    pub fn text(&self) -> (String, String) {
        let stats = if self.stats_capsule.isHidden() { String::new() } else { self.stats.stringValue().to_string() };
        (self.activity.stringValue().to_string(), stats)
    }
}

fn padded(mtm: MainThreadMarker, views: &[&NSView]) -> Retained<NSStackView> {
    let row = hstack(mtm, 6.0, views);
    row.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 14.0, bottom: 0.0, right: 14.0 });
    row
}

pub struct EmptyState {
    pub view: Retained<NSStackView>,
    title: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    pub button: Retained<NSButton>,
}

impl EmptyState {
    pub fn new(mtm: MainThreadMarker, actions: &Actions) -> Self {
        let image = NSImageView::new(mtm);
        image.setImage(Some(&mark::template_image(rex::mono(), rex::SIZE, rex::SIZE, 96.0)));
        image.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));

        let title = label(mtm, "", 16.0, unsafe { NSFontWeightSemibold });
        title.setAlignment(NSTextAlignment::Center);
        let detail = secondary_label(mtm, "", 12.5);
        detail.setAlignment(NSTextAlignment::Center);
        detail.setSelectable(true);

        let button = unsafe {
            NSButton::buttonWithTitle_target_action(&ns("Start Colima"), Some(actions), Some(sel!(dpPerform:)), mtm)
        };
        glass::button(&button, true);
        button.setControlSize(NSControlSize::Large);
        button.setTag(Action::ColimaStart as NSInteger);

        let view = vstack(mtm, 8.0, &[&image, &title, &detail, &button]);
        view.setAlignment(NSLayoutAttribute::CenterX);
        view.setCustomSpacing_afterView(14.0, &detail);
        Self { view, title, detail, button }
    }

    pub fn set(&self, title: &str, detail: &str, show_button: bool) {
        self.title.setStringValue(&ns(title));
        self.detail.setStringValue(&ns(detail));
        self.button.setHidden(!show_button);
    }
}
