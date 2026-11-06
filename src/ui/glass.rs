//! Thin wrappers over AppKit's real Liquid Glass views.
//!
//! Only controls that float above content use these directly. The sidebar and
//! toolbar get their glass from the system for linking against the macOS 26 SDK.

use objc2::rc::Retained;
use objc2::{msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::*;

/// Wraps `content` in a capsule of glass.
pub fn capsule(
    mtm: MainThreadMarker,
    content: &NSView,
    corner_radius: f64,
    interactive: bool,
) -> Retained<NSGlassEffectView> {
    let glass = NSGlassEffectView::new(mtm);
    glass.setCornerRadius(corner_radius);
    glass.setContentView(Some(content));
    if interactive {
        set_interactive(&glass);
    }
    glass
}

/// `effectIsInteractive` arrived in macOS 27; on 26 the glass just stays static.
fn set_interactive(glass: &NSGlassEffectView) {
    if glass.respondsToSelector(sel!(setEffectIsInteractive:)) {
        unsafe {
            let _: () = msg_send![glass, setEffectIsInteractive: true];
        }
    }
}

/// Groups glass so nearby pieces merge and render in one pass.
pub fn container(mtm: MainThreadMarker, content: &NSView, spacing: f64) -> Retained<NSGlassEffectContainerView> {
    let container = NSGlassEffectContainerView::new(mtm);
    container.setSpacing(spacing);
    container.setContentView(Some(content));
    container
}

/// A push button drawn in glass, optionally tinted as the primary action.
pub fn button(button: &NSButton, prominent: bool) {
    button.setBezelStyle(NSBezelStyle::Glass);
    if prominent {
        button.setTintProminence(NSTintProminence::Primary);
    }
}
