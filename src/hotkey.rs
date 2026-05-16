//! Reading a shortcut out of a key press, and writing one back for display.
//!
//! Shortcuts are stored as text (`"cmd+shift+KeyV"`) so the config file stays
//! hand-editable, but neither end of that is what a user should have to type —
//! they press the keys, and they see `⇧⌘V`.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};

/// Used when nothing is stored, or what's stored can't be parsed.
pub const DEFAULT: &str = "cmd+shift+KeyV";

/// Parses a stored shortcut, falling back to the default.
///
/// A shortcut that won't parse must never leave the app with no way to open, so
/// this can't return an error.
pub fn parse(spec: &str) -> HotKey {
    spec.parse()
        .or_else(|_| DEFAULT.parse())
        // The default is a literal that parses, so this arm is unreachable in
        // practice; it exists so a bad shortcut can't be a panic.
        .unwrap_or(HotKey::new(
            Some(Modifiers::META | Modifiers::SHIFT),
            Code::KeyV,
        ))
}

/// Renders a shortcut the way macOS writes them: modifiers in the canonical
/// ⌃⌥⇧⌘ order, then the key.
pub fn describe(spec: &str) -> String {
    let hotkey = parse(spec);
    let mods = hotkey.mods;

    let mut out = String::new();
    if mods.contains(Modifiers::CONTROL) {
        out.push('⌃');
    }
    if mods.contains(Modifiers::ALT) {
        out.push('⌥');
    }
    if mods.contains(Modifiers::SHIFT) {
        out.push('⇧');
    }
    if mods.intersects(Modifiers::META | Modifiers::SUPER) {
        out.push('⌘');
    }

    out.push_str(&key_glyph(hotkey.key));
    out
}

/// The printed form of the non-modifier key.
fn key_glyph(code: Code) -> String {
    match code {
        Code::Space => "Space".to_string(),
        Code::Enter => "↩".to_string(),
        Code::Tab => "⇥".to_string(),
        Code::Backspace => "⌫".to_string(),
        Code::Delete => "⌦".to_string(),
        Code::Escape => "⎋".to_string(),
        Code::ArrowLeft => "←".to_string(),
        Code::ArrowRight => "→".to_string(),
        Code::ArrowUp => "↑".to_string(),
        Code::ArrowDown => "↓".to_string(),
        // macOS writes these as the symbol on the key, not its name.
        Code::Minus => "-".to_string(),
        Code::Equal => "=".to_string(),
        Code::BracketLeft => "[".to_string(),
        Code::BracketRight => "]".to_string(),
        Code::Semicolon => ";".to_string(),
        Code::Quote => "'".to_string(),
        Code::Comma => ",".to_string(),
        Code::Period => ".".to_string(),
        Code::Slash => "/".to_string(),
        Code::Backslash => "\\".to_string(),
        Code::Backquote => "`".to_string(),
        other => {
            // "KeyV" -> "V", "Digit4" -> "4"; anything else prints as-is.
            let name = other.to_string();
            name.strip_prefix("Key")
                .or_else(|| name.strip_prefix("Digit"))
                .unwrap_or(&name)
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unparseable_stored_shortcut_falls_back_to_the_default() {
        assert_eq!(describe("total+nonsense"), describe(DEFAULT));
    }
}
