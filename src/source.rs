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

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, bundle: &str) -> Source {
        Source {
            name: Some(name.to_string()),
            bundle_id: Some(bundle.to_string()),
        }
    }

    #[test]
    fn a_source_with_neither_field_is_empty() {
        assert!(Source::default().is_empty());
    }

    #[test]
    fn either_field_alone_is_enough_to_be_useful() {
        // A name with no bundle id still labels the row; a bundle id with no
        // name still finds the icon. Only both missing is worth discarding.
        assert!(
            !Source {
                name: Some("Safari".to_string()),
                bundle_id: None,
            }
            .is_empty()
        );
        assert!(
            !Source {
                name: None,
                bundle_id: Some("com.apple.Safari".to_string()),
            }
            .is_empty()
        );
    }

    #[test]
    fn a_fully_populated_source_is_not_empty() {
        assert!(!named("Safari", "com.apple.Safari").is_empty());
    }

    #[test]
    fn sources_compare_by_value() {
        // The menu dedupes and looks up icons by source, so equality has to be
        // structural rather than identity.
        assert_eq!(
            named("Safari", "com.apple.Safari"),
            named("Safari", "com.apple.Safari")
        );
        assert_ne!(
            named("Safari", "com.apple.Safari"),
            named("Notes", "com.apple.Notes")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn there_is_no_frontmost_app_to_find_off_macos() {
        assert!(frontmost().is_none());
    }
}
