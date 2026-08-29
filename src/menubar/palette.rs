//! The search palette — the Spotlight-style window the shortcut can open.
//!
//! The state ([`PaletteState`]) is deliberately separate from the views
//! ([`PaletteView`]): filtering, selection and scrolling are ordinary logic
//! worth testing, and none of it needs AppKit to be running.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::StackBlock;
// `muda` and `global-hotkey` re-export these from the same `keyboard-types`
// version, so one import covers both the menu accelerators and the hotkey.
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSBox, NSBoxType, NSColor,
    NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSFloatingWindowLevel, NSFont,
    NSImage, NSImageView, NSModalResponse, NSPanel, NSScreen, NSTextField, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowButton, NSWindowDidResignKeyNotification, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSPoint, NSRect, NSSize, NSString};

use crate::{display, fuzzy, history};

use super::icons::{app_icon, symbol};
use super::{App, TEXT_CENTER, TEXT_RIGHT, metadata_line};

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
pub(super) struct PaletteView {
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

impl App {
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
    pub(super) fn search_palette(&mut self) -> Option<String> {
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
