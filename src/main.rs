use arboard::Clipboard;
use clipvault::{Result, poll};

fn main() -> Result<()> {
    let mut clipboard = Clipboard::new()?;
    println!("{}", poll::fetch_clipboard(&mut clipboard)?);
    Ok(())
}
