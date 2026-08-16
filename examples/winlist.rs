//! Lists on-screen windows owned by a process, so a screenshot can target one
//! by id instead of capturing the whole desktop.
//!
//! A menu is a separate window that exists only while it is open, and the app
//! is blocked in event tracking at that moment — so it can't report its own
//! menu's id. This runs as a separate process and asks the window server.
//!
//!   cargo run --example winlist -- <pid>
//!
//! Prints `<window-id> <width>x<height>`, largest first.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFString, CFType};
    use objc2_core_graphics::{
        CGWindowListCopyWindowInfo, CGWindowListOption, kCGWindowBounds, kCGWindowNumber,
        kCGWindowOwnerPID,
    };
    use std::ffi::c_void;

    let want_pid: i64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: winlist <pid>");

    let list = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        0,
    )
    .expect("window list unavailable");

    // The dictionaries are untyped, so values come back through the raw getter
    // and are downcast by hand.
    fn number(dict: &CFDictionary, key: &CFString) -> Option<f64> {
        // SAFETY: `key` is a valid CFString and the dictionary is live.
        let raw = unsafe { CFDictionary::value(dict, key as *const CFString as *const c_void) };
        if raw.is_null() {
            return None;
        }
        let value = unsafe { &*(raw as *const CFType) };
        value.downcast_ref::<CFNumber>()?.as_f64()
    }

    /// Reads a nested dictionary. The value is owned by the parent, which
    /// outlives the read, so it is used by reference rather than retained.
    fn bounds_of(dict: &CFDictionary) -> (f64, f64) {
        let key = unsafe { kCGWindowBounds };
        let raw = unsafe { CFDictionary::value(dict, key as *const CFString as *const c_void) };
        if raw.is_null() {
            return (0.0, 0.0);
        }
        let value = unsafe { &*(raw as *const CFType) };
        let Some(bounds) = value.downcast_ref::<CFDictionary>() else {
            return (0.0, 0.0);
        };
        (
            number(bounds, &CFString::from_str("Width")).unwrap_or(0.0),
            number(bounds, &CFString::from_str("Height")).unwrap_or(0.0),
        )
    }

    let count = CFArray::count(&list);
    let mut rows: Vec<(f64, i64, f64, f64)> = Vec::new();

    for index in 0..count {
        let raw = unsafe { CFArray::value_at_index(&list, index) };
        if raw.is_null() {
            continue;
        }
        let entry = unsafe { &*(raw as *const CFType) };
        let Some(dict) = entry.downcast_ref::<CFDictionary>() else {
            continue;
        };

        if number(dict, unsafe { kCGWindowOwnerPID }).unwrap_or(-1.0) as i64 != want_pid {
            continue;
        }

        let id = number(dict, unsafe { kCGWindowNumber }).unwrap_or(0.0) as i64;
        let (w, h) = bounds_of(dict);
        rows.push((w * h, id, w, h));
    }

    // Largest first: the menu dwarfs the status item's button.
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, id, w, h) in rows {
        println!("{id} {}x{}", w as i64, h as i64);
    }
}
