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

    pub fn append(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        append_text(&self.text, text);
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

/// Turns a log stream into clean text on the reading thread, off the main one.
/// Socket reads can end anywhere, so whatever is cut off at the end of a chunk, a
/// multi-byte character or a colour code, waits for the rest to arrive.
#[derive(Default)]
pub struct LogCleaner {
    held: Vec<u8>,
}

impl LogCleaner {
    pub fn feed(&mut self, raw: &[u8]) -> String {
        self.held.extend_from_slice(raw);
        let ready = self.held.len() - unfinished_tail(&self.held);
        let text = strip_ansi(&self.held[..ready]);
        self.held.drain(..ready);
        text
    }

    /// Whatever is left when the stream ends.
    pub fn finish(&mut self) -> String {
        let text = strip_ansi(&self.held);
        self.held.clear();
        text
    }
}

/// How many bytes at the end belong to something not yet complete.
fn unfinished_tail(data: &[u8]) -> usize {
    const LONGEST_ESCAPE: usize = 32;
    let from = data.len().saturating_sub(LONGEST_ESCAPE);
    if let Some(start) = data[from..].iter().rposition(|byte| *byte == 0x1b).map(|i| from + i) {
        let escape = &data[start..];
        let finished = match escape.get(1) {
            None => false,
            Some(b'[') => escape[2..].iter().any(|byte| (0x40..=0x7e).contains(byte)),
            Some(_) => true,
        };
        if !finished {
            return escape.len();
        }
    }
    for back in 1..=data.len().min(3) {
        let byte = data[data.len() - back];
        if byte & 0xc0 == 0x80 {
            continue; // a continuation byte: keep looking for its start
        }
        let needed = match byte {
            0xf0.. => 4,
            0xe0.. => 3,
            0xc0.. => 2,
            _ => 1,
        };
        return if back < needed { back } else { 0 };
    }
    0
}

/// Decodes log bytes and removes ANSI colour and cursor sequences.
pub fn strip_ansi(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    if !text.contains(['\u{1b}', '\r']) {
        return text.into_owned();
    }
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
    use super::{LogCleaner, strip_ansi};

    #[test]
    fn colour_codes_and_carriage_returns_go() {
        assert_eq!(strip_ansi(b"\x1b[31;1mred\x1b[0m text\r\n"), "red text\n");
    }

    #[test]
    fn splitting_a_stream_anywhere_changes_nothing() {
        let wire = "dòng \u{1b}[32mtiếng Việt\u{1b}[0m ắ ộ ữ 🦖\r\nnext\n".as_bytes();
        let whole = strip_ansi(wire);
        assert!(!whole.contains('\u{fffd}') && !whole.contains('\u{1b}'));
        for at in 0..=wire.len() {
            let mut cleaner = LogCleaner::default();
            let mut out = cleaner.feed(&wire[..at]);
            out.push_str(&cleaner.feed(&wire[at..]));
            out.push_str(&cleaner.finish());
            assert_eq!(out, whole, "split at byte {at}");
        }
        let mut cleaner = LogCleaner::default();
        let mut one_by_one: String = wire.iter().map(|byte| cleaner.feed(&[*byte])).collect();
        one_by_one.push_str(&cleaner.finish());
        assert_eq!(one_by_one, whole);
    }

    #[test]
    fn a_stream_cut_mid_character_still_ends_cleanly() {
        let mut cleaner = LogCleaner::default();
        assert_eq!(cleaner.feed(&"ắ".as_bytes()[..2]), "");
        assert_eq!(cleaner.finish(), "\u{fffd}");
        assert_eq!(cleaner.finish(), "");
    }

    #[test]
    fn invalid_utf8_does_not_lose_the_line() {
        assert_eq!(strip_ansi(b"ok \xff done"), "ok \u{fffd} done");
    }
}
