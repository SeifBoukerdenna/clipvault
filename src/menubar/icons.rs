//! The menu bar glyph, and the app icons drawn beside history rows.
//!
//! Kept together because they are the same problem twice: ask macOS for an
//! image that may not exist, and degrade quietly when it doesn't.

// `muda` and `global-hotkey` re-export these from the same `keyboard-types`
// version, so one import covers both the menu accelerators and the hotkey.
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSImage, NSStatusItem, NSWorkspace};
use objc2_foundation::{NSSize, NSString};

use super::App;

/// The SF Symbol drawn in the menu bar, with a fallback for older macOS.
const ICON_SYMBOL: &str = "list.clipboard";
const ICON_FALLBACK: &str = "📋";

/// Hover text on the menu bar icon. Modifiers are written in Apple's canonical
/// order (⌃⌥⇧⌘), which is what makes it read like a system tooltip.
const TOOLTIP: &str = "ClipVault — ⇧⌘V";

/// Menu row icons are sized to match the system's menu item metrics.
const ROW_ICON_POINTS: f64 = 16.0;

/// A template image tracks the menu bar's light/dark appearance automatically.
pub(super) fn set_icon(status_item: &NSStatusItem, mtm: MainThreadMarker) {
    let Some(button) = status_item.button(mtm) else {
        return;
    };

    button.setToolTip(Some(&NSString::from_str(TOOLTIP)));

    match NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(ICON_SYMBOL),
        Some(&NSString::from_str("ClipVault")),
    ) {
        Some(image) => {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
        // macOS without that symbol in its catalog: draw a glyph instead.
        None => button.setTitle(&NSString::from_str(ICON_FALLBACK)),
    }
}

/// A template SF Symbol, or `None` on a macOS whose catalog lacks it.
pub(super) fn symbol(name: &str, description: &str) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(description)),
    )?;
    image.setTemplate(true);
    Some(image)
}

/// Resolves an installed app's icon, scaled to menu row size.
pub(super) fn app_icon(bundle_id: &str) -> Option<Retained<NSImage>> {
    let workspace = NSWorkspace::sharedWorkspace();
    let url = workspace.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
    let path = url.path()?;

    let icon = workspace.iconForFile(&path);
    icon.setSize(NSSize::new(ROW_ICON_POINTS, ROW_ICON_POINTS));
    Some(icon)
}

impl App {
    /// Puts the source app's icon on a row. `muda` has no API for this, so it
    /// goes straight through the NSMenu we kept a handle to.
    pub(super) fn set_row_icon(&mut self, index: usize, bundle_id: &str) {
        let icon = self
            .icons
            .entry(bundle_id.to_string())
            .or_insert_with(|| app_icon(bundle_id));

        let Some(icon) = icon else { return };
        let Some(item) = self.ns_menu.itemAtIndex(index as isize) else {
            return;
        };
        item.setImage(Some(icon));
    }
}
