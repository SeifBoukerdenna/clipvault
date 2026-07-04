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
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSBezelStyle, NSBox, NSBoxType, NSButton, NSColor, NSControlStateValueOff,
    NSControlStateValueOn, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSFloatingWindowLevel, NSFont, NSImage, NSImageView, NSMenu, NSMenuItem, NSModalResponse,
    NSPanel, NSPopUpButton, NSScreen, NSStatusBar, NSStatusItem, NSTextAlignment, NSTextField,
    NSVariableStatusItemLength, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowButton, NSWindowDidResignKeyNotification,
    NSWindowStyleMask, NSWindowTitleVisibility, NSWorkspace,
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSPoint, NSRect, NSSize, NSString, NSTimer,
};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::config::Config;
use crate::history::HistoryEntry;
use crate::{Result, config, display, fuzzy, history, hotkey, lock, pins, poll, source};

/// Result rows the search palette shows at once. Results beyond this scroll.
const PALETTE_ROWS: usize = 8;
const PALETTE_WIDTH: f64 = 680.0;
/// Two lines per row: the content, and where it came from.
const PALETTE_ROW_HEIGHT: f64 = 48.0;
const PALETTE_SEARCH_HEIGHT: f64 = 62.0;
/// Gap between the search field's rule and the first result, so a selected top
/// row doesn't sit flush against what you're typing.
const PALETTE_LIST_GAP: f64 = 8.0;
const PALETTE_FOOTER_HEIGHT: f64 = 30.0;
/// Outer horizontal padding. The search glyph, row icons and footer text all
/// start here, so the palette reads as one column rather than three.
const PALETTE_PADDING: f64 = 16.0;
const PALETTE_ICON: f64 = 22.0;
/// Space between an icon and the text beside it.
const PALETTE_ICON_GAP: f64 = 12.0;
/// How far the selection highlight is inset from the panel edge.
const PALETTE_ROW_INSET: f64 = 10.0;
/// Where every line of text begins, icon column included.
const PALETTE_TEXT_LEFT: f64 = PALETTE_PADDING + PALETTE_ICON + PALETTE_ICON_GAP;
/// Preview width inside the palette, which is wider than the menu.
const PALETTE_PREVIEW_WIDTH: usize = 82;

/// Captures between prunes. Pruning rewrites the whole file, so it shouldn't
/// ride along on every copy.
const PRUNE_EVERY_CAPTURES: u32 = 25;

/// Preview width inside the menu — narrower than the terminal's 80, since a
/// menu that stretches across the screen is unusable.
const MENU_PREVIEW_WIDTH: usize = 60;

/// Timer period, and the ceiling on how long a hotkey press or menu click waits
/// to be noticed.
const TICK_SECONDS: f64 = 0.05;
const TICK_MS: u64 = 50;

/// The SF Symbol drawn in the menu bar, with a fallback for older macOS.
const ICON_SYMBOL: &str = "list.clipboard";
const ICON_FALLBACK: &str = "📋";

/// Hover text on the menu bar icon. Modifiers are written in Apple's canonical
/// order (⌃⌥⇧⌘), which is what makes it read like a system tooltip.
const TOOLTIP: &str = "ClipVault — ⇧⌘V";

/// Menu row icons are sized to match the system's menu item metrics.
const ROW_ICON_POINTS: f64 = 16.0;

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

/// A template image tracks the menu bar's light/dark appearance automatically.
fn set_icon(status_item: &NSStatusItem, mtm: MainThreadMarker) {
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

/// One searchable history entry, flattened once when the palette opens.
struct Candidate {
    content: String,
    preview: String,
    meta: String,
    bundle_id: Option<String>,
}

/// The palette's live state.
struct PaletteState {
    query: String,
    /// Index into `matches`, not into the visible rows — results past the
    /// visible window are reachable, and `offset` scrolls to follow.
    selection: usize,
    offset: usize,
    /// Indices into the candidate list, best match first.
    matches: Vec<usize>,
}

impl PaletteState {
    fn refilter(&mut self, candidates: &[Candidate]) {
        let mut scored: Vec<(i32, usize)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                fuzzy::score(&self.query, &candidate.content).map(|score| (score, index))
            })
            .collect();

        // Best score first; ties keep history order, which is newest first.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

        self.matches = scored.into_iter().map(|(_, index)| index).collect();
        self.selection = 0;
        self.offset = 0;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selection = 0;
            self.offset = 0;
            return;
        }

        // Stops at both ends rather than wrapping. Wrapping from the last
        // result back to the first makes it impossible to tell, while holding a
        // key, whether you are at the bottom or have gone round again.
        let last = self.matches.len() - 1;
        self.selection = if delta < 0 {
            self.selection.saturating_sub(1)
        } else {
            (self.selection + 1).min(last)
        };

        self.scroll_into_view();
    }

    /// Keeps `offset` such that the selection is always on screen, including
    /// when it wraps from the last row back to the first.
    fn scroll_into_view(&mut self) {
        if self.selection < self.offset {
            self.offset = self.selection;
        } else if self.selection >= self.offset + PALETTE_ROWS {
            self.offset = self.selection + 1 - PALETTE_ROWS;
        }
    }

    fn visible(&self) -> &[usize] {
        let end = (self.offset + PALETTE_ROWS).min(self.matches.len());
        &self.matches[self.offset.min(end)..end]
    }
}

/// Scroll distance that advances the selection by one row.
const SCROLL_STEP: f64 = 12.0;
/// Line-scroll wheels report in lines rather than points.
const SCROLL_LINE_HEIGHT: f64 = 10.0;

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

/// Modal result codes for the palette. Any non-zero value works; these are just
/// distinct from `NSModalResponse` values AppKit produces on its own.
const PALETTE_ACCEPT: NSModalResponse = 1;
const PALETTE_CANCEL: NSModalResponse = 0;

/// Puts the palette where macOS puts Spotlight: centred horizontally, and high
/// enough that it reads as an overlay rather than a dialog parked mid-screen.
fn position_palette(panel: &NSPanel) {
    let mtm = MainThreadMarker::from(panel);
    let Some(screen) = panel.screen().or_else(|| NSScreen::mainScreen(mtm)) else {
        panel.center();
        return;
    };

    let visible = screen.visibleFrame();
    let frame = panel.frame();
    let x = visible.origin.x + (visible.size.width - frame.size.width) / 2.0;
    let y = visible.origin.y + visible.size.height * 0.72 - frame.size.height / 2.0;
    panel.setFrameOrigin(NSPoint::new(x, y));
}

/// Total height for a palette showing `rows` results.
fn palette_height(rows: usize) -> f64 {
    PALETTE_SEARCH_HEIGHT
        + PALETTE_LIST_GAP
        + PALETTE_ROW_HEIGHT * rows as f64
        + PALETTE_FOOTER_HEIGHT
}

/// Which result row a window-relative point falls on, if any.
///
/// Window coordinates run bottom-up, so this measures down from where the list
/// starts, below the search field and its gap.
fn row_at(point: NSPoint, height: f64) -> Option<usize> {
    let list_top = height - PALETTE_SEARCH_HEIGHT - PALETTE_LIST_GAP;
    if point.y >= list_top || point.y <= PALETTE_FOOTER_HEIGHT {
        return None;
    }

    let row = ((list_top - point.y) / PALETTE_ROW_HEIGHT).floor();
    let row = row as usize;
    (row < PALETTE_ROWS).then_some(row)
}

/// A template SF Symbol, or `None` on a macOS whose catalog lacks it.
fn symbol(name: &str, description: &str) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(description)),
    )?;
    image.setTemplate(true);
    Some(image)
}

/// A one-pixel divider.
fn separator(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSBox> {
    let rule = NSBox::initWithFrame(NSBox::alloc(mtm), frame);
    rule.setBoxType(NSBoxType::Custom);
    rule.setBorderWidth(0.0);
    rule.setFillColor(&NSColor::separatorColor());
    rule
}

/// One result row: an app icon, the content, and a dimmed second line.
///
/// The selection background is an `NSBox` rather than a text field's background
/// so it can have rounded corners and span the whole row — a square highlight
/// behind just the label is most of what made the first version look unfinished.
struct PaletteRow {
    box_view: Retained<NSBox>,
    icon: Retained<NSImageView>,
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
}

impl PaletteRow {
    fn new(mtm: MainThreadMarker, y: f64) -> Self {
        let inset = PALETTE_ROW_INSET;
        let box_view = NSBox::initWithFrame(
            NSBox::alloc(mtm),
            NSRect::new(
                NSPoint::new(inset, y + 2.0),
                NSSize::new(PALETTE_WIDTH - inset * 2.0, PALETTE_ROW_HEIGHT - 4.0),
            ),
        );
        box_view.setBoxType(NSBoxType::Custom);
        box_view.setBorderWidth(0.0);
        box_view.setCornerRadius(8.0);
        box_view.setFillColor(&NSColor::clearColor());

        let content = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(PALETTE_WIDTH - inset * 2.0, PALETTE_ROW_HEIGHT - 4.0),
            ),
        );

        // Positions are relative to the highlight box, so both shift left by the
        // inset to stay in the same screen column as the search field above.
        let icon = NSImageView::initWithFrame(
            NSImageView::alloc(mtm),
            NSRect::new(
                NSPoint::new(
                    PALETTE_PADDING - inset,
                    (PALETTE_ROW_HEIGHT - 4.0 - PALETTE_ICON) / 2.0,
                ),
                NSSize::new(PALETTE_ICON, PALETTE_ICON),
            ),
        );
        content.addSubview(&icon);

        let left = PALETTE_TEXT_LEFT - inset;
        let width = PALETTE_WIDTH - PALETTE_TEXT_LEFT - PALETTE_PADDING;

        let title = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        title.setFrame(NSRect::new(
            NSPoint::new(left, 22.0),
            NSSize::new(width, 18.0),
        ));
        title.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        content.addSubview(&title);

        let subtitle = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        subtitle.setFrame(NSRect::new(
            NSPoint::new(left, 5.0),
            NSSize::new(width, 15.0),
        ));
        subtitle.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        subtitle.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&subtitle);

        box_view.setContentView(Some(&content));

        Self {
            box_view,
            icon,
            title,
            subtitle,
        }
    }

    fn show(&self, candidate: &Candidate, selected: bool, icon: Option<&Retained<NSImage>>) {
        self.box_view.setHidden(false);
        self.title
            .setStringValue(&NSString::from_str(&candidate.preview));
        self.subtitle
            .setStringValue(&NSString::from_str(&candidate.meta));

        // Entries captured before source tracking, or from an app that has since
        // been removed, would otherwise leave a hole in the icon column.
        match icon {
            Some(icon) => self.icon.setImage(Some(icon)),
            None => self
                .icon
                .setImage(symbol("doc.on.clipboard", "Clipboard").as_deref()),
        }

        if selected {
            self.box_view
                .setFillColor(&NSColor::selectedContentBackgroundColor());
            self.title.setTextColor(Some(&NSColor::selectedTextColor()));
            self.subtitle
                .setTextColor(Some(&NSColor::selectedTextColor()));
        } else {
            self.box_view.setFillColor(&NSColor::clearColor());
            self.title.setTextColor(Some(&NSColor::labelColor()));
            self.subtitle
                .setTextColor(Some(&NSColor::secondaryLabelColor()));
        }
    }

    fn hide(&self) {
        self.box_view.setHidden(true);
    }
}

/// The views the palette draws into, reused across keystrokes rather than
/// rebuilt, so typing doesn't churn the view hierarchy.
struct PaletteView {
    panel: Retained<NSPanel>,
    query_label: Retained<NSTextField>,
    rows: Vec<PaletteRow>,
    empty: Retained<NSTextField>,
    count_label: Retained<NSTextField>,
}

impl PaletteView {
    fn render(
        &self,
        state: &PaletteState,
        candidates: &[Candidate],
        icons: &HashMap<String, Option<Retained<NSImage>>>,
    ) {
        // An empty query shows a prompt rather than a blank line, so the palette
        // never looks like it failed to open.
        if state.query.is_empty() {
            self.query_label
                .setStringValue(&NSString::from_str("Search clipboard history"));
            self.query_label
                .setTextColor(Some(&NSColor::tertiaryLabelColor()));
        } else {
            self.query_label
                .setStringValue(&NSString::from_str(&state.query));
            self.query_label.setTextColor(Some(&NSColor::labelColor()));
        }

        let visible = state.visible();
        for (slot, row) in self.rows.iter().enumerate() {
            match visible.get(slot) {
                Some(index) => {
                    let candidate = &candidates[*index];
                    let icon = candidate
                        .bundle_id
                        .as_ref()
                        .and_then(|id| icons.get(id))
                        .and_then(|icon| icon.as_ref());
                    row.show(candidate, state.offset + slot == state.selection, icon);
                }
                None => row.hide(),
            }
        }

        if state.matches.is_empty() {
            let message = if state.query.is_empty() {
                "No clipboard history yet".to_string()
            } else {
                format!("No matches for “{}”", state.query)
            };
            self.empty.setStringValue(&NSString::from_str(&message));
            self.empty.setHidden(false);
        } else {
            self.empty.setHidden(true);
        }

        // Position within the results, so scrolling past the visible rows is
        // legible rather than silent.
        let count = match state.matches.len() {
            0 => "no results".to_string(),
            1 => "1 result".to_string(),
            n => format!("{} of {} results", state.selection + 1, n),
        };
        self.count_label.setStringValue(&NSString::from_str(&count));

        self.resize_to_fit(visible.len());
    }

    /// Shrinks the panel to the number of rows actually in use.
    ///
    /// Keeping eight rows of empty space when one thing matched reads as broken.
    /// The top edge is pinned so the panel doesn't appear to jump around the
    /// screen as you type.
    fn resize_to_fit(&self, rows_shown: usize) {
        // At least one row: the "no matches" line needs somewhere to sit.
        let needed = palette_height(rows_shown.max(1));

        let frame = self.panel.frame();
        if (frame.size.height - needed).abs() < 0.5 {
            return;
        }

        self.panel.setFrame_display(
            NSRect::new(
                NSPoint::new(frame.origin.x, frame.origin.y + frame.size.height - needed),
                NSSize::new(frame.size.width, needed),
            ),
            true,
        );
    }
}

/// What the preferences sheet was dismissed with.
enum PrefsOutcome {
    Cancelled,
    Save(Config),
}

/// Modal result codes for the preferences window.
const PREFS_SAVE: NSModalResponse = 1;
const PREFS_CANCEL: NSModalResponse = 0;

/// Whether a window-relative point falls inside a view's frame.
fn point_in(point: NSPoint, frame: NSRect) -> bool {
    point.x >= frame.origin.x
        && point.x <= frame.origin.x + frame.size.width
        && point.y >= frame.origin.y
        && point.y <= frame.origin.y + frame.size.height
}

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

    /// Puts the source app's icon on a row. `muda` has no API for this, so it
    /// goes straight through the NSMenu we kept a handle to.
    fn set_row_icon(&mut self, index: usize, bundle_id: &str) {
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

    /// A modal preferences sheet built from labelled text fields.
    ///
    /// A real preferences window would mean a custom `NSWindowController` and
    /// an Objective-C class for the control actions; an alert with an accessory
    /// view gets the same four settings edited with a fraction of the surface.
    /// Runs the preferences sheet, reopening it after a shortcut is recorded so
    /// the recorder feels like a step inside the sheet rather than a detour.
    fn edit_preferences(&mut self) {
        if let PrefsOutcome::Save(updated) = self.show_preferences(self.settings.hotkey.clone()) {
            self.apply_settings(updated);
        }
    }

    /// A real preferences window rather than an alert.
    ///
    /// An `NSAlert` brings an icon, a message, and a vertical stack of buttons,
    /// which is why the old one read as a warning dialog and truncated its own
    /// labels. This lays out a proper two-column form and puts Cancel/Save
    /// where macOS puts them.
    ///
    /// Buttons are hit-tested through the event monitor instead of target/action,
    /// which avoids defining an Objective-C class for three callbacks.
    fn show_preferences(&self, pending_hotkey: String) -> PrefsOutcome {
        let app = NSApplication::sharedApplication(self.mtm);
        app.activate();

        let rows: [(&str, String); 3] = [
            ("Poll interval", self.settings.poll_interval_ms.to_string()),
            ("Entries in menu", self.settings.menu_entries.to_string()),
            ("History limit", self.settings.history_limit.to_string()),
        ];

        const W: f64 = 500.0;
        const PAD: f64 = 22.0;
        const LABEL_W: f64 = 150.0;
        const FIELD_W: f64 = 110.0;
        const ROW_H: f64 = 34.0;
        const CTRL_X: f64 = PAD + LABEL_W + 12.0;
        // shortcut + 3 numeric rows + opens row, then the button bar
        let body = ROW_H * 5.0;
        let height = PAD + 44.0 + body + PAD;

        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, height)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setTitle(&NSString::from_str("ClipVault Preferences"));

        let content = NSView::initWithFrame(
            NSView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, height)),
        );

        let label = |text: &str, y: f64| {
            let l = NSTextField::labelWithString(&NSString::from_str(text), self.mtm);
            l.setFrame(NSRect::new(
                NSPoint::new(PAD, y),
                NSSize::new(LABEL_W, 18.0),
            ));
            // Right-aligned against the controls, the way system forms read.
            l.setAlignment(TEXT_RIGHT);
            l.setTextColor(Some(&NSColor::labelColor()));
            l
        };

        let mut y = height - PAD - 26.0;

        content.addSubview(&label("Shortcut", y + 3.0));
        let shortcut = NSTextField::initWithFrame(
            NSTextField::alloc(self.mtm),
            NSRect::new(NSPoint::new(CTRL_X, y), NSSize::new(FIELD_W + 40.0, 22.0)),
        );
        shortcut.setStringValue(&NSString::from_str(&pending_hotkey));
        content.addSubview(&shortcut);

        // Show what the typed spec resolves to, so a typo is visible before
        // saving rather than after the shortcut quietly stops working.
        let resolved = NSTextField::labelWithString(
            &NSString::from_str(&format!(
                "{}   e.g. cmd+shift+V",
                hotkey::describe(&pending_hotkey)
            )),
            self.mtm,
        );
        resolved.setFrame(NSRect::new(
            NSPoint::new(CTRL_X + FIELD_W + 50.0, y + 3.0),
            NSSize::new(W - CTRL_X - FIELD_W - 50.0 - PAD, 18.0),
        ));
        resolved.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        resolved.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&resolved);

        // Numeric fields.
        let mut fields = Vec::new();
        for (index, (text, value)) in rows.iter().enumerate() {
            y -= ROW_H;
            content.addSubview(&label(text, y + 3.0));

            let field = NSTextField::initWithFrame(
                NSTextField::alloc(self.mtm),
                NSRect::new(NSPoint::new(CTRL_X, y), NSSize::new(FIELD_W, 22.0)),
            );
            field.setStringValue(&NSString::from_str(value));
            content.addSubview(&field);
            fields.push(field);

            // A unit or hint beside the field, so the labels stay short.
            let hint = match index {
                0 => "milliseconds",
                1 => "rows",
                _ => "entries (0 keeps everything)",
            };
            let h = NSTextField::labelWithString(&NSString::from_str(hint), self.mtm);
            h.setFrame(NSRect::new(
                NSPoint::new(CTRL_X + FIELD_W + 10.0, y + 3.0),
                NSSize::new(W - CTRL_X - FIELD_W - 10.0 - PAD, 18.0),
            ));
            h.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            h.setTextColor(Some(&NSColor::secondaryLabelColor()));
            content.addSubview(&h);
        }

        // Popup instead of asking the user to type "menu" or "search".
        y -= ROW_H;
        content.addSubview(&label("Shortcut opens", y + 3.0));
        let opens = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm),
            NSRect::new(NSPoint::new(CTRL_X, y - 2.0), NSSize::new(150.0, 26.0)),
            false,
        );
        opens.addItemWithTitle(&NSString::from_str("Menu"));
        opens.addItemWithTitle(&NSString::from_str("Search"));
        opens.selectItemAtIndex(if self.settings.hotkey_opens == config::OPENS_SEARCH {
            1
        } else {
            0
        });
        content.addSubview(&opens);

        // Cancel then Save, right-aligned — macOS puts the default rightmost.
        let save = NSButton::initWithFrame(
            NSButton::alloc(self.mtm),
            NSRect::new(
                NSPoint::new(W - PAD - 100.0, PAD - 6.0),
                NSSize::new(100.0, 30.0),
            ),
        );
        save.setTitle(&NSString::from_str("Save"));
        save.setBezelStyle(NSBezelStyle::Push);
        save.setKeyEquivalent(&NSString::from_str("\r"));
        content.addSubview(&save);
        let save_frame = save.frame();

        let cancel = NSButton::initWithFrame(
            NSButton::alloc(self.mtm),
            NSRect::new(
                NSPoint::new(W - PAD - 212.0, PAD - 6.0),
                NSSize::new(100.0, 30.0),
            ),
        );
        cancel.setTitle(&NSString::from_str("Cancel"));
        cancel.setBezelStyle(NSBezelStyle::Push);
        content.addSubview(&cancel);
        let cancel_frame = cancel.frame();

        panel.setContentView(Some(&content));
        panel.center();
        if let Some(first) = fields.first() {
            panel.makeFirstResponder(Some(first));
        }

        let monitor = {
            let app = app.clone();
            let block = StackBlock::new(move |event: NonNull<NSEvent>| {
                let raw = event;
                // SAFETY: AppKit hands us a live event for the handler's duration.
                let event = unsafe { raw.as_ref() };

                if event.r#type() == NSEventType::LeftMouseDown {
                    let point = event.locationInWindow();
                    if point_in(point, save_frame) {
                        app.stopModalWithCode(PREFS_SAVE);
                        return std::ptr::null_mut();
                    }
                    if point_in(point, cancel_frame) {
                        app.stopModalWithCode(PREFS_CANCEL);
                        return std::ptr::null_mut();
                    }
                    // Anything else (a text field, the popup) is handled normally.
                    return raw.as_ptr();
                }

                match event.keyCode() {
                    36 | 76 => {
                        app.stopModalWithCode(PREFS_SAVE);
                        std::ptr::null_mut()
                    }
                    53 => {
                        app.stopModalWithCode(PREFS_CANCEL);
                        std::ptr::null_mut()
                    }
                    // Everything else reaches the field being typed into.
                    _ => raw.as_ptr(),
                }
            })
            .copy();

            // SAFETY: removed before this function returns.
            unsafe {
                NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                    NSEventMask::KeyDown | NSEventMask::LeftMouseDown,
                    &block,
                )
            }
        };

        panel.makeKeyAndOrderFront(None);
        let response = app.runModalForWindow(&panel);

        let read = |index: usize| fields[index].stringValue().to_string();
        let defaults = Config::default();
        let edited = Config {
            hotkey: shortcut.stringValue().to_string(),
            // A field typed into nonsense keeps the current value rather than
            // silently snapping to a default the user didn't ask for.
            poll_interval_ms: read(0)
                .trim()
                .parse()
                .unwrap_or(self.settings.poll_interval_ms),
            menu_entries: read(1).trim().parse().unwrap_or(self.settings.menu_entries),
            history_limit: read(2).trim().parse().unwrap_or(defaults.history_limit),
            hotkey_opens: if opens.indexOfSelectedItem() == 1 {
                config::OPENS_SEARCH.to_string()
            } else {
                config::OPENS_MENU.to_string()
            },
        };

        panel.orderOut(None);
        if let Some(monitor) = monitor {
            // SAFETY: the token the matching add call returned.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }

        if response != PREFS_SAVE {
            return PrefsOutcome::Cancelled;
        }

        PrefsOutcome::Save(edited.sanitized())
    }

    /// A live fuzzy-search palette: type and the list filters as you go, ↑/↓
    /// moves, ⏎ copies.
    ///
    /// This is a real floating panel rather than an alert. An `NSAlert` brings
    /// its own icon, title, message and buttons, which is why the first version
    /// read as a dialog instead of a launcher.
    ///
    /// The query is tracked here rather than in an editable `NSTextField`,
    /// because a field would hand key events to AppKit first and leave the
    /// filter a keystroke behind. Owning the keys keeps the list in step with
    /// what's on screen.
    fn search_palette(&mut self) -> Option<String> {
        let entries = match history::read_history() {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("clipvault: could not read history: {e}");
                return None;
            }
        };

        if entries.is_empty() {
            return None;
        }

        // Newest first, flattened once so scoring doesn't redo this per keystroke.
        let candidates: Vec<Candidate> = entries
            .iter()
            .rev()
            .map(|entry| Candidate {
                content: entry.content.clone(),
                preview: display::preview(&entry.content, PALETTE_PREVIEW_WIDTH),
                meta: metadata_line(entry),
                bundle_id: entry.source_bundle_id.clone(),
            })
            .collect();

        // Resolve icons once up front; the render path then never blocks.
        for candidate in &candidates {
            if let Some(bundle_id) = &candidate.bundle_id {
                self.icons
                    .entry(bundle_id.clone())
                    .or_insert_with(|| app_icon(bundle_id));
            }
        }

        let app = NSApplication::sharedApplication(self.mtm);
        app.activate();

        if self.palette.is_none() {
            let (panel, view) = self.build_palette();
            self.palette = Some((panel, Rc::new(view)));
        }
        let (panel, view) = self.palette.as_ref().expect("just built");
        let (panel, view) = (panel.clone(), Rc::clone(view));
        position_palette(&panel);

        let state = Rc::new(RefCell::new(PaletteState {
            query: String::new(),
            selection: 0,
            offset: 0,
            matches: (0..candidates.len()).collect(),
        }));

        let candidates = Rc::new(candidates);
        view.render(&state.borrow(), &candidates, &self.icons);

        let monitor = {
            let state = Rc::clone(&state);
            let view = Rc::clone(&view);
            let candidates = Rc::clone(&candidates);
            let icons = self.icons.clone();
            let app = app.clone();
            let panel_height = panel.frame().size.height;
            let scroll_carry = Rc::new(RefCell::new(0.0f64));
            let mtm = self.mtm;

            let block = StackBlock::new(move |event: NonNull<NSEvent>| {
                // SAFETY: AppKit hands us a live NSEvent for the handler's
                // duration, and monitors run on the main thread.
                let raw = event;
                let event = unsafe { raw.as_ref() };
                let flags = event.modifierFlags();
                let control = flags.contains(NSEventModifierFlags::Control);
                let command = flags.contains(NSEventModifierFlags::Command);

                let mut state = state.borrow_mut();

                if event.r#type() == NSEventType::ScrollWheel {
                    // Trackpads emit a stream of small deltas, so accumulate and
                    // step a row each time the total crosses a threshold.
                    let delta = if event.hasPreciseScrollingDeltas() {
                        event.scrollingDeltaY()
                    } else {
                        event.scrollingDeltaY() * SCROLL_LINE_HEIGHT
                    };

                    let mut carry = scroll_carry.borrow_mut();
                    *carry += delta;
                    while *carry <= -SCROLL_STEP {
                        *carry += SCROLL_STEP;
                        state.move_selection(1);
                    }
                    while *carry >= SCROLL_STEP {
                        *carry -= SCROLL_STEP;
                        state.move_selection(-1);
                    }

                    view.render(&state, &candidates, &icons);
                    return std::ptr::null_mut();
                }

                // A click on a row picks it. People reach for the mouse whether
                // or not the palette is keyboard-first.
                if event.r#type() == NSEventType::LeftMouseDown {
                    let live_height = event
                        .window(mtm)
                        .map(|w| w.frame().size.height)
                        .unwrap_or(panel_height);
                    if let Some(row) = row_at(event.locationInWindow(), live_height)
                        && state.offset + row < state.matches.len()
                    {
                        state.selection = state.offset + row;
                        view.render(&state, &candidates, &icons);
                        app.stopModalWithCode(PALETTE_ACCEPT);
                    }
                    return std::ptr::null_mut();
                }

                match event.keyCode() {
                    // ⏎ accepts, esc dismisses. With no buttons on the panel,
                    // ending the modal session is ours to do.
                    36 | 76 => app.stopModalWithCode(PALETTE_ACCEPT),
                    53 => app.stopModalWithCode(PALETTE_CANCEL),
                    126 => state.move_selection(-1),
                    125 => state.move_selection(1),
                    51 => {
                        state.query.pop();
                        state.refilter(&candidates);
                    }
                    _ => {
                        if command {
                            // ⌘1–⌘9 lands straight on a visible row, the same way
                            // the menu numbers its entries.
                            if let Some(digit) = event
                                .charactersIgnoringModifiers()
                                .and_then(|c| c.to_string().chars().next())
                                .and_then(|c| c.to_digit(10))
                                .filter(|d| (1..=9).contains(d))
                            {
                                let row = digit as usize - 1;
                                if state.offset + row < state.matches.len() {
                                    state.selection = state.offset + row;
                                    view.render(&state, &candidates, &icons);
                                    app.stopModalWithCode(PALETTE_ACCEPT);
                                    return std::ptr::null_mut();
                                }
                            }
                        } else if control {
                            // ⌃N/⌃P are the same motion in fzf and vim.
                            match event
                                .charactersIgnoringModifiers()
                                .map(|s| s.to_string())
                                .as_deref()
                            {
                                Some("n") => state.move_selection(1),
                                Some("p") => state.move_selection(-1),
                                _ => {}
                            }
                        } else if let Some(text) = event.characters().map(|s| s.to_string()) {
                            let printable: String =
                                text.chars().filter(|c| !c.is_control()).collect();
                            if !printable.is_empty() {
                                state.query.push_str(&printable);
                                state.refilter(&candidates);
                            }
                        }
                    }
                }

                view.render(&state, &candidates, &icons);
                // Swallow everything else so keys can't leak into the app behind.
                std::ptr::null_mut()
            })
            .copy();

            // SAFETY: removed before this function returns, so the block never
            // outlives what it borrows.
            unsafe {
                NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                    NSEventMask::KeyDown | NSEventMask::LeftMouseDown | NSEventMask::ScrollWheel,
                    &block,
                )
            }
        };

        // Dismiss as soon as the palette stops being the key window — clicking
        // another app, or anything else taking focus. Without this the panel
        // floats above everything with no way to reach it, because the modal
        // session swallows the clicks aimed at it.
        let resign = {
            let app = app.clone();
            let block = StackBlock::new(move |_: NonNull<NSNotification>| {
                app.stopModalWithCode(PALETTE_CANCEL);
            })
            .copy();

            // SAFETY: the observer is removed before this function returns, and
            // the notification is delivered on the main thread, which is where
            // the panel's key state changes.
            unsafe {
                NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                    Some(NSWindowDidResignKeyNotification),
                    Some(&panel),
                    None,
                    &block,
                )
            }
        };

        panel.makeKeyAndOrderFront(None);
        let response = app.runModalForWindow(&panel);

        // Detach before hiding the panel: `orderOut` makes it resign key, which
        // would otherwise fire the observer and call `stopModal` again with the
        // session already finished.
        // SAFETY: `resign` is the token the matching add call returned, and it
        // is removed exactly once.
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(resign.as_ref()) };
        panel.orderOut(None);

        if let Some(monitor) = monitor {
            // SAFETY: the token the matching add call returned.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }

        if response != PALETTE_ACCEPT {
            return None;
        }

        let state = state.borrow();
        state
            .matches
            .get(state.selection)
            .map(|index| candidates[*index].content.clone())
    }

    /// Builds the panel and every view it reuses across keystrokes.
    fn build_palette(&self) -> (Retained<NSPanel>, PaletteView) {
        let height = palette_height(PALETTE_ROWS);
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PALETTE_WIDTH, height));

        // Titled + FullSizeContentView with a hidden, transparent titlebar: it
        // looks borderless but can still become the key window, which a truly
        // borderless window cannot without subclassing NSWindow.
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(self.mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setTitlebarAppearsTransparent(true);
        panel.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        panel.setMovableByWindowBackground(true);
        panel.setLevel(NSFloatingWindowLevel);
        for button in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(button) = panel.standardWindowButton(button) {
                button.setHidden(true);
            }
        }

        // The blurred backdrop is most of what makes this read as a system
        // surface rather than a grey rectangle.
        let backdrop =
            NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(self.mtm), frame);
        backdrop.setMaterial(NSVisualEffectMaterial::Popover);
        backdrop.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        backdrop.setState(NSVisualEffectState::Active);

        // Search line, at the top (coordinates run bottom-up).
        let glyph = NSImageView::initWithFrame(
            NSImageView::alloc(self.mtm),
            NSRect::new(
                NSPoint::new(PALETTE_PADDING, height - PALETTE_SEARCH_HEIGHT + 18.0),
                NSSize::new(PALETTE_ICON, PALETTE_ICON),
            ),
        );
        if let Some(image) = symbol("magnifyingglass", "Search") {
            glyph.setImage(Some(&image));
        }
        // Everything above the footer keeps its distance from the top, so the
        // panel can shrink to fit the results without the layout drifting.
        glyph.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        backdrop.addSubview(&glyph);

        let query_label = NSTextField::labelWithString(&NSString::from_str(""), self.mtm);
        query_label.setFrame(NSRect::new(
            NSPoint::new(PALETTE_TEXT_LEFT, height - PALETTE_SEARCH_HEIGHT + 16.0),
            NSSize::new(PALETTE_WIDTH - PALETTE_TEXT_LEFT - PALETTE_PADDING, 26.0),
        ));
        query_label.setFont(Some(&NSFont::systemFontOfSize(21.0)));
        query_label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        backdrop.addSubview(&query_label);

        let top_rule = separator(
            self.mtm,
            NSRect::new(
                NSPoint::new(0.0, height - PALETTE_SEARCH_HEIGHT),
                NSSize::new(PALETTE_WIDTH, 1.0),
            ),
        );
        top_rule.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        backdrop.addSubview(&top_rule);

        let mut rows = Vec::new();
        for index in 0..PALETTE_ROWS {
            let y = height
                - PALETTE_SEARCH_HEIGHT
                - PALETTE_LIST_GAP
                - PALETTE_ROW_HEIGHT * (index as f64 + 1.0);
            let row = PaletteRow::new(self.mtm, y);
            row.box_view
                .setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
            backdrop.addSubview(&row.box_view);
            rows.push(row);
        }

        // Shown in place of the rows when nothing matches.
        let empty = NSTextField::labelWithString(&NSString::from_str(""), self.mtm);
        empty.setFrame(NSRect::new(
            // Sits in the first row's slot, so it still lands correctly once the
            // panel has shrunk to its one-row height.
            NSPoint::new(
                0.0,
                height - PALETTE_SEARCH_HEIGHT - PALETTE_LIST_GAP - PALETTE_ROW_HEIGHT
                    + (PALETTE_ROW_HEIGHT - 24.0) / 2.0,
            ),
            NSSize::new(PALETTE_WIDTH, 24.0),
        ));
        empty.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        empty.setTextColor(Some(&NSColor::secondaryLabelColor()));
        empty.setAlignment(TEXT_CENTER);
        empty.setHidden(true);
        empty.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        backdrop.addSubview(&empty);

        let bottom_rule = separator(
            self.mtm,
            NSRect::new(
                NSPoint::new(0.0, PALETTE_FOOTER_HEIGHT),
                NSSize::new(PALETTE_WIDTH, 1.0),
            ),
        );
        bottom_rule.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
        backdrop.addSubview(&bottom_rule);

        let count_label = NSTextField::labelWithString(&NSString::from_str(""), self.mtm);
        count_label.setFrame(NSRect::new(
            NSPoint::new(PALETTE_PADDING, 7.0),
            NSSize::new(PALETTE_WIDTH / 2.0, 16.0),
        ));
        count_label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        count_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        count_label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
        backdrop.addSubview(&count_label);

        let hints = NSTextField::labelWithString(
            &NSString::from_str("↑↓ navigate   ⌘1–9 pick   ⏎ copy   esc close"),
            self.mtm,
        );
        hints.setFrame(NSRect::new(
            NSPoint::new(PALETTE_WIDTH / 2.0, 7.0),
            NSSize::new(PALETTE_WIDTH / 2.0 - PALETTE_PADDING, 16.0),
        ));
        hints.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        hints.setTextColor(Some(&NSColor::tertiaryLabelColor()));
        hints.setAlignment(TEXT_RIGHT);
        hints.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
        backdrop.addSubview(&hints);

        panel.setContentView(Some(&backdrop));

        let view = PaletteView {
            panel: panel.clone(),
            query_label,
            rows,
            empty,
            count_label,
        };

        (panel, view)
    }

    fn apply_settings(&mut self, updated: Config) {
        if updated == self.settings {
            return;
        }

        if updated.hotkey != self.settings.hotkey {
            let next = hotkey::parse(&updated.hotkey);
            // Release the old binding first: registering a shortcut the system
            // still has attached to us would just fail.
            if let Err(e) = self.hotkeys.unregister(self.hotkey) {
                eprintln!("clipvault: could not release the old shortcut: {e}");
            }
            match self.hotkeys.register(next) {
                Ok(()) => self.hotkey = next,
                Err(e) => {
                    eprintln!("clipvault: could not register {}: {e}", updated.hotkey);
                    // Put the previous one back so the app doesn't end up with
                    // no way to open at all.
                    let _ = self.hotkeys.register(self.hotkey);

                    // Say so. A shortcut another app already owns would
                    // otherwise appear to save and then quietly not work.
                    let alert = NSAlert::new(self.mtm);
                    alert.setMessageText(&NSString::from_str("Shortcut unavailable"));
                    alert.setInformativeText(&NSString::from_str(&format!(
                        "{} could not be registered — another app is probably using it. Keeping {}.",
                        hotkey::describe(&updated.hotkey),
                        hotkey::describe(&self.settings.hotkey),
                    )));
                    alert.runModal();

                    // Keep the stored setting matching what is actually bound.
                    let mut updated = updated;
                    updated.hotkey = self.settings.hotkey.clone();
                    return self.finish_settings(updated);
                }
            }
        }

        self.finish_settings(updated);
    }

    fn finish_settings(&mut self, updated: Config) {
        self.poll_every_ticks = (updated.poll_interval_ms / TICK_MS).max(1) as u32;
        self.ticks_to_poll = self.poll_every_ticks;
        self.settings = updated;

        if let Err(e) = config::save(&self.settings) {
            eprintln!("clipvault: could not save preferences: {e}");
        }

        self.prune_now();
        self.refresh();
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

/// Resolves an installed app's icon, scaled to menu row size.
fn app_icon(bundle_id: &str) -> Option<Retained<NSImage>> {
    let workspace = NSWorkspace::sharedWorkspace();
    let url = workspace.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
    let path = url.path()?;

    let icon = workspace.iconForFile(&path);
    icon.setSize(NSSize::new(ROW_ICON_POINTS, ROW_ICON_POINTS));
    Some(icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(count: usize) -> Vec<Candidate> {
        (0..count)
            .map(|i| Candidate {
                content: format!("entry {i}"),
                preview: format!("entry {i}"),
                meta: String::new(),
                bundle_id: None,
            })
            .collect()
    }

    fn palette(count: usize) -> PaletteState {
        PaletteState {
            query: String::new(),
            selection: 0,
            offset: 0,
            matches: (0..count).collect(),
        }
    }

    #[test]
    fn selection_reaches_results_past_the_visible_rows() {
        // The bug this replaced clamped selection to the row count, so anything
        // beyond the last visible row could never be picked.
        let mut state = palette(40);
        for _ in 0..20 {
            state.move_selection(1);
        }
        assert_eq!(state.selection, 20);
        assert!(state.matches.len() > PALETTE_ROWS);
    }

    #[test]
    fn the_window_scrolls_to_keep_the_selection_visible() {
        let mut state = palette(40);
        for _ in 0..PALETTE_ROWS {
            state.move_selection(1);
        }
        // Selection just stepped past the last row, so the window moved by one.
        assert_eq!(state.selection, PALETTE_ROWS);
        assert_eq!(state.offset, 1);
        assert_eq!(state.visible().len(), PALETTE_ROWS);
        assert!(state.visible().contains(&state.selection));
    }

    #[test]
    fn the_selection_stops_at_both_ends_instead_of_wrapping() {
        let mut state = palette(30);

        // Already at the top: up does nothing.
        state.move_selection(-1);
        assert_eq!(state.selection, 0);
        assert_eq!(state.offset, 0);

        // Walk to the bottom and keep pushing.
        for _ in 0..40 {
            state.move_selection(1);
        }
        assert_eq!(state.selection, 29, "should rest on the last result");
        assert!(state.visible().contains(&29), "end should be on screen");

        // And back up, still without wrapping.
        for _ in 0..40 {
            state.move_selection(-1);
        }
        assert_eq!(state.selection, 0);
        assert_eq!(state.offset, 0, "window should follow back to the top");
    }

    #[test]
    fn a_short_result_list_never_scrolls() {
        let mut state = palette(3);
        for _ in 0..10 {
            state.move_selection(1);
        }
        assert_eq!(state.offset, 0);
        assert_eq!(state.visible().len(), 3);
    }

    #[test]
    fn no_matches_leaves_the_selection_pinned_and_the_window_empty() {
        let mut state = palette(0);
        state.move_selection(1);
        state.move_selection(-1);
        assert_eq!(state.selection, 0);
        assert!(state.visible().is_empty());
    }

    #[test]
    fn filtering_resets_the_scroll_position() {
        let all = candidates(40);
        let mut state = palette(40);
        for _ in 0..20 {
            state.move_selection(1);
        }
        assert!(state.offset > 0);

        state.query = "entry 3".to_string();
        state.refilter(&all);
        // A new query starts from the top; leaving the old scroll would hide the
        // best match.
        assert_eq!(state.selection, 0);
        assert_eq!(state.offset, 0);
    }

    /// Top of the result list, measured the same way the layout does.
    fn list_top(height: f64) -> f64 {
        height - PALETTE_SEARCH_HEIGHT - PALETTE_LIST_GAP
    }

    #[test]
    fn clicks_map_to_the_row_under_the_pointer() {
        let height = palette_height(PALETTE_ROWS);
        let top = list_top(height);

        // Just under the search field is the first row; window coords run up.
        assert_eq!(row_at(NSPoint::new(100.0, top - 1.0), height), Some(0));
        assert_eq!(
            row_at(NSPoint::new(100.0, top - PALETTE_ROW_HEIGHT * 2.5), height),
            Some(2)
        );
        // The last row still resolves rather than falling into the footer.
        assert_eq!(
            row_at(
                NSPoint::new(100.0, top - PALETTE_ROW_HEIGHT * PALETTE_ROWS as f64 + 1.0),
                height
            ),
            Some(PALETTE_ROWS - 1)
        );
    }

    #[test]
    fn clicks_on_the_chrome_select_nothing() {
        let height = palette_height(PALETTE_ROWS);

        // In the search field.
        assert_eq!(row_at(NSPoint::new(100.0, height - 10.0), height), None);
        // In the gap between the search field and the first row.
        assert_eq!(
            row_at(NSPoint::new(100.0, list_top(height) + 2.0), height),
            None
        );
        // In the footer.
        assert_eq!(row_at(NSPoint::new(100.0, 10.0), height), None);
    }

    #[test]
    fn refilter_ranks_the_closest_match_first() {
        let all = candidates(40);
        let mut state = palette(40);
        state.query = "entry 7".to_string();
        state.refilter(&all);

        let best = &all[state.matches[0]];
        assert_eq!(best.content, "entry 7");
    }
}
