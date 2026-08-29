//! The preferences window.
//!
//! Modal and hand-laid-out in code rather than a nib, which is why the geometry
//! constants are as fussy as they are. Nothing here is applied until Save: the
//! sheet edits a copy of the [`Config`] and hands it back.

use std::ptr::NonNull;

use block2::StackBlock;
// `muda` and `global-hotkey` re-export these from the same `keyboard-types`
// version, so one import covers both the menu accelerators and the hotkey.
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSAlert, NSApplication, NSBackingStoreType, NSBezelStyle, NSButton, NSColor, NSEvent,
    NSEventMask, NSEventType, NSFont, NSModalResponse, NSPanel, NSPopUpButton, NSTextField, NSView,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::config::Config;
use crate::{config, hotkey};

use super::{App, TEXT_RIGHT, TICK_MS};

/// What the preferences sheet was dismissed with.
enum PrefsOutcome {
    Cancelled,
    Save(Config),
}

/// Modal result codes for the preferences window.
const PREFS_SAVE: NSModalResponse = 1;
const PREFS_CANCEL: NSModalResponse = 0;

/// Whether a window-relative point falls inside a view's frame.
fn point_in(point: NSPoint, frame: NSRect) -> bool {
    point.x >= frame.origin.x
        && point.x <= frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y <= frame.origin.y + frame.size.height
}

impl App {
    /// A modal preferences sheet built from labelled text fields.
    ///
    /// A real preferences window would mean a custom `NSWindowController` and
    /// an Objective-C class for the control actions; an alert with an accessory
    /// view gets the same four settings edited with a fraction of the surface.
    /// Runs the preferences sheet, reopening it after a shortcut is recorded so
    /// the recorder feels like a step inside the sheet rather than a detour.
    pub(super) fn edit_preferences(&mut self) {
        if let PrefsOutcome::Save(updated) = self.show_preferences(self.settings.hotkey.clone()) {
            self.apply_settings(updated);
        }
    }

    /// A real preferences window rather than an alert.
    ///
    /// An `NSAlert` brings an icon, a message, and a vertical stack of buttons,
    /// which is why the old one read as a warning dialog and truncated its own
    /// labels. This lays out a proper two-column form and puts Cancel/Save
    /// where macOS puts them.
    ///
    /// Buttons are hit-tested through the event monitor instead of target/action,
    /// which avoids defining an Objective-C class for three callbacks.
    fn show_preferences(&self, pending_hotkey: String) -> PrefsOutcome {
        let app = NSApplication::sharedApplication(self.mtm);
        app.activate();

        let rows: [(&str, String); 3] = [
            ("Poll interval", self.settings.poll_interval_ms.to_string()),
            ("Entries in menu", self.settings.menu_entries.to_string()),
            ("History limit", self.settings.history_limit.to_string()),
        ];

        const W: f64 = 500.0;
        const PAD: f64 = 22.0;
        const LABEL_W: f64 = 150.0;
        const FIELD_W: f64 = 110.0;
        const ROW_H: f64 = 34.0;
        const CTRL_X: f64 = PAD + LABEL_W + 12.0;
        // shortcut + 3 numeric rows + opens row, then the button bar
        let body = ROW_H * 5.0;
        let height = PAD + 44.0 + body + PAD;

        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, height)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setTitle(&NSString::from_str("ClipVault Preferences"));

        let content = NSView::initWithFrame(
            NSView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, height)),
        );

        let label = |text: &str, y: f64| {
            let l = NSTextField::labelWithString(&NSString::from_str(text), self.mtm);
            l.setFrame(NSRect::new(
                NSPoint::new(PAD, y),
                NSSize::new(LABEL_W, 18.0),
            ));
            // Right-aligned against the controls, the way system forms read.
            l.setAlignment(TEXT_RIGHT);
            l.setTextColor(Some(&NSColor::labelColor()));
            l
        };

        let mut y = height - PAD - 26.0;

        content.addSubview(&label("Shortcut", y + 3.0));
        let shortcut = NSTextField::initWithFrame(
            NSTextField::alloc(self.mtm),
            NSRect::new(NSPoint::new(CTRL_X, y), NSSize::new(FIELD_W + 40.0, 22.0)),
        );
        shortcut.setStringValue(&NSString::from_str(&pending_hotkey));
        content.addSubview(&shortcut);

        // Show what the typed spec resolves to, so a typo is visible before
        // saving rather than after the shortcut quietly stops working.
        let resolved = NSTextField::labelWithString(
            &NSString::from_str(&format!(
                "{}   e.g. cmd+shift+V",
                hotkey::describe(&pending_hotkey)
            )),
            self.mtm,
        );
        resolved.setFrame(NSRect::new(
            NSPoint::new(CTRL_X + FIELD_W + 50.0, y + 3.0),
            NSSize::new(W - CTRL_X - FIELD_W - 50.0 - PAD, 18.0),
        ));
        resolved.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        resolved.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&resolved);

        // Numeric fields.
        let mut fields = Vec::new();
        for (index, (text, value)) in rows.iter().enumerate() {
            y -= ROW_H;
            content.addSubview(&label(text, y + 3.0));

            let field = NSTextField::initWithFrame(
                NSTextField::alloc(self.mtm),
                NSRect::new(NSPoint::new(CTRL_X, y), NSSize::new(FIELD_W, 22.0)),
            );
            field.setStringValue(&NSString::from_str(value));
            content.addSubview(&field);
            fields.push(field);

            // A unit or hint beside the field, so the labels stay short.
            let hint = match index {
                0 => "milliseconds",
                1 => "rows",
                _ => "entries (0 keeps everything)",
            };
            let h = NSTextField::labelWithString(&NSString::from_str(hint), self.mtm);
            h.setFrame(NSRect::new(
                NSPoint::new(CTRL_X + FIELD_W + 10.0, y + 3.0),
                NSSize::new(W - CTRL_X - FIELD_W - 10.0 - PAD, 18.0),
            ));
            h.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            h.setTextColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&h);
        }

        // Popup instead of asking the user to type "menu" or "search".
        y -= ROW_H;
        content.addSubview(&label("Shortcut opens", y + 3.0));
        let opens = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm),
            NSRect::new(NSPoint::new(CTRL_X, y - 2.0), NSSize::new(150.0, 26.0)),
            false,
        );
        opens.addItemWithTitle(&NSString::from_str("Menu"));
        opens.addItemWithTitle(&NSString::from_str("Search"));
        opens.selectItemAtIndex(if self.settings.hotkey_opens == config::OPENS_SEARCH {
            1
        } else {
            0
        });
        content.addSubview(&opens);

        // Cancel then Save, right-aligned — macOS puts the default rightmost.
        let save = NSButton::initWithFrame(
            NSButton::alloc(self.mtm),
            NSRect::new(
                NSPoint::new(W - PAD - 100.0, PAD - 6.0),
                NSSize::new(100.0, 30.0),
            ),
        );
        save.setTitle(&NSString::from_str("Save"));
        save.setBezelStyle(NSBezelStyle::Push);
        save.setKeyEquivalent(&NSString::from_str("\r"));
        content.addSubview(&save);
        let save_frame = save.frame();

        let cancel = NSButton::initWithFrame(
            NSButton::alloc(self.mtm),
            NSRect::new(
                NSPoint::new(W - PAD - 212.0, PAD - 6.0),
                NSSize::new(100.0, 30.0),
            ),
        );
        cancel.setTitle(&NSString::from_str("Cancel"));
        cancel.setBezelStyle(NSBezelStyle::Push);
        content.addSubview(&cancel);
        let cancel_frame = cancel.frame();

        panel.setContentView(Some(&content));
        panel.center();
        if let Some(first) = fields.first() {
            panel.makeFirstResponder(Some(first));
        }

        let monitor = {
            let app = app.clone();
            let block = StackBlock::new(move |event: NonNull<NSEvent>| {
                let raw = event;
                // SAFETY: AppKit hands us a live event for the handler's duration.
                let event = unsafe { raw.as_ref() };

                if event.r#type() == NSEventType::LeftMouseDown {
                    let point = event.locationInWindow();
                    if point_in(point, save_frame) {
                        app.stopModalWithCode(PREFS_SAVE);
                        return std::ptr::null_mut();
                    }
                    if point_in(point, cancel_frame) {
                        app.stopModalWithCode(PREFS_CANCEL);
                        return std::ptr::null_mut();
                    }
                    // Anything else (a text field, the popup) is handled normally.
                    return raw.as_ptr();
                }

                match event.keyCode() {
                    36 | 76 => {
                        app.stopModalWithCode(PREFS_SAVE);
                        std::ptr::null_mut()
                    }
                    53 => {
                        app.stopModalWithCode(PREFS_CANCEL);
                        std::ptr::null_mut()
                    }
                    // Everything else reaches the field being typed into.
                    _ => raw.as_ptr(),
                }
            })
            .copy();

            // SAFETY: removed before this function returns.
            unsafe {
                NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                    NSEventMask::KeyDown | NSEventMask::LeftMouseDown,
                    &block,
                )
            }
        };

        panel.makeKeyAndOrderFront(None);
        let response = app.runModalForWindow(&panel);

        let read = |index: usize| fields[index].stringValue().to_string();
        let defaults = Config::default();
        let edited = Config {
            hotkey: shortcut.stringValue().to_string(),
            // A field typed into nonsense keeps the current value rather than
            // silently snapping to a default the user didn't ask for.
            poll_interval_ms: read(0)
                .trim()
                .parse()
                .unwrap_or(self.settings.poll_interval_ms),
            menu_entries: read(1).trim().parse().unwrap_or(self.settings.menu_entries),
            history_limit: read(2).trim().parse().unwrap_or(defaults.history_limit),
            hotkey_opens: if opens.indexOfSelectedItem() == 1 {
                config::OPENS_SEARCH.to_string()
            } else {
                config::OPENS_MENU.to_string()
            },
        };

        panel.orderOut(None);
        if let Some(monitor) = monitor {
            // SAFETY: the token the matching add call returned.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }

        if response != PREFS_SAVE {
            return PrefsOutcome::Cancelled;
        }

        PrefsOutcome::Save(edited.sanitized())
    }

    fn apply_settings(&mut self, updated: Config) {
        if updated == self.settings {
            return;
        }

        if updated.hotkey != self.settings.hotkey {
            let next = hotkey::parse(&updated.hotkey);
            // Release the old binding first: registering a shortcut the system
            // still has attached to us would just fail.
            if let Err(e) = self.hotkeys.unregister(self.hotkey) {
                eprintln!("clipvault: could not release the old shortcut: {e}");
            }
            match self.hotkeys.register(next) {
                Ok(()) => self.hotkey = next,
                Err(e) => {
                    eprintln!("clipvault: could not register {}: {e}", updated.hotkey);
                    // Put the previous one back so the app doesn't end up with
                    // no way to open at all.
                    let _ = self.hotkeys.register(self.hotkey);

                    // Say so. A shortcut another app already owns would
                    // otherwise appear to save and then quietly not work.
                    let alert = NSAlert::new(self.mtm);
                    alert.setMessageText(&NSString::from_str("Shortcut unavailable"));
                    alert.setInformativeText(&NSString::from_str(&format!(
                        "{} could not be registered — another app is probably using it. Keeping {}.",
                        hotkey::describe(&updated.hotkey),
                        hotkey::describe(&self.settings.hotkey),
                    )));
                    alert.runModal();

                    // Keep the stored setting matching what is actually bound.
                    let mut updated = updated;
                    updated.hotkey = self.settings.hotkey.clone();
                    return self.finish_settings(updated);
                }
            }
        }

        self.finish_settings(updated);
    }

    fn finish_settings(&mut self, updated: Config) {
        self.poll_every_ticks = (updated.poll_interval_ms / TICK_MS).max(1) as u32;
        self.ticks_to_poll = self.poll_every_ticks;
        self.settings = updated;

        if let Err(e) = config::save(&self.settings) {
            eprintln!("clipvault: could not save preferences: {e}");
        }

        self.prune_now();
        self.refresh();
    }
}
