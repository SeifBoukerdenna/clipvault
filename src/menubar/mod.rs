//! macOS menu bar app: a status item whose menu is your clipboard history.
//!
//! Ownership is deliberately split. `muda` builds and owns the `NSMenu`,
//! including the target/action wiring that turns a click into a `MenuEvent` on
//! a global channel — that saves defining an Objective-C delegate class. We own
//! the `NSStatusItem` itself, which is the piece `muda`'s tray abstraction
//! doesn't expose, and is exactly what the global hotkey needs in order to pop
//! the menu open programmatically. We also keep the `NSMenu` handle so rows can
//! be given app icons, which `muda`'s `MenuItem` has no API for.
//!
//! Everything runs on the main thread, driven by a repeating `NSTimer`, so no
//! AppKit object is ever touched from another thread.
//!
//! [`App`] holds the whole of that state. The two windows it can open are big
//! enough to live next door: the search palette in [`palette`], the preferences
//! sheet in [`prefs`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::rc::Rc;

use arboard::Clipboard;
use block2::StackBlock;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
// `muda` and `global-hotkey` re-export these from the same `keyboard-types`
// version, so one import covers both the menu accelerators and the hotkey.
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use objc2::rc::Retained;
use objc2::{ClassType, MainThreadMarker, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSControlStateValueOff,
    NSControlStateValueOn, NSImage, NSMenu, NSMenuItem, NSPanel, NSStatusBar, NSStatusItem,
    NSTextAlignment, NSVariableStatusItemLength,
};
use objc2_foundation::{NSString, NSTimer};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::config::Config;
use crate::history::HistoryEntry;
use crate::{Result, config, display, history, hotkey, lock, pins, poll, source};

mod icons;
mod palette;
mod prefs;

use icons::set_icon;
use palette::PaletteView;

/// Captures between prunes. Pruning rewrites the whole file, so it shouldn't
/// ride along on every copy.
const PRUNE_EVERY_CAPTURES: u32 = 25;

/// Preview width inside the menu — narrower than the terminal's 80, since a
/// menu that stretches across the screen is unusable.
const MENU_PREVIEW_WIDTH: usize = 60;

/// Timer period, and the ceiling on how long a hotkey press or menu click waits
/// to be noticed. At the old 50ms this was the dominant source of lag — the work
/// per tick is two channel polls, so the display refresh rate is a much better
/// trade than the wakeup saving.
const TICK_SECONDS: f64 = 0.016;
const TICK_MS: u64 = 16;

pub fn run() -> Result<()> {
    let mtm = MainThreadMarker::new().ok_or("the menu bar app must run on the main thread")?;

    // Two pollers on one history file means every copy recorded twice, so a
    // second instance bows out instead of quietly duplicating everything.
    let Some(instance) = lock::acquire()? else {
        already_running(mtm);
        return Ok(());
    };

    let settings = config::load();

    let app = NSApplication::sharedApplication(mtm);
    // Accessory = lives in the menu bar with no Dock icon and no ⌘-Tab entry.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    set_icon(&status_item, mtm);

    let menu = Menu::new();

    // Retained rather than held as a raw pointer: we keep this handle for the
    // life of the app to set per-row icons, so it should own a reference.
    // SAFETY: `ns_menu()` returns a valid, live NSMenu owned by `menu`.
    let ns_menu: Retained<NSMenu> = unsafe { Retained::retain(menu.ns_menu().cast()) }
        .ok_or("muda did not hand back an NSMenu")?;
    status_item.setMenu(Some(&ns_menu));

    // Registered for the process lifetime; dropping the manager unregisters it,
    // so `App` holds onto it.
    let hotkeys = GlobalHotKeyManager::new()?;
    let hotkey = hotkey::parse(&settings.hotkey);
    hotkeys.register(hotkey)?;

    let mut state = App::new(
        mtm,
        status_item,
        menu,
        ns_menu,
        hotkeys,
        hotkey,
        settings,
        instance,
    )?;
    state.build_footer()?;
    // Trim anything left over from a previous run before the first menu build.
    state.prune_now();
    state.rebuild()?;

    let state = Rc::new(RefCell::new(state));

    let block = StackBlock::new(move |_: NonNull<NSTimer>| {
        // The search prompt is modal, and a modal session runs a nested run
        // loop. Should that loop ever pump this timer, re-entering `tick` would
        // panic on the already-held borrow — so skip the tick instead.
        if let Ok(mut state) = state.try_borrow_mut() {
            state.tick();
        }
    })
    .copy();

    // SAFETY: the timer is scheduled on the main run loop from the main thread,
    // so the block only ever runs there — which is what makes it sound for it to
    // capture the non-Send AppKit handles inside `App`.
    let _timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(TICK_SECONDS, true, &block)
    };

    app.run();
    Ok(())
}

fn already_running(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.activate();

    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("ClipVault is already running"));
    alert.setInformativeText(&NSString::from_str(
        "Look for the clipboard icon in the menu bar. Only one copy can watch \
         the clipboard at a time.",
    ));
    alert.runModal();
}

/// Whether this macOS draws menu item subtitles (14.4+). Checked once against
/// the class rather than per item.
fn supports_subtitles() -> bool {
    NSMenuItem::class()
        .instance_method(sel!(setSubtitle:))
        .is_some()
}

/// ⌘1–⌘9 for the first nine rows and nothing beyond — the same place Safari
/// stops numbering tabs. AppKit renders these right-aligned in the menu, which
/// is the detail that makes it read as a system menu rather than a list of text.
fn digit_accelerator(position: usize, extra_shift: bool) -> Option<Accelerator> {
    let code = match position {
        0 => Code::Digit1,
        1 => Code::Digit2,
        2 => Code::Digit3,
        3 => Code::Digit4,
        4 => Code::Digit5,
        5 => Code::Digit6,
        6 => Code::Digit7,
        7 => Code::Digit8,
        8 => Code::Digit9,
        _ => return None,
    };

    let mods = if extra_shift {
        Modifiers::META | Modifiers::SHIFT
    } else {
        Modifiers::META
    };

    Some(Accelerator::new(Some(mods), code))
}

/// One item in the menu's dynamic top section.
///
/// Held rather than dropped after insertion so each item's click target stays
/// alive, and counted so the next rebuild knows how many rows to remove.
enum TopItem {
    /// Clicking copies the carried content back to the clipboard.
    Row(MenuItem, String),
    // The next two carry their item purely to keep it alive; nothing reads them
    // back, which is exactly what the retention is for.
    /// A disabled section header.
    #[allow(dead_code)]
    Label(MenuItem),
    #[allow(dead_code)]
    Separator(PredefinedMenuItem),
}

/// `NSTextAlignment`'s Center and Right values swap by ABI: arm64 macOS uses the
/// iOS ordering (Center = 1), x86_64 macOS uses the older AppKit one
/// (Right = 1). `objc2` omits both constants for exactly this reason, so we pick
/// per architecture — this binary is universal, and hardcoding either value
/// silently right-aligns centred text on the other slice.
#[cfg(target_arch = "aarch64")]
const TEXT_CENTER: NSTextAlignment = NSTextAlignment(1);
#[cfg(target_arch = "aarch64")]
const TEXT_RIGHT: NSTextAlignment = NSTextAlignment(2);
#[cfg(not(target_arch = "aarch64"))]
const TEXT_CENTER: NSTextAlignment = NSTextAlignment(2);
#[cfg(not(target_arch = "aarch64"))]
const TEXT_RIGHT: NSTextAlignment = NSTextAlignment(1);

struct App {
    mtm: MainThreadMarker,
    /// Also keeps the icon in the menu bar: dropping the status item removes it.
    _status_item: Retained<NSStatusItem>,
    menu: Menu,
    ns_menu: Retained<NSMenu>,

    clipboard: Clipboard,
    /// Clipboard text as of the last poll, for change detection.
    last: Option<String>,
    ticks_to_poll: u32,

    top: Vec<TopItem>,
    /// App icons keyed by bundle id. `None` marks a lookup that already failed,
    /// so a missing app isn't re-resolved on every rebuild.
    icons: HashMap<String, Option<Retained<NSImage>>>,
    /// Built once and reused. Constructing the panel's ~30 views measured 18ms,
    /// which was the largest single cost of opening the palette.
    palette: Option<(Retained<NSPanel>, Rc<PaletteView>)>,

    settings: Config,
    hotkey: HotKey,
    supports_subtitles: bool,
    captures_since_prune: u32,
    poll_every_ticks: u32,

    search_item: MenuItem,
    pin_item: MenuItem,
    delete_item: MenuItem,
    prefs_item: MenuItem,
    login_item: CheckMenuItem,
    clear_item: MenuItem,
    quit_item: MenuItem,
    /// Footer separators, retained for the same reason as `top`.
    _footer_separators: Vec<PredefinedMenuItem>,

    hotkeys: GlobalHotKeyManager,
    /// Held only so the single-instance lock lives as long as the app.
    _instance: lock::InstanceLock,
}

/// The dimmed second line on a row: where it came from and how old it is.
fn metadata_line(entry: &HistoryEntry) -> String {
    let age = display::relative_time(&entry.timestamp);
    match entry.source.as_deref() {
        Some(app) => format!("{app} · {age}"),
        None => age,
    }
}

fn login_enabled() -> bool {
    unsafe { SMAppService::mainAppService().status() }.0 == SMAppServiceStatus::Enabled.0
}

impl App {
    #[allow(clippy::too_many_arguments)]
    fn new(
        mtm: MainThreadMarker,
        status_item: Retained<NSStatusItem>,
        menu: Menu,
        ns_menu: Retained<NSMenu>,
        hotkeys: GlobalHotKeyManager,
        hotkey: HotKey,
        settings: Config,
        instance: lock::InstanceLock,
    ) -> Result<Self> {
        let mut clipboard = Clipboard::new()?;
        // Seed from the current clipboard so launching the app doesn't re-record
        // whatever happened to be copied beforehand.
        let last = poll::fetch_clipboard(&mut clipboard).ok();

        let poll_every_ticks = (settings.poll_interval_ms / TICK_MS).max(1) as u32;

        Ok(Self {
            mtm,
            _status_item: status_item,
            menu,
            ns_menu,
            clipboard,
            last,
            ticks_to_poll: poll_every_ticks,
            top: Vec::new(),
            icons: HashMap::new(),
            palette: None,
            settings,
            hotkey,
            supports_subtitles: supports_subtitles(),
            captures_since_prune: 0,
            poll_every_ticks,
            search_item: MenuItem::new(
                "Search History…",
                true,
                Some(Accelerator::new(Some(Modifiers::META), Code::KeyF)),
            ),
            pin_item: MenuItem::new(
                "Pin Current Clipboard",
                true,
                Some(Accelerator::new(Some(Modifiers::META), Code::KeyP)),
            ),
            // ⌘⌫ deletes one thing; clearing everything is deliberately a
            // bigger gesture so the two can't be confused mid-reflex.
            delete_item: MenuItem::new(
                "Delete Current Entry",
                true,
                Some(Accelerator::new(Some(Modifiers::META), Code::Backspace)),
            ),
            prefs_item: MenuItem::new(
                "Preferences…",
                true,
                Some(Accelerator::new(Some(Modifiers::META), Code::Comma)),
            ),
            login_item: CheckMenuItem::new("Launch at Login", true, login_enabled(), None),
            // ⌘⌫ to clear and ⌘Q to quit are the system-wide idioms for
            // "delete this" and "quit", so they need no explaining.
            clear_item: MenuItem::new(
                "Clear History",
                true,
                Some(Accelerator::new(
                    Some(Modifiers::META | Modifiers::SHIFT),
                    Code::Backspace,
                )),
            ),
            quit_item: MenuItem::new(
                "Quit ClipVault",
                true,
                Some(Accelerator::new(Some(Modifiers::META), Code::KeyQ)),
            ),
            _footer_separators: Vec::new(),
            hotkeys,
            _instance: instance,
        })
    }

    /// Appends the fixed commands. Built once: the dynamic rows are inserted
    /// above them, so the footer never has to be rebuilt.
    fn build_footer(&mut self) -> Result<()> {
        let mut separators = Vec::new();

        let top_separator = PredefinedMenuItem::separator();
        self.menu.append(&top_separator)?;
        separators.push(top_separator);

        self.menu.append(&self.search_item)?;
        self.menu.append(&self.pin_item)?;
        self.menu.append(&self.delete_item)?;

        let mid_separator = PredefinedMenuItem::separator();
        self.menu.append(&mid_separator)?;
        separators.push(mid_separator);

        self.menu.append(&self.prefs_item)?;
        self.menu.append(&self.login_item)?;
        self.menu.append(&self.clear_item)?;
        self.menu.append(&self.quit_item)?;

        self._footer_separators = separators;
        Ok(())
    }

    /// Rebuilds the dynamic section: pinned entries and recent history, or
    /// search results when a query is active.
    fn rebuild(&mut self) -> Result<()> {
        // The dynamic section always occupies the top of the menu, so dropping
        // the previous batch is just removing that many items from position 0.
        for _ in 0..self.top.len() {
            self.menu.remove_at(0);
        }
        self.top.clear();

        // The menu shows a fixed handful, so read only that many from the end
        // instead of parsing the whole log on every capture. Searching is the
        // palette's job now, and it reads the full history only when opened.
        let entries = history::read_tail(self.settings.menu_entries)?;
        self.build_recent_rows(&entries)?;

        self.refresh_pin_item();
        Ok(())
    }

    fn build_recent_rows(&mut self, entries: &[HistoryEntry]) -> Result<()> {
        let pinned = pins::read_pins()?;

        if !pinned.is_empty() {
            self.push_label("Pinned")?;
            for (position, pin) in pinned.iter().enumerate() {
                // The pin carries its own source, so this no longer costs a
                // scan of the history per pinned row.
                self.push_row(
                    &pin.content,
                    pin.source_bundle_id.as_deref(),
                    pin.source.clone(),
                    digit_accelerator(position, true),
                )?;
            }
            self.push_separator()?;
        }

        if entries.is_empty() {
            self.push_label("No clipboard history yet")?;
            return Ok(());
        }

        self.push_label("Recent")?;
        for (position, entry) in entries.iter().rev().enumerate() {
            self.push_row(
                &entry.content,
                entry.source_bundle_id.as_deref(),
                Some(metadata_line(entry)),
                digit_accelerator(position, false),
            )?;
        }

        Ok(())
    }

    /// Inserts at the boundary between the dynamic section and the footer.
    fn insert_at_end_of_top(&self, item: &dyn muda::IsMenuItem) -> Result<()> {
        self.menu.insert(item, self.top.len())?;
        Ok(())
    }

    fn push_label(&mut self, text: &str) -> Result<()> {
        let item = MenuItem::new(text, false, None);
        self.insert_at_end_of_top(&item)?;
        self.top.push(TopItem::Label(item));
        Ok(())
    }

    fn push_separator(&mut self) -> Result<()> {
        let separator = PredefinedMenuItem::separator();
        self.insert_at_end_of_top(&separator)?;
        self.top.push(TopItem::Separator(separator));
        Ok(())
    }

    fn push_row(
        &mut self,
        content: &str,
        bundle_id: Option<&str>,
        metadata: Option<String>,
        accelerator: Option<Accelerator>,
    ) -> Result<()> {
        let mut label = display::preview(content, MENU_PREVIEW_WIDTH);

        // Without subtitle support the same information has to ride in the
        // title, which is worse-looking but better than losing it.
        if let Some(metadata) = metadata.as_deref()
            && !self.supports_subtitles
        {
            label = format!("{label}  ·  {metadata}");
        }

        let item = MenuItem::new(&label, true, accelerator);

        let index = self.top.len();
        self.insert_at_end_of_top(&item)?;
        let is_current = self.last.as_deref() == Some(content);
        self.top.push(TopItem::Row(item, content.to_string()));

        if let Some(bundle_id) = bundle_id {
            self.set_row_icon(index, bundle_id);
        }

        let Some(ns_item) = self.ns_menu.itemAtIndex(index as isize) else {
            return Ok(());
        };

        if let Some(metadata) = metadata
            && self.supports_subtitles
        {
            ns_item.setSubtitle(Some(&NSString::from_str(&metadata)));
        }

        // A checkmark is how macOS marks "this is the current one" everywhere
        // else, so it needs no legend.
        ns_item.setState(if is_current {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });

        Ok(())
    }

    /// The pin command reads as pin or unpin depending on what's on the
    /// clipboard right now, which avoids needing a control on every row.
    fn refresh_pin_item(&self) {
        let Some(current) = self.last.as_deref() else {
            self.pin_item.set_enabled(false);
            return;
        };

        match pins::is_pinned(current) {
            Ok(true) => {
                self.pin_item.set_text("Unpin Current Clipboard");
                self.pin_item.set_enabled(true);
            }
            Ok(false) => {
                self.pin_item.set_text("Pin Current Clipboard");
                self.pin_item.set_enabled(true);
            }
            Err(e) => {
                eprintln!("clipvault: could not read pins: {e}");
                self.pin_item.set_enabled(false);
            }
        }
    }

    fn tick(&mut self) {
        self.drain_menu_events();
        self.drain_hotkey_events();

        self.ticks_to_poll -= 1;
        if self.ticks_to_poll == 0 {
            self.ticks_to_poll = self.poll_every_ticks;
            self.poll_clipboard();
        }
    }

    /// Same four gates as the CLI watcher in `watch.rs`.
    fn poll_clipboard(&mut self) {
        let text = match poll::fetch_clipboard(&mut self.clipboard) {
            Ok(text) => text,
            // Non-text clipboard content, or a transient read failure.
            Err(_) => return,
        };

        if text.trim().is_empty() {
            return;
        }
        if self.last.as_deref() == Some(text.as_str()) {
            return;
        }

        self.last = Some(text.clone());

        // Passwords and other secrets are flagged on the pasteboard itself.
        // `last` is already updated, so this isn't re-examined every poll.
        if poll::is_concealed() {
            return;
        }

        match history::append_history(&text, source::frontmost().as_ref()) {
            Ok(()) => {
                // Pruning rewrites the whole file, so it happens on a cadence
                // rather than on every single copy.
                self.captures_since_prune += 1;
                if self.captures_since_prune >= PRUNE_EVERY_CAPTURES {
                    self.prune_now();
                }
                if let Err(e) = self.rebuild() {
                    eprintln!("clipvault: could not refresh the menu: {e}");
                }
            }
            Err(e) => eprintln!("clipvault: could not record entry: {e}"),
        }
    }

    fn drain_menu_events(&mut self) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == *self.quit_item.id() {
                // Every capture is already durable on disk, so there's nothing
                // to flush on the way out.
                std::process::exit(0);
            }

            if event.id == *self.clear_item.id() {
                if let Err(e) = history::clear_history() {
                    eprintln!("clipvault: could not clear history: {e}");
                }
                // `last` deliberately stays put: clearing shouldn't cause the
                // next poll to immediately re-record the current clipboard.
                self.refresh();
                continue;
            }

            if event.id == *self.login_item.id() {
                self.toggle_login();
                continue;
            }

            if event.id == *self.search_item.id() {
                if let Some(content) = self.search_palette() {
                    self.take(content);
                }
                continue;
            }

            if event.id == *self.pin_item.id() {
                self.toggle_pin_for_current();
                self.refresh();
                continue;
            }

            if event.id == *self.delete_item.id() {
                self.delete_current();
                continue;
            }

            if event.id == *self.prefs_item.id() {
                self.edit_preferences();
                continue;
            }

            self.handle_top_click(&event.id);
        }
    }

    fn handle_top_click(&mut self, id: &muda::MenuId) {
        let picked = self.top.iter().find_map(|item| match item {
            TopItem::Row(menu_item, content) if menu_item.id() == id => Some(content.clone()),
            _ => None,
        });

        if let Some(content) = picked {
            self.take(content);
        }
    }

    /// Puts `content` on the clipboard as the result of a deliberate pick.
    fn take(&mut self, content: String) {
        match poll::set_clipboard(&content) {
            // We put this on the clipboard ourselves — remember it so the next
            // poll doesn't log it back as a brand new capture.
            Ok(()) => self.last = Some(content),
            Err(e) => eprintln!("clipvault: could not set the clipboard: {e}"),
        }
        // The pin command's label tracks the clipboard, so it's stale now.
        self.refresh();
    }

    /// Pins whatever is on the clipboard, carrying over its recorded source so
    /// the pinned row can show the same icon and app name.
    fn toggle_pin_for_current(&mut self) {
        let Some(current) = self.last.clone() else {
            return;
        };

        // The whole history, not just the menu's window: the palette can reach
        // an entry from months ago, and pinning it there would otherwise lose
        // the app name and icon.
        let entry = history::read_history()
            .unwrap_or_default()
            .into_iter()
            .rev()
            .find(|e| e.content == current);

        let (name, bundle) = match &entry {
            Some(e) => (e.source.clone(), e.source_bundle_id.clone()),
            None => (None, None),
        };

        if let Err(e) = pins::toggle_pin(&current, name.as_deref(), bundle.as_deref()) {
            eprintln!("clipvault: could not update pins: {e}");
        }
    }

    /// Removes the entry currently on the clipboard, then moves to the next one
    /// so you're never left holding something you just deleted.
    fn delete_current(&mut self) {
        let Some(current) = self.last.clone() else {
            return;
        };

        if let Err(e) = history::delete_entry(&current) {
            eprintln!("clipvault: could not delete entry: {e}");
            return;
        }
        // A pin pointing at a deleted entry would keep offering it back.
        if let Err(e) = pins::remove_pin(&current) {
            eprintln!("clipvault: could not update pins: {e}");
        }

        // Promote the next newest onto the clipboard.
        match history::read_tail(1) {
            Ok(entries) => match entries.into_iter().next_back() {
                Some(next) => match poll::set_clipboard(&next.content) {
                    Ok(()) => self.last = Some(next.content),
                    Err(e) => eprintln!("clipvault: could not set the clipboard: {e}"),
                },
                // Nothing left to promote. Leave the clipboard as the system has
                // it, but stop claiming the deleted text is current.
                None => self.last = None,
            },
            Err(e) => eprintln!("clipvault: could not read history: {e}"),
        }

        self.refresh();
    }

    fn prune_now(&mut self) {
        self.captures_since_prune = 0;
        match history::prune(self.settings.history_limit) {
            Ok(0) => {}
            Ok(n) => eprintln!("clipvault: pruned {n} old entries"),
            Err(e) => eprintln!("clipvault: could not prune history: {e}"),
        }
    }

    fn refresh(&mut self) {
        if let Err(e) = self.rebuild() {
            eprintln!("clipvault: could not refresh the menu: {e}");
        }
    }

    fn toggle_login(&self) {
        let service = unsafe { SMAppService::mainAppService() };
        let currently_on = unsafe { service.status() }.0 == SMAppServiceStatus::Enabled.0;

        let result = if currently_on {
            unsafe { service.unregisterAndReturnError() }
        } else {
            unsafe { service.registerAndReturnError() }
        };

        match result {
            Ok(()) => self.login_item.set_checked(!currently_on),
            Err(e) => {
                // Most often: the app isn't in /Applications, or macOS wants the
                // user to approve it in Login Items first.
                eprintln!("clipvault: could not change the login item: {e:?}");
                // Snap the checkbox back to reality rather than lying about it.
                self.login_item.set_checked(login_enabled());
            }
        }
    }

    fn drain_hotkey_events(&mut self) {
        // Drain first, open once. Holding the shortcut can queue several presses,
        // and each would otherwise open the palette again on the way out.
        let mut pressed = false;
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            // Each press also delivers a matching release.
            if event.state == HotKeyState::Pressed {
                pressed = true;
            }
        }

        if !pressed {
            return;
        }

        // Which surface the shortcut opens is a preference, not a decision to
        // make for the user: going through the menu costs a second keystroke
        // (⌘F) before you can type, but the menu is also where pinning,
        // deleting and preferences live.
        if self.settings.hotkey_opens == config::OPENS_SEARCH {
            if let Some(content) = self.search_palette() {
                self.take(content);
            }
        } else if let Some(button) = self._status_item.button(self.mtm) {
            // SAFETY: main thread, where AppKit requires all UI work. Clicking
            // the button is what makes the status item drop its menu.
            unsafe { button.performClick(None) };
        }

        // The palette runs a modal loop, so presses landing while it is open
        // queue up behind it. Without discarding them, hitting the shortcut
        // again to dismiss the palette would immediately reopen it.
        while GlobalHotKeyEvent::receiver().try_recv().is_ok() {}
    }
}
