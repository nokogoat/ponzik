//! Editing in the text fields (rename, search, typed values, paths…): a caret,
//! a selection (Shift+arrows, the mouse, a double-click for a word), word moves
//! and deletes with Ctrl, Ctrl+A / C / X / V with the system clipboard, a
//! right-click menu, and the middle button pasting the primary selection (X11).
//!
//! One field has the keyboard at a time: its caret lives in [`EditState`],
//! keyed by the field's id, and resets when another field takes over.

use ponzik_i18n::{tr, tr_ctx};
use ponzik_ui::{Canvas, Color, FontId, Rect};
use ponzik_window::{Key, Modifiers};

use super::app::App;
use super::Input;
use super::widgets::wid;

/// The caret and selection of the field being edited. Positions are byte
/// offsets on character boundaries; the selection runs from `anchor` to `caret`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditState {
    pub id: u64,
    pub caret: usize,
    pub anchor: usize,
    /// Horizontal scroll of a text longer than its field, in pixels.
    pub scroll: f32,
    /// The left button selects while it is held.
    pub dragging: bool,
    /// Text to insert at the caret on the next frame (a paste from the menu or
    /// the middle button).
    pub insert: Option<String>,
    /// Commands of the right-click menu for the next frame, as their keys.
    pub inject: Vec<(Key, Modifiers)>,
}

/// What an edit did, and the keys it left for the caller (Enter, Escape, Up,
/// Down, Tab, Ctrl shortcuts…).
#[derive(Debug, Default, PartialEq)]
pub struct Edited {
    pub changed: bool,
    pub enter: bool,
    pub escape: bool,
    /// Text for the clipboard (Ctrl+C, Ctrl+X).
    pub copy: Option<String>,
    pub rest: Vec<(Key, Modifiers)>,
}

/// How a field takes its keys.
#[derive(Clone, Copy, Debug, Default)]
pub struct EditOpts {
    /// Home and End (without Shift) are left to the caller: a list under a
    /// filter field moves with them.
    pub keep_home_end: bool,
}

const CTRL: Modifiers = Modifiers { shift: false, ctrl: true, alt: false };

/// One line of text from a paste: line breaks and tabs become spaces, the
/// other control characters go, a final line break is dropped.
pub fn single_line(s: &str) -> String {
    let s = s.trim_end_matches(['\r', '\n']);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' | '\n' | '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Word, space or punctuation: what a double-click and Ctrl+arrows stop at.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Start of the word before `at` (Ctrl+Left, Ctrl+Backspace).
pub fn word_left(text: &str, at: usize) -> usize {
    let mut it = text[..at].char_indices().rev().peekable();
    while it.next_if(|&(_, c)| c.is_whitespace()).is_some() {}
    let Some(&(_, first)) = it.peek() else { return 0 };
    let k = class(first);
    let mut pos = at;
    while let Some((i, _)) = it.next_if(|&(_, c)| class(c) == k) {
        pos = i;
    }
    if it.peek().is_none() { 0 } else { pos }
}

/// End of the word after `at` (Ctrl+Right, Ctrl+Delete).
pub fn word_right(text: &str, at: usize) -> usize {
    let mut it = text[at..].char_indices().peekable();
    while it.next_if(|&(_, c)| c.is_whitespace()).is_some() {}
    let Some(&(_, first)) = it.peek() else { return text.len() };
    let k = class(first);
    while it.next_if(|&(_, c)| class(c) == k).is_some() {}
    it.peek().map_or(text.len(), |&(i, _)| at + i)
}

/// The word (or run of spaces or punctuation) around `at`, for a double-click.
pub fn word_at(text: &str, at: usize) -> (usize, usize) {
    let Some(c) = text[at..].chars().next().or_else(|| text[..at].chars().next_back()) else { return (0, 0) };
    let k = class(c);
    let start = text[..at].char_indices().rev().take_while(|&(_, c)| class(c) == k).last().map_or(at, |(i, _)| i);
    let end = text[at..].char_indices().find(|&(_, c)| class(c) != k).map_or(text.len(), |(i, _)| at + i);
    (start, end)
}

fn prev_char(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next_char(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

impl EditState {
    /// The caret at the end of `text`, nothing selected.
    pub fn at_end(id: u64, text: &str) -> Self {
        Self { id, caret: text.len(), anchor: text.len(), ..Self::default() }
    }

    /// The selection, start first.
    pub fn range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selected<'a>(&self, text: &'a str) -> &'a str {
        let (a, b) = self.range();
        text.get(a..b).unwrap_or("")
    }

    /// Keeps the positions on `text` (it may have changed elsewhere).
    pub fn clamp(&mut self, text: &str) {
        self.caret = floor_boundary(text, self.caret);
        self.anchor = floor_boundary(text, self.anchor);
    }

    fn set(&mut self, at: usize, extend: bool) {
        self.caret = at;
        if !extend {
            self.anchor = at;
        }
    }

    /// Replaces the selection (or inserts at the caret).
    pub fn insert(&mut self, text: &mut String, s: &str) -> bool {
        let (a, b) = self.range();
        if s.is_empty() && a == b {
            return false;
        }
        text.replace_range(a..b, s);
        self.set(a + s.len(), false);
        true
    }

    /// Applies the frame's typed text, then its keys. `paste` is the
    /// clipboard's text, read beforehand when Ctrl+V is among the keys.
    pub fn apply(&mut self, text: &mut String, keys: &[(Key, Modifiers)], typed: &str, paste: Option<&str>, opts: EditOpts) -> Edited {
        self.clamp(text);
        let mut out = Edited::default();
        if let Some(s) = self.insert.take() {
            out.changed |= self.insert(text, &single_line(&s));
        }
        let typed = single_line(typed);
        if !typed.is_empty() {
            out.changed |= self.insert(text, &typed);
        }
        for &(key, mods) in keys {
            let shift = mods.shift;
            match key {
                Key::Char('a') if mods.ctrl && !mods.alt => self.set_all(text),
                Key::Char('c') if mods.ctrl && !mods.alt && !shift => {
                    if self.has_selection() {
                        out.copy = Some(self.selected(text).to_string());
                    }
                }
                Key::Char('x') if mods.ctrl && !mods.alt => {
                    if self.has_selection() {
                        out.copy = Some(self.selected(text).to_string());
                        out.changed |= self.insert(text, "");
                    }
                }
                Key::Char('v') if mods.ctrl && !mods.alt => {
                    if let Some(p) = paste {
                        out.changed |= self.insert(text, &single_line(p));
                    }
                }
                Key::Backspace | Key::Delete => {
                    if !self.has_selection() {
                        let at = self.caret;
                        let to = match (key, mods.ctrl) {
                            (Key::Backspace, false) => prev_char(text, at),
                            (Key::Backspace, true) => word_left(text, at),
                            (_, false) => next_char(text, at),
                            (_, true) => word_right(text, at),
                        };
                        if to == at {
                            // Nothing to delete: the caller may want it (a folder up).
                            out.rest.push((key, mods));
                            continue;
                        }
                        self.anchor = to;
                    }
                    out.changed |= self.insert(text, "");
                }
                Key::Left | Key::Right if !mods.alt => {
                    let left = key == Key::Left;
                    let at = if self.has_selection() && !shift && !mods.ctrl {
                        // A selection collapses to its edge.
                        if left { self.range().0 } else { self.range().1 }
                    } else {
                        match (left, mods.ctrl) {
                            (true, false) => prev_char(text, self.caret),
                            (true, true) => word_left(text, self.caret),
                            (false, false) => next_char(text, self.caret),
                            (false, true) => word_right(text, self.caret),
                        }
                    };
                    self.set(at, shift);
                }
                Key::Home | Key::End if !mods.alt && (shift || !opts.keep_home_end) => {
                    self.set(if key == Key::Home { 0 } else { text.len() }, shift);
                }
                Key::Enter if !mods.ctrl && !mods.alt => {
                    out.enter = true;
                    out.rest.push((key, mods));
                }
                Key::Escape => {
                    out.escape = true;
                    out.rest.push((key, mods));
                }
                // Typed characters came as text above.
                Key::Char(_) | Key::Space if !mods.ctrl && !mods.alt => {}
                _ => out.rest.push((key, mods)),
            }
        }
        out
    }

    fn set_all(&mut self, text: &str) {
        self.anchor = 0;
        self.caret = text.len();
    }
}

/// The right-click menu of a text field.
#[derive(Clone, Debug, PartialEq)]
pub struct EditMenu {
    /// The field it belongs to.
    pub id: u64,
    pub x: f32,
    pub y: f32,
    /// Where it was drawn last, for the clicks of the next frame.
    pub rect: Rect,
    /// The field had a selection when it opened: Cut and Copy work.
    pub selection: bool,
}

const MENU_W: f32 = 200.0;
const MENU_ROW: f32 = 32.0;
const MENU_PAD: f32 = 6.0;

/// Rows of the menu: label, shortcut, the key it sends to the field.
fn menu_rows() -> [(&'static str, &'static str, Key); 4] {
    [
        (tr_ctx("text", "Cut"), "Ctrl+X", Key::Char('x')),
        (tr_ctx("text", "Copy"), "Ctrl+C", Key::Char('c')),
        (tr_ctx("text", "Paste"), "Ctrl+V", Key::Char('v')),
        (tr("Select all"), "Ctrl+A", Key::Char('a')),
    ]
}

fn menu_row_rect(m: &EditMenu, i: usize) -> Rect {
    Rect::new(m.rect.x + MENU_PAD, m.rect.y + MENU_PAD + i as f32 * MENU_ROW, m.rect.w - 2.0 * MENU_PAD, MENU_ROW)
}

/// A field to draw with [`App::edit_field`].
pub struct Field<'a> {
    pub id: u64,
    pub text: &'a str,
    /// Where the text may show (clipped there), and where it starts.
    pub area: Rect,
    pub x: f32,
    pub cy: f32,
    pub font: FontId,
    pub size: f32,
    pub ink: Color,
    pub caret: Color,
}

impl App {
    /// The system clipboard's text, or what Ponzik copied last without one.
    pub(super) fn clipboard_text(&mut self) -> Option<String> {
        match self.clipboard.as_mut() {
            Some(c) => c.text(),
            None => (!self.clip_text.is_empty()).then(|| self.clip_text.clone()),
        }
    }

    pub(super) fn set_clipboard_text(&mut self, text: &str) {
        self.clip_text = text.to_string();
        if let Some(c) = self.clipboard.as_mut() {
            c.set_text(text);
        }
    }

    fn set_primary(&mut self, text: &str) {
        if let Some(c) = self.clipboard.as_mut() {
            c.set_selection(text);
        }
    }

    /// The keys and typed text of this frame for the field `id`, which has
    /// the keyboard. Returns what happened and the keys it did not use.
    pub(super) fn edit_keys(&mut self, id: u64, text: &mut String, input: &Input, opts: EditOpts) -> Edited {
        if self.edit.id != id {
            self.edit = EditState::at_end(id, text);
        }
        let mut keys = std::mem::take(&mut self.edit.inject);
        keys.extend_from_slice(&input.keys);
        let wants_paste = keys.iter().any(|&(k, m)| k == Key::Char('v') && m.ctrl && !m.alt);
        let paste = if wants_paste { self.clipboard_text() } else { None };
        let before = self.edit.range();
        let out = self.edit.apply(text, &keys, &input.text, paste.as_deref(), opts);
        if let Some(copy) = &out.copy {
            self.set_clipboard_text(copy);
        }
        // A new selection from the keyboard becomes the primary selection.
        if self.edit.has_selection() && self.edit.range() != before && out.copy.is_none() {
            let s = self.edit.selected(text).to_string();
            self.set_primary(&s);
        }
        if out.changed || !keys.is_empty() {
            self.edit_typed_at = self.time;
        }
        out
    }

    /// Byte offset in `f.text` under the pointer at `px`.
    fn edit_hit(&self, f: &Field, px: f32) -> usize {
        let x = px - (f.x - self.edit.scroll);
        let mut best = (0, f32::MAX);
        for i in f.text.char_indices().map(|(i, _)| i).chain(std::iter::once(f.text.len())) {
            let d = (self.fonts.measure(f.font, f.size, &f.text[..i]) - x).abs();
            if d < best.1 {
                best = (i, d);
            }
        }
        best.0
    }

    /// Draws the field `f` being edited: selection, text, blinking caret; and
    /// takes its mouse: click and drag to place and select, double-click for a
    /// word, the middle button to paste the primary selection, the right one
    /// for the Cut / Copy / Paste menu.
    pub(super) fn edit_field(&mut self, c: &mut Canvas, input: &Input, f: &Field) {
        if self.edit.id != f.id {
            self.edit = EditState::at_end(f.id, f.text);
        }
        self.edit.clamp(f.text);
        self.edit_drawn = true;
        let t = self.theme;
        let width = |s: &Self, a: usize| s.fonts.measure(f.font, f.size, &f.text[..a]);
        let room = (f.area.right() - f.x - 2.0).max(8.0);
        let total = width(self, f.text.len());

        // Mouse.
        let over = input.inside && f.area.contains(input.x, input.y);
        if over && input.pressed {
            let at = self.edit_hit(f, input.x);
            if input.double {
                let (a, b) = word_at(f.text, at);
                (self.edit.anchor, self.edit.caret) = (a, b);
                self.edit.dragging = false;
                if b > a {
                    self.set_primary(&f.text[a..b]);
                }
            } else {
                self.edit.set(at, input.mods.shift);
                self.edit.dragging = true;
            }
            self.edit_typed_at = self.time;
        } else if self.edit.dragging && input.down {
            let at = self.edit_hit(f, input.x);
            self.edit.caret = at;
        } else if self.edit.dragging {
            self.edit.dragging = false;
            if self.edit.has_selection() {
                let s = self.edit.selected(f.text).to_string();
                self.set_primary(&s);
            }
        }
        if over && input.middle_pressed {
            let at = self.edit_hit(f, input.x);
            self.edit.set(at, false);
            let primary = self.clipboard.as_mut().and_then(|c| c.selection());
            if let Some(p) = primary {
                self.edit.insert = Some(p);
            }
        }
        if over && input.right_pressed {
            self.edit_menu =
                Some(EditMenu { id: f.id, x: input.x, y: input.y, rect: Rect::default(), selection: self.edit.has_selection() });
        }
        if over {
            self.set_cursor(ponzik_window::Cursor::Text);
        }

        // Scroll so that the caret shows.
        let cx = width(self, self.edit.caret);
        let mut scroll = self.edit.scroll.min((total - room).max(0.0));
        if cx - scroll > room {
            scroll = cx - room;
        } else if cx < scroll {
            scroll = cx;
        }
        self.edit.scroll = scroll.max(0.0);
        let x0 = f.x - self.edit.scroll;

        let prev = c.push_clip(f.area);
        let h = f.size + 8.0;
        if self.edit.has_selection() {
            let (a, b) = self.edit.range();
            let (xa, xb) = (x0 + width(self, a), x0 + width(self, b));
            c.fill_rrect(Rect::new(xa, f.cy - h * 0.5, xb - xa, h), 3.0, t.select.alpha(0.32));
        }
        self.text(c, f.font, f.size, x0, f.cy, f.text, f.ink);
        // Solid while typing, then blinking.
        let since = self.time - self.edit_typed_at;
        if since < 0.5 || (since * 2.0).fract() < 0.6 {
            c.fill_rect(Rect::new(x0 + cx, f.cy - h * 0.5 + 1.0, 1.5, h - 2.0), f.caret);
        }
        c.set_clip(Some(prev));
        // Keeps the frames coming for the blink.
        self.anim.set(wid("caret", 0, 0), self.time);
    }

    /// Capture: a selection in the field being edited (`anchor,caret`), and
    /// its menu opened at `x,y` if given.
    pub(super) fn screenshot_text_edit(&mut self, spec: &str, first: bool) {
        let v: Vec<f32> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if let [anchor, caret, ..] = v[..] {
            (self.edit.anchor, self.edit.caret) = (anchor as usize, caret as usize);
        }
        if let [_, _, x, y] = v[..]
            && first
        {
            self.edit_menu = Some(EditMenu { id: self.edit.id, x, y, rect: Rect::default(), selection: self.edit.has_selection() });
        }
        self.edit_typed_at = self.time;
    }

    /// Before the frame: a click in the open text menu runs its row (and does
    /// not reach what is under it); Escape or a click elsewhere closes it.
    pub(super) fn edit_menu_begin(&mut self, input: &Input) -> Option<Input> {
        self.edit_drawn = false;
        let m = self.edit_menu.clone()?;
        if input.keys.iter().any(|(k, _)| *k == Key::Escape) {
            self.edit_menu = None;
            let keys = input.keys.iter().copied().filter(|(k, _)| *k != Key::Escape).collect();
            return Some(Input { keys, ..input.clone() });
        }
        let inside = m.rect.contains(input.x, input.y);
        if !(input.pressed || input.right_pressed) {
            // What is under the menu does not light up.
            return inside.then(|| Input { wheel: 0.0, inside: false, ..input.clone() });
        }
        self.edit_menu = None;
        if !inside {
            return None;
        }
        if input.pressed
            && let Some(i) = (0..4).find(|&i| menu_row_rect(&m, i).contains(input.x, input.y))
        {
            let (_, _, key) = menu_rows()[i];
            let usable = m.selection || i >= 2;
            if usable && m.id == self.edit.id {
                if key == Key::Char('v') {
                    self.edit.insert = self.clipboard_text();
                } else {
                    self.edit.inject.push((key, CTRL));
                }
            }
        }
        Some(Input { pressed: false, right_pressed: false, double: false, wheel: 0.0, ..input.clone() })
    }

    /// After everything else: the text menu, over the field it belongs to.
    pub(super) fn edit_menu_layer(&mut self, c: &mut Canvas, screen: Rect) {
        let Some(mut m) = self.edit_menu.clone() else { return };
        if !self.edit_drawn || m.id != self.edit.id {
            // Its field went away.
            self.edit_menu = None;
            return;
        }
        let t = self.theme;
        let h = 4.0 * MENU_ROW + 2.0 * MENU_PAD;
        let x = m.x.min(screen.right() - MENU_W - 8.0).max(8.0);
        let y = if m.y + h + 8.0 > screen.bottom() { m.y - h } else { m.y };
        m.rect = Rect::new(x, y, MENU_W, h);
        let open = self.anim.to(wid("text-menu", 0, 0), 1.0, super::theme::T_HOVER);
        let r = m.rect;
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), 14.0, t.shade(90.0 * open));
        c.fill_rrect(r, 12.0, t.panel.alpha(open));
        c.stroke_rrect(r, 12.0, 1.0, t.hairline.alpha(1.6 * open));
        for (i, (label, keys, _)) in menu_rows().into_iter().enumerate() {
            let row = menu_row_rect(&m, i);
            let usable = m.selection || i >= 2;
            let hov = usable && row.contains(self.pointer.0, self.pointer.1);
            let hv = self.hover_anim(wid("text-menu-row", i, 0), hov);
            if hv > 0.01 {
                c.fill_rrect(row, 8.0, t.raised.mix(t.raised_hi, hv * 0.6).alpha(hv * open));
            }
            let cy = row.y + row.h * 0.5;
            let ink = if usable { t.text2.mix(t.text, hv) } else { t.muted.alpha(0.6) };
            let right = self.text_right(c, self.f.mono, 11.0, row.right() - 12.0, cy, keys, t.muted.alpha(open)) - 10.0;
            self.text_fit(c, self.f.sans_med, 13.0, row.x + 12.0, cy, label, right - row.x - 12.0, ink.alpha(open));
            if hov {
                self.set_cursor(ponzik_window::Cursor::Hand);
            }
        }
        self.edit_menu = Some(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers { shift: false, ctrl: false, alt: false };
    const SHIFT: Modifiers = Modifiers { shift: true, ctrl: false, alt: false };
    const CTRL_SHIFT: Modifiers = Modifiers { shift: true, ctrl: true, alt: false };

    fn run(text: &str, caret: usize, keys: &[(Key, Modifiers)], typed: &str, paste: Option<&str>) -> (String, EditState, Edited) {
        let mut s = text.to_string();
        let mut st = EditState { caret, anchor: caret, ..EditState::default() };
        let out = st.apply(&mut s, keys, typed, paste, EditOpts::default());
        (s, st, out)
    }

    #[test]
    fn typing_inserts_at_the_caret_and_replaces_a_selection() {
        let (s, st, out) = run("Kick", 2, &[], "x", None);
        assert_eq!((s.as_str(), st.caret, out.changed), ("Kixck", 3, true));
        let mut s = "Snare 01".to_string();
        let mut st = EditState { caret: 8, anchor: 6, ..Default::default() };
        st.apply(&mut s, &[], "é2", None, EditOpts::default());
        assert_eq!(s, "Snare é2");
        assert_eq!(st.caret, s.len());
    }

    #[test]
    fn arrows_move_and_shift_selects() {
        let (_, st, _) = run("héllo", 4, &[(Key::Left, NONE), (Key::Left, NONE)], "", None);
        assert_eq!(st.caret, 1, "back over the two-byte é");
        let (s, st, _) = run("héllo", 6, &[(Key::Left, SHIFT), (Key::Left, SHIFT)], "", None);
        assert_eq!(st.selected(&s), "lo");
        // Without Shift, a selection collapses to its edge.
        let mut st2 = st.clone();
        st2.apply(&mut s.clone(), &[(Key::Left, NONE)], "", None, EditOpts::default());
        assert_eq!((st2.caret, st2.has_selection()), (4, false));
        let (s, st, _) = run("one two", 7, &[(Key::Home, SHIFT)], "", None);
        assert_eq!(st.selected(&s), "one two");
        let (_, st, _) = run("one two", 0, &[(Key::End, NONE)], "", None);
        assert_eq!(st.caret, 7);
    }

    #[test]
    fn ctrl_moves_and_deletes_by_word() {
        let text = "Lead synth  (wide)";
        assert_eq!(word_left(text, text.len()), 17, "the closing parenthesis");
        assert_eq!(word_left(text, 17), 13);
        assert_eq!(word_left(text, 12), 5);
        assert_eq!(word_left(text, 5), 0);
        assert_eq!(word_right(text, 0), 4);
        assert_eq!(word_right(text, 4), 10);
        assert_eq!(word_right(text, 10), 13);
        let (s, st, _) = run("Lead synth", 10, &[(Key::Backspace, CTRL)], "", None);
        assert_eq!((s.as_str(), st.caret), ("Lead ", 5));
        let (s, _, _) = run("Lead synth", 0, &[(Key::Delete, CTRL)], "", None);
        assert_eq!(s, " synth");
        let (_, st, _) = run("Lead synth", 0, &[(Key::Right, CTRL_SHIFT)], "", None);
        assert_eq!(st.range(), (0, 4));
        assert_eq!(word_at("Lead synth", 6), (5, 10));
        assert_eq!(word_at("Lead synth", 4), (4, 5), "the space between");
        assert_eq!(word_at("Lead", 4), (0, 4), "after the end: the last word");
        assert_eq!(word_at("", 0), (0, 0));
    }

    #[test]
    fn select_all_copy_cut_and_paste() {
        let (s, st, out) = run("Bass", 2, &[(Key::Char('a'), CTRL), (Key::Char('c'), CTRL)], "", None);
        assert_eq!((st.range(), out.copy.as_deref(), out.changed), ((0, 4), Some("Bass"), false));
        assert_eq!(s, "Bass");
        let (s, _, out) = run("Bass", 0, &[(Key::Char('a'), CTRL), (Key::Char('x'), CTRL)], "", None);
        assert_eq!((s.as_str(), out.copy.as_deref()), ("", Some("Bass")));
        // Paste replaces the selection, on one line.
        let (s, st, out) = run("Bass", 4, &[(Key::Char('a'), CTRL), (Key::Char('v'), CTRL)], "", Some("Sub\r\nbass\t2\u{7}\n"));
        assert_eq!((s.as_str(), st.caret, out.changed), ("Sub bass 2", 10, true));
        // Nothing selected: Ctrl+C copies nothing; no clipboard text: Ctrl+V does nothing.
        let (s, _, out) = run("Bass", 4, &[(Key::Char('c'), CTRL), (Key::Char('v'), CTRL)], "", None);
        assert_eq!((s.as_str(), out.copy, out.changed), ("Bass", None, false));
    }

    #[test]
    fn unused_keys_go_back_to_the_caller() {
        let keys = [(Key::Enter, NONE), (Key::Escape, NONE), (Key::Down, NONE), (Key::Char('z'), CTRL), (Key::Char('k'), NONE)];
        let (_, _, out) = run("x", 1, &keys, "k", None);
        assert!(out.enter && out.escape);
        assert_eq!(out.rest, keys[..4].to_vec());
        // Backspace with nothing before the caret: left for the caller (a folder up).
        let (_, _, out) = run("", 0, &[(Key::Backspace, NONE)], "", None);
        assert_eq!(out.rest, vec![(Key::Backspace, NONE)]);
        // Home and End can stay with a list.
        let mut s = "abc".to_string();
        let mut st = EditState { caret: 1, anchor: 1, ..Default::default() };
        let out = st.apply(&mut s, &[(Key::Home, NONE), (Key::End, SHIFT)], "", None, EditOpts { keep_home_end: true });
        assert_eq!(out.rest, vec![(Key::Home, NONE)]);
        assert_eq!(st.range(), (1, 3));
    }

    #[test]
    fn pasted_text_stays_on_one_line() {
        assert_eq!(single_line("a\nb\r\nc\rd\te\u{0}f\n\n"), "a b c d ef");
        assert_eq!(single_line("plain"), "plain");
    }

    #[test]
    fn positions_follow_a_text_changed_elsewhere() {
        let mut st = EditState { caret: 9, anchor: 3, ..Default::default() };
        st.clamp("é");
        assert_eq!((st.caret, st.anchor), (2, 2));
        st.anchor = 1;
        st.clamp("é");
        assert_eq!(st.anchor, 0, "back to a character boundary");
    }

    // --- In the app -----------------------------------------------------------

    use super::super::app::{Popup, Rename, View};

    struct Ui {
        app: App,
        c: Canvas,
        i: Input,
    }

    impl Ui {
        fn new() -> Self {
            let project = ponzik_project::demo_project();
            let none = |_: &str| None;
            let (_engine, handle) = project.build_engine(48_000.0, &none);
            let mut ui = Self { app: App::new(handle, project, 48_000), c: Canvas::new(1600, 960), i: Input::default() };
            ui.i.inside = true;
            ui.frame();
            ui
        }

        fn frame(&mut self) {
            self.app.frame(&mut self.c, &self.i, 1.0 / 60.0);
            self.i.keys.clear();
            self.i.text.clear();
            (self.i.pressed, self.i.right_pressed, self.i.middle_pressed, self.i.double, self.i.released) =
                (false, false, false, false, false);
        }

        fn keys(&mut self, keys: &[(Key, Modifiers)]) {
            self.i.keys.extend_from_slice(keys);
            self.frame();
        }

        fn type_text(&mut self, s: &str) {
            self.i.text.push_str(s);
            self.frame();
        }

        fn click(&mut self, x: f32, y: f32) {
            (self.i.x, self.i.y, self.i.pressed, self.i.down) = (x, y, true, true);
            self.frame();
            (self.i.down, self.i.released) = (false, true);
            self.frame();
        }

        fn right_click(&mut self, x: f32, y: f32) {
            (self.i.x, self.i.y, self.i.right_pressed) = (x, y, true);
            self.frame();
        }

        /// Clicks the row `i` of the open text menu.
        fn menu_row(&mut self, i: usize) {
            let m = self.app.edit_menu.clone().expect("the text menu is open");
            let r = menu_row_rect(&m, i);
            self.click(r.x + 20.0, r.y + r.h * 0.5);
        }

        fn rename_text(&self) -> String {
            match &self.app.popup.as_ref().expect("the rename field is open").kind {
                Popup::Rename { text, .. } => text.clone(),
                _ => panic!("not the rename field"),
            }
        }

        fn open_rename(&mut self, text: &str) {
            self.app.open_popup(Popup::Rename { target: Rename::Channel(0), text: text.into() }, Rect::default());
            for _ in 0..20 {
                self.frame();
            }
        }

        /// Where the rename field's text starts, and its middle.
        fn field(&self) -> (f32, f32) {
            let open = self.app.anim.get(wid("popup", 0, 0)).unwrap_or(1.0);
            ((1600.0 - 360.0) * 0.5 + 28.0, 960.0 * 0.32 + (1.0 - open) * 8.0 + 40.0 + 19.0)
        }

        fn x_after(&self, s: &str) -> f32 {
            self.field().0 + self.app.fonts.measure(self.app.f.sans_med, 14.0, s)
        }
    }

    #[test]
    fn rename_field_selects_copies_cuts_and_pastes() {
        let mut ui = Ui::new();
        ui.open_rename("Kick drum");
        // The caret starts at the end: typing appends, as before.
        ui.type_text(" 2");
        assert_eq!(ui.rename_text(), "Kick drum 2");
        // Ctrl+Backspace drops the last word.
        ui.keys(&[(Key::Backspace, CTRL), (Key::Backspace, NONE)]);
        assert_eq!(ui.rename_text(), "Kick drum");
        // Shift+Home selects, Ctrl+C copies.
        ui.keys(&[(Key::Left, CTRL), (Key::Home, SHIFT)]);
        assert_eq!(ui.app.edit.selected("Kick drum"), "Kick ");
        ui.keys(&[(Key::Char('c'), CTRL)]);
        assert_eq!(ui.app.clip_text, "Kick ");
        // Ctrl+X cuts; at the end, Ctrl+V pastes.
        ui.keys(&[(Key::Char('x'), CTRL), (Key::End, NONE), (Key::Char('v'), CTRL)]);
        assert_eq!(ui.rename_text(), "drumKick ");
        ui.keys(&[(Key::Char('a'), CTRL)]);
        ui.type_text("Snare");
        assert_eq!(ui.rename_text(), "Snare");
        // Enter renames the channel, as one undoable step.
        ui.keys(&[(Key::Enter, NONE)]);
        assert!(ui.app.popup.is_none());
        assert_eq!(ui.app.project.channels[0].name, "Snare");
        ui.app.undo();
        assert_ne!(ui.app.project.channels[0].name, "Snare");
    }

    #[test]
    fn mouse_places_the_caret_selects_words_and_opens_the_menu() {
        let mut ui = Ui::new();
        ui.open_rename("Lead synth");
        let (x0, cy) = ui.field();
        // A click at the start of the text puts the caret there.
        ui.click(x0 - 2.0, cy);
        assert_eq!((ui.app.edit.caret, ui.app.edit.has_selection()), (0, false));
        ui.type_text("A ");
        assert_eq!(ui.rename_text(), "A Lead synth");
        // A double-click selects the word under the pointer.
        (ui.i.x, ui.i.y, ui.i.pressed, ui.i.double) = (ui.x_after("A Lead sy"), cy, true, true);
        ui.frame();
        assert_eq!(ui.app.edit.selected("A Lead synth"), "synth");
        // Dragging from the start selects up to the pointer.
        (ui.i.x, ui.i.pressed, ui.i.down) = (x0 - 2.0, true, true);
        ui.frame();
        ui.i.x = ui.x_after("A Lead");
        ui.frame();
        ui.i.down = false;
        ui.frame();
        assert_eq!(ui.app.edit.selected("A Lead synth"), "A Lead");

        // Right-click: Cut, Copy, Paste, Select all.
        ui.right_click(x0 + 20.0, cy);
        assert!(ui.app.edit_menu.is_some());
        ui.menu_row(1);
        assert_eq!(ui.app.clip_text, "A Lead");
        assert!(ui.app.edit_menu.is_none(), "a row closes the menu");
        assert!(ui.app.popup.is_some(), "the click stays in the menu");
        ui.right_click(x0 + 20.0, cy);
        ui.menu_row(3);
        assert_eq!(ui.app.edit.selected("A Lead synth"), "A Lead synth");
        ui.right_click(x0 + 20.0, cy);
        ui.menu_row(0);
        assert_eq!((ui.rename_text().as_str(), ui.app.clip_text.as_str()), ("", "A Lead synth"));
        ui.right_click(x0 + 20.0, cy);
        ui.menu_row(2);
        assert_eq!(ui.rename_text(), "A Lead synth", "Paste from the menu");
        // Escape closes the menu only, then the field.
        ui.right_click(x0 + 20.0, cy);
        ui.keys(&[(Key::Escape, NONE)]);
        assert!(ui.app.edit_menu.is_none() && ui.app.popup.is_some());
        ui.keys(&[(Key::Escape, NONE)]);
        assert!(ui.app.popup.is_none());
    }

    #[test]
    fn a_paste_stays_on_one_line() {
        let mut ui = Ui::new();
        ui.app.clip_text = "Bass\nline\t2\n".into();
        ui.open_rename("");
        ui.keys(&[(Key::Char('v'), CTRL)]);
        assert_eq!(ui.rename_text(), "Bass line 2");
    }

    #[test]
    fn palette_and_browser_search_edit_like_the_other_fields() {
        let mut ui = Ui::new();
        ui.app.open_palette();
        ui.frame();
        ui.type_text("mixer");
        ui.keys(&[(Key::Left, SHIFT), (Key::Left, SHIFT), (Key::Char('x'), CTRL)]);
        assert_eq!(ui.app.clip_text, "er");
        assert!(matches!(&ui.app.popup.as_ref().unwrap().kind, Popup::Palette { query, .. } if query == "mix"));
        ui.keys(&[(Key::Escape, NONE)]);
        assert!(ui.app.popup.is_none());

        // The browser's search, focused as a click on it does.
        ui.app.sidebar_focus = true;
        ui.frame();
        ui.keys(&[(Key::Char('v'), CTRL)]);
        assert_eq!(ui.app.sidebar_query, "er");
        ui.type_text(" kick");
        ui.keys(&[(Key::Home, NONE), (Key::Delete, CTRL)]);
        assert_eq!(ui.app.sidebar_query, " kick");
    }

    #[test]
    fn ctrl_c_outside_text_fields_still_copies_notes() {
        let mut ui = Ui::new();
        ui.app.set_view(View::Piano);
        ui.frame();
        ui.app.clip_text = "untouched".into();
        let ch = ui.app.piano_channel().expect("a channel in the piano roll");
        assert!(!ui.app.notes(ch).is_empty(), "the demo has notes");
        ui.app.piano.selected = vec![0];
        ui.keys(&[(Key::Char('c'), CTRL)]);
        assert_eq!(ui.app.clipboard_notes.len(), 1);
        assert_eq!(ui.app.clip_text, "untouched");
    }
}
