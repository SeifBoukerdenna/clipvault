//! Which app was frontmost when an entry was captured.
//!
//! Recorded at capture time because it can't be recovered later: by the time
//! you open the menu, the app you copied from may be long gone.

/// The app a clipboard entry came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    /// Display name, e.g. "Safari".
    pub name: Option<String>,
    /// Bundle id, e.g. "com.apple.Safari" — used to look up the app's icon.
    pub bundle_id: Option<String>,
}

impl Source {
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.bundle_id.is_none()
    }
}

/// The frontmost app right now, or `None` if it can't be determined.
///
/// This is a best-effort attribution: the capture happens up to one poll
/// interval after the copy, so switching apps immediately after ⌘C can credit
/// the wrong one. Being occasionally wrong is worth more than being absent.
#[cfg(target_os = "macos")]
pub fn frontmost() -> Option<Source> {
    use objc2_app_kit::NSWorkspace;

    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;

    let source = Source {
        name: app.localizedName().map(|s| s.to_string()),
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
    };

    (!source.is_empty()).then_some(source)
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost() -> Option<Source> {
    None
}
