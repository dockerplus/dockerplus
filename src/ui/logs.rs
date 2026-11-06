//! The container log panel: a plain text view with a floating glass control
//! capsule. Log volume can be huge, so text is batched before it reaches AppKit
//! and the view keeps only the newest part.

use super::actions::{Action, Actions};
use super::glass;
use super::util::*;
use objc2::rc::Retained;
use objc2::sel;
use objc2_app_kit::*;
use objc2_foundation::*;
use std::cell::Cell;

/// Trim once the view holds this many UTF-16 units, down to `KEEP_CHARS`.
const MAX_CHARS: usize = 600_000;
const KEEP_CHARS: usize = 400_000;

pub struct LogPanel {
    pub view: Retained<NSView>,
    pub text: Retained<NSTextView>,
    title: Retained<NSTextField>,
    follow: Retained<NSButton>,
    following: Cell<bool>,
}

impl LogPanel {
    pub fn new(mtm: MainThreadMarker, actions: &Actions) -> Self {
        let view = NSView::new(mtm);
        let (scroll, text) = console(mtm);
        text.setTextContainerInset(NSSize::new(8.0, 40.0));
        view.addSubview(&scroll);
        pin(&scroll, &view);

        let title = label(mtm, "Logs", 12.0, unsafe { NSFontWeightMedium });
        let follow = tool_button(mtm, actions, "arrow.down.to.line", "Follow new output", Action::LogsFollow);
        follow.setButtonType(NSButtonType::PushOnPushOff);
        follow.setState(NSControlStateValueOn);
        let clear = tool_button(mtm, actions, "eraser", "Clear", Action::LogsClear);
        let close = tool_button(mtm, actions, "xmark", "Close", Action::LogsClose);

        let row = hstack(mtm, 6.0, &[&title, &follow, &clear, &close]);
        row.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 14.0, bottom: 0.0, right: 8.0 });
        let capsule = glass::capsule(mtm, &row, 15.0, true);
        let container = glass::container(mtm, &capsule, 0.0);
        view.addSubview(&container);
        container.setTranslatesAutoresizingMaskIntoConstraints(false);
        activate(&[
            container.topAnchor().constraintEqualToAnchor_constant(&view.topAnchor(), 8.0),
            container.trailingAnchor().constraintEqualToAnchor_constant(&view.trailingAnchor(), -12.0),
            capsule.heightAnchor().constraintEqualToConstant(30.0),
            view.heightAnchor().constraintGreaterThanOrEqualToConstant(120.0),
        ]);

        Self { view, text, title, follow, following: Cell::new(true) }
    }

    pub fn begin(&self, title: &str) {
        self.title.setStringValue(&ns(title));
        clear_text(&self.text);
    }

    pub fn clear(&self) {
        clear_text(&self.text);
    }

    pub fn toggle_follow(&self) {
        self.following.set(!self.following.get());
        let state = if self.following.get() { NSControlStateValueOn } else { NSControlStateValueOff };
        self.follow.setState(state);
        if self.following.get() {
            scroll_to_end(&self.text);
        }
    }

    pub fn append(&self, raw: &[u8]) {
        let cleaned = strip_ansi(raw);
        if cleaned.is_empty() {
            return;
        }
        append_text(&self.text, &cleaned);
        self.trim();
        if self.following.get() {
            scroll_to_end(&self.text);
        }
    }

    fn trim(&self) {
        let Some(storage) = (unsafe { self.text.textStorage() }) else { return };
        let length = storage.length();
        if length > MAX_CHARS {
            storage.deleteCharactersInRange(NSRange::new(0, length - KEEP_CHARS));
        }
    }

    pub fn note(&self, text: &str) {
        append_text(&self.text, text);
    }
}

fn tool_button(
    mtm: MainThreadMarker,
    actions: &Actions,
    icon: &str,
    description: &str,
    action: Action,
) -> Retained<NSButton> {
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&ns(""), Some(actions), Some(sel!(dpPerform:)), mtm)
    };
    button.setImage(symbol(icon, description).as_deref());
    button.setImagePosition(NSCellImagePosition::ImageOnly);
    button.setBordered(false);
    button.setToolTip(Some(&ns(description)));
    button.setTag(action as NSInteger);
    button
}

/// Decodes log bytes and removes ANSI colour and cursor sequences.
pub fn strip_ansi(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for following in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&following) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn colour_codes_and_carriage_returns_go() {
        assert_eq!(strip_ansi(b"\x1b[31;1mred\x1b[0m text\r\n"), "red text\n");
    }

    #[test]
    fn invalid_utf8_does_not_lose_the_line() {
        assert_eq!(strip_ansi(b"ok \xff done"), "ok \u{fffd} done");
    }
}
