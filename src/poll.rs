//! Reads the system clipboard's text contents.

use arboard::Clipboard;

use crate::Result;

/// Returns the clipboard's current text.
///
/// Errors when the clipboard holds something that isn't text (an image, a file
/// reference) or is momentarily locked by another process — callers in the
/// watch loop treat that as "nothing to do this tick" rather than fatal.
pub fn fetch_clipboard(cp: &mut Clipboard) -> Result<String> {
    Ok(cp.get_text()?)
}
