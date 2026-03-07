use arboard::Clipboard;

fn main() {
    let mut clipboard = match Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(e) => {
            eprintln!("clipvault: could not open the clipboard: {e}");
            return;
        }
    };

    match clipboard.get_text() {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("clipvault: {e}"),
    }
}
