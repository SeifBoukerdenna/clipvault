//! Reads and writes the system clipboard's text contents.

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

/// Puts `text` back onto the system clipboard.
///
/// On macOS the pasteboard takes ownership of the data, so it survives this
/// process exiting. (X11 would need the clipboard owner to stay alive.)
pub fn set_clipboard(text: &str) -> Result<()> {
    let mut cp = Clipboard::new()?;
    cp.set_text(text.to_string())?;
    Ok(())
}
