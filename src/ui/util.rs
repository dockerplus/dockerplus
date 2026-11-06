//! Small helpers that keep the view code readable.

use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;

pub fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

pub fn symbol(name: &str, description: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), Some(&ns(description)))
}

/// An image view for a symbol. A symbol the system lacks leaves it blank: a
/// missing icon is not worth refusing to open the app over.
pub fn icon_view(mtm: MainThreadMarker, name: &str, description: &str) -> Retained<NSImageView> {
    let view = NSImageView::new(mtm);
    view.setImage(symbol(name, description).as_deref());
    view
}

pub fn label(mtm: MainThreadMarker, text: &str, size: f64, weight: NSFontWeight) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&ns(text), mtm);
    field.setFont(Some(&NSFont::systemFontOfSize_weight(size, weight)));
    field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    field
}

pub fn secondary_label(mtm: MainThreadMarker, text: &str, size: f64) -> Retained<NSTextField> {
    let field = label(mtm, text, size, unsafe { NSFontWeightRegular });
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field
}

pub fn activate(constraints: &[Retained<NSLayoutConstraint>]) {
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(constraints));
}

/// Pins `child` to all four edges of `parent`.
pub fn pin(child: &NSView, parent: &NSView) {
    child.setTranslatesAutoresizingMaskIntoConstraints(false);
    activate(&[
        child.topAnchor().constraintEqualToAnchor(&parent.topAnchor()),
        child.bottomAnchor().constraintEqualToAnchor(&parent.bottomAnchor()),
        child.leadingAnchor().constraintEqualToAnchor(&parent.leadingAnchor()),
        child.trailingAnchor().constraintEqualToAnchor(&parent.trailingAnchor()),
    ]);
}

pub fn hstack(mtm: MainThreadMarker, spacing: f64, views: &[&NSView]) -> Retained<NSStackView> {
    stack(mtm, NSUserInterfaceLayoutOrientation::Horizontal, spacing, views)
}

pub fn vstack(mtm: MainThreadMarker, spacing: f64, views: &[&NSView]) -> Retained<NSStackView> {
    let stack = stack(mtm, NSUserInterfaceLayoutOrientation::Vertical, spacing, views);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack
}

fn stack(
    mtm: MainThreadMarker,
    orientation: NSUserInterfaceLayoutOrientation,
    spacing: f64,
    views: &[&NSView],
) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(orientation);
    stack.setSpacing(spacing);
    for view in views {
        stack.addArrangedSubview(view);
    }
    stack
}

/// A read-only, monospaced, non-wrapping text view inside a scroll view.
pub fn console(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<NSTextView>) {
    let scroll = NSTextView::scrollableTextView(mtm);
    let text = scroll
        .documentView()
        .and_then(|view| view.downcast::<NSTextView>().ok())
        .expect("scrollableTextView always holds a text view");
    text.setEditable(false);
    text.setRichText(false);
    text.setUsesFontPanel(false);
    text.setAllowsUndo(false);
    text.setHorizontallyResizable(true);
    text.setAutomaticSpellingCorrectionEnabled(false);
    text.setDrawsBackground(false);
    // TextKit 2 lays out the whole document to scroll to its end, which is most
    // of the cost of following a busy log. Asking for the layout manager puts the
    // view on TextKit 1, where non-contiguous layout covers only what is visible.
    if let Some(layout) = unsafe { text.layoutManager() } {
        layout.setAllowsNonContiguousLayout(true);
    }
    if let Some(container) = unsafe { text.textContainer() } {
        container.setWidthTracksTextView(false);
        container.setContainerSize(NSSize::new(f64::MAX / 4.0, f64::MAX / 4.0));
    }
    scroll.setHasHorizontalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    scroll.setBorderType(NSBorderType::NoBorder);
    (scroll, text)
}

/// Appends plain text in the console's font.
pub fn append_text(view: &NSTextView, text: &str) {
    let Some(storage) = (unsafe { view.textStorage() }) else { return };
    let font = NSFont::monospacedSystemFontOfSize_weight(11.5, unsafe { NSFontWeightRegular });
    let color = NSColor::labelColor();
    let keys: [&NSAttributedStringKey; 2] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&objc2::runtime::AnyObject; 2] = [&font, &color];
    let attributes = NSDictionary::from_slices(&keys, &values);
    let piece = unsafe {
        NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &ns(text), Some(&attributes))
    };
    storage.appendAttributedString(&piece);
}

pub fn clear_text(view: &NSTextView) {
    view.setString(&ns(""));
}

pub fn scroll_to_end(view: &NSTextView) {
    let length = view.string().length();
    view.scrollRangeToVisible(NSRange::new(length, 0));
}

pub fn alloc_scroll(mtm: MainThreadMarker) -> Retained<NSScrollView> {
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::ZERO);
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setBorderType(NSBorderType::NoBorder);
    scroll
}

#[cfg(test)]
mod tests {
    use super::symbol;
    use std::path::Path;

    /// Finds every quoted name that looks like a symbol in the UI sources, so a
    /// renamed symbol fails here instead of leaving a blank button.
    #[test]
    fn every_symbol_used_by_the_ui_exists() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
        let known = ["play.fill", "stop.fill", "arrow.clockwise", "trash", "eraser", "xmark", "speedometer",
            "shippingbox", "externaldrive", "text.alignleft", "arrow.down.circle",
            "square.stack.3d.up", "arrow.down.to.line", "arrow.trianglehead.2.clockwise",
            "slider.horizontal.3", "line.3.horizontal.decrease.circle", "line.3.horizontal.decrease.circle.fill",
            "chart.bar", "chart.bar.fill", "sparkles", "magnifyingglass", "internaldrive", "network",
            "folder.badge.plus"];
        let sources: String = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        for name in known {
            assert!(sources.contains(&format!("\"{name}\"")), "{name} is no longer used");
            assert!(symbol(name, name).is_some(), "{name} does not exist on this system");
        }
    }
}
