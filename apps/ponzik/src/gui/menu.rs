//! Menus: rows with an icon, a name and a value, and cascading submenus.
//!
//! A row made with [`MenuItem::sub`] (its items given) or [`MenuItem::sub_of`]
//! (the items of another popup, built only while it is open) shows a chevron
//! and opens a child menu beside its parent: on the right, or on the left when
//! there is no room (the main menu, top right), clamped to the screen. Hovering
//! opens it after a short intent delay; a safe triangle between the pointer and
//! the child keeps it open while the pointer travels diagonally towards it over
//! other rows. Keyboard: Up and Down move, Right or Enter opens, Left or Escape
//! closes the child, Enter runs a row. Two levels at most.

use ponzik_i18n::tr;
use ponzik_ui::{Canvas, Rect};
use ponzik_window::Key;

use super::Input;
use super::app::{Action, App, MenuItem, Popup, PopupState};
use super::keymap::Shortcut as S;
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};

/// Hover time on a row before its submenu opens (or the open one closes).
pub const INTENT_DELAY: f32 = 0.12;
/// A pointer still for this long is no longer heading for the submenu.
const AIM_STILL: f32 = 0.2;
/// Gap between a menu and its submenu.
const SUB_GAP: f32 = 4.0;
/// Margin kept from the screen edges.
const EDGE: f32 = 12.0;
/// Height of a group header row ("Audio").
const HEADER_H: f32 = 28.0;

/// The child menu of a row.
#[derive(Clone)]
pub enum Sub {
    Items(Vec<MenuItem>),
    /// The rows of this popup's menu, computed when the submenu shows.
    Of(Popup),
}

/// Navigation state of an open menu: keyboard selection, open submenu,
/// hover intent and safe triangle.
#[derive(Clone, Debug, Default)]
pub struct MenuNav {
    /// Selected row of the menu (keyboard, or the last row hovered).
    pub sel: Option<usize>,
    /// Row whose submenu is open.
    pub open: Option<usize>,
    /// Selected row of the submenu.
    pub sub_sel: Option<usize>,
    /// The keyboard is in the submenu.
    pub in_sub: bool,
    /// Row under the pointer, and since when (intent delay).
    hover: Option<usize>,
    hover_t: f32,
    /// Apex of the safe triangle: where the pointer was when it last moved
    /// other than towards the submenu.
    apex: (f32, f32),
    last: (f32, f32),
    moved_t: f32,
    /// The pointer moved since the last key: hovering selects and opens.
    mouse: bool,
    /// A submenu to open, by row id, at the next frame (captures, lessons).
    pub want: Option<&'static str>,
    /// The lesson or tour already opened its submenu once.
    pub guided: bool,
    /// Where the rows of the menu and of its submenu were drawn (tests).
    pub rows: Vec<Rect>,
    pub sub_rows: Vec<Rect>,
}

impl MenuNav {
    /// Opens the submenu of row `i`; the keyboard goes into it if `focus`.
    pub fn open_sub(&mut self, i: usize, focus: bool) {
        self.open = Some(i);
        self.sel = Some(i);
        self.sub_sel = None;
        self.in_sub = focus;
    }

    pub fn close_sub(&mut self) {
        self.open = None;
        self.sub_sel = None;
        self.in_sub = false;
    }
}

/// What a row is for the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowKind {
    pub selectable: bool,
    pub has_sub: bool,
}

impl RowKind {
    pub fn of(it: &MenuItem) -> Self {
        Self { selectable: selectable(it), has_sub: it.sub.is_some() }
    }
}

/// A row that does something: opens a submenu or runs an action (not a header,
/// a line of text or a "nothing here" row).
pub fn selectable(it: &MenuItem) -> bool {
    !it.header && (it.sub.is_some() || !matches!(it.action, Action::None))
}

/// What a key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavOut {
    None,
    /// Run the action of a row: (row, in the submenu).
    Run(usize, bool),
}

/// The next selectable row from `from` going `step` (±1), wrapping around.
fn step_row(rows: &[RowKind], from: Option<usize>, step: i32) -> Option<usize> {
    let n = rows.len() as i32;
    if n == 0 {
        return None;
    }
    let mut i = match from {
        Some(i) => i as i32,
        None if step > 0 => -1,
        None => n,
    };
    for _ in 0..n {
        i = (i + step).rem_euclid(n);
        if rows[i as usize].selectable {
            return Some(i as usize);
        }
    }
    None
}

/// A key pressed in an open menu. `child(i)` gives the rows of row `i`'s
/// submenu. Escape at the first level is left to the caller (it closes the menu).
pub fn nav_key(nav: &mut MenuNav, key: Key, rows: &[RowKind], child: &dyn Fn(usize) -> Vec<RowKind>) -> NavOut {
    let sub_rows = nav.open.map(child).unwrap_or_default();
    if nav.in_sub && nav.open.is_some() {
        match key {
            Key::Up | Key::Down => {
                let step = if key == Key::Up { -1 } else { 1 };
                nav.sub_sel = step_row(&sub_rows, nav.sub_sel, step).or(nav.sub_sel);
            }
            Key::Left | Key::Escape => {
                nav.sel = nav.open;
                nav.close_sub();
            }
            Key::Enter => {
                if let Some(j) = nav.sub_sel.filter(|&j| sub_rows.get(j).is_some_and(|r| r.selectable)) {
                    return NavOut::Run(j, true);
                }
            }
            Key::Home => nav.sub_sel = step_row(&sub_rows, None, 1),
            Key::End => nav.sub_sel = step_row(&sub_rows, None, -1),
            _ => {}
        }
        return NavOut::None;
    }
    match key {
        Key::Up | Key::Down => {
            let step = if key == Key::Up { -1 } else { 1 };
            nav.sel = step_row(rows, nav.sel, step).or(nav.sel);
            if nav.open != nav.sel {
                nav.close_sub();
            }
        }
        Key::Home => nav.sel = step_row(rows, None, 1),
        Key::End => nav.sel = step_row(rows, None, -1),
        Key::Right | Key::Enter => {
            let Some(i) = nav.sel.filter(|&i| rows.get(i).is_some_and(|r| r.selectable)) else { return NavOut::None };
            if rows[i].has_sub {
                nav.open_sub(i, true);
                nav.sub_sel = step_row(&child(i), None, 1);
            } else if key == Key::Enter {
                return NavOut::Run(i, false);
            }
        }
        Key::Left | Key::Escape => nav.close_sub(),
        _ => {}
    }
    NavOut::None
}

/// Place of a menu `w`×`h` opened from `anchor`: below it, pushed left of the
/// screen's right edge, above it when there is no room below.
pub fn menu_rect(anchor: Rect, w: f32, h: f32, screen: Rect) -> Rect {
    let mut x = anchor.x;
    let mut y = anchor.bottom() + 8.0;
    if x + w > screen.right() - EDGE {
        x = screen.right() - EDGE - w;
    }
    if y + h > screen.bottom() - EDGE {
        y = (anchor.y - 8.0 - h).max(TOP_BAR);
    }
    Rect::new(x.max(EDGE), y, w, h)
}

/// Place of a submenu `w`×`h` opened from `row` of the menu `parent`: on the
/// right, its first row level with `row` (`lead`: height of the group title
/// above it, if any); on the left when the right has no room (then the side
/// with more room, clamped). True when on the left.
pub fn sub_rect(parent: Rect, row: Rect, lead: f32, w: f32, h: f32, screen: Rect) -> (Rect, bool) {
    let right_x = parent.right() + SUB_GAP;
    let left_x = parent.x - SUB_GAP - w;
    let (x, left) = if right_x + w <= screen.right() - EDGE {
        (right_x, false)
    } else if left_x >= screen.x + EDGE {
        (left_x, true)
    } else {
        // No room on either side: the wider side, kept on screen.
        let left = parent.x - screen.x > screen.right() - parent.right();
        let x = if left { left_x } else { right_x };
        (x.clamp(screen.x + EDGE, (screen.right() - EDGE - w).max(screen.x + EDGE)), left)
    };
    let h = h.min(screen.h - 2.0 * EDGE);
    let y = (row.y - SUB_PAD - lead).min(screen.bottom() - EDGE - h).max(screen.y + EDGE);
    (Rect::new(x, y, w, h), left)
}

/// True when `p` is inside the triangle `a b c` (either winding).
fn in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let cross = |o: (f32, f32), u: (f32, f32), v: (f32, f32)| (u.0 - o.0) * (v.1 - o.1) - (u.1 - o.1) * (v.0 - o.0);
    let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// True when the pointer `p`, coming from `apex`, heads for the submenu `sub`
/// (on the left of its parent if `left`): inside the triangle from the apex
/// to the submenu's near edge.
pub fn aiming(apex: (f32, f32), p: (f32, f32), sub: Rect, left: bool) -> bool {
    if apex == p {
        return false;
    }
    let ex = if left { sub.right() } else { sub.x };
    in_triangle(p, apex, (ex, sub.y - 8.0), (ex, sub.bottom() + 8.0))
}

/// Top padding of a submenu (no title).
const SUB_PAD: f32 = 6.0;

/// Height of a row as laid out (headers are shorter).
fn row_h(it: &MenuItem) -> f32 {
    if it.header { HEADER_H } else { it.height }
}

/// Rows of a menu drawn in `r`: their rectangles, scrolled.
fn layout(r: Rect, top: f32, items: &[MenuItem], scroll: f32) -> Vec<Rect> {
    let mut ry = r.y + top - scroll;
    items
        .iter()
        .map(|it| {
            if it.sep {
                ry += 9.0;
            }
            let row = Rect::new(r.x + 6.0, ry, r.w - 12.0, row_h(it) - 4.0);
            ry += row_h(it);
            row
        })
        .collect()
}

// --- The main menu -----------------------------------------------------------

/// A format of the Import submenu. To offer a new importer, add it to
/// [`import_formats`]: its row runs `action` (usually a popup listing the
/// files found, or the Open dialog).
pub struct ImportFormat {
    pub icon: Icon,
    /// Translated name ("MIDI file").
    pub name: &'static str,
    pub tip: &'static str,
    pub action: Action,
}

/// The rows of Import, in order.
pub fn import_formats() -> Vec<ImportFormat> {
    vec![
        ImportFormat {
            icon: Icon::Piano,
            name: tr("MIDI file"),
            tip: tr("Its notes in a new pattern"),
            action: Action::ImportMidiMenu,
        },
        ImportFormat {
            icon: Icon::Rack,
            name: tr("FL Studio project"),
            tip: tr("Opens an .flp as a new project: patterns, playlist, mixer"),
            action: Action::ImportFlpMenu,
        },
        ImportFormat {
            icon: Icon::Session,
            name: tr("Ableton Live set"),
            tip: tr("Opens an .als as a new project: MIDI tracks, tempo, markers"),
            action: Action::ImportAlsMenu,
        },
        ImportFormat {
            icon: Icon::Folder,
            name: tr("REAPER, LMMS, Ardour, MusicXML…"),
            tip: tr("Pick the file: it opens as a new project, with a report of what came across"),
            action: Action::OpenMenu,
        },
    ]
}

/// Rows grouped under their category, in order of first appearance: a
/// submenu per category (its count on the right); the categories of a single
/// row are not worth a submenu, their rows come last, inline.
pub fn by_category(rows: Vec<(&'static str, MenuItem)>, icon: fn(&str) -> Icon) -> Vec<MenuItem> {
    let mut cats: Vec<(&'static str, Vec<MenuItem>)> = Vec::new();
    for (cat, it) in rows {
        match cats.iter_mut().find(|(c, _)| *c == cat) {
            Some((_, v)) => v.push(it),
            None => cats.push((cat, vec![it])),
        }
    }
    let (groups, single): (Vec<_>, Vec<_>) = cats.into_iter().partition(|(_, v)| v.len() > 1);
    let mut out: Vec<MenuItem> = groups
        .into_iter()
        .map(|(cat, v)| MenuItem::sub(icon(cat), ponzik_i18n::tr(cat), v.clone()).detail(v.len().to_string()))
        .collect();
    let grouped = !out.is_empty();
    for (k, (_, v)) in single.into_iter().enumerate() {
        out.extend(v.into_iter().map(|it| if k == 0 && grouped { it.sep() } else { it }));
    }
    out
}

/// Icon of an instrument or effect category of the registry.
pub fn category_icon(cat: &str) -> Icon {
    match cat {
        "Synths" => Icon::Wand,
        "Drums" => Icon::Drum,
        "Samplers" => Icon::Sample,
        "Bass" => Icon::Bass,
        "Keys" => Icon::Piano,
        "Acoustic" => Icon::Instrument,
        "External" => Icon::Midi,
        "EQ & Filters" => Icon::Spectral,
        "Dynamics" => Icon::Normalize,
        "Space" => Icon::Send,
        "Modulation" => Icon::Automation,
        "Saturation" => Icon::Effect,
        "Utilities" => Icon::Layout,
        "Pitch" => Icon::Voice,
        "Rhythm" => Icon::Metronome,
        "Mixing" => Icon::Mixer,
        _ => Icon::Effect,
    }
}

/// The native effects (those `keep` accepts) by category, a row running `action(id)`.
pub fn effect_items(keep: impl Fn(&str) -> bool, action: impl Fn(&'static str) -> Action) -> Vec<MenuItem> {
    let rows = ponzik_engine::registry::effects()
        .iter()
        .filter(|k| keep(k.id))
        .map(|k| (k.category, MenuItem::new(Icon::Effect, tr(k.name), action(k.id)).tip(tr(k.description))))
        .collect();
    by_category(rows, category_icon)
}

/// The native instruments by category; `replace`: the channel whose instrument they replace.
pub fn instrument_items(replace: Option<usize>) -> Vec<MenuItem> {
    let rows = ponzik_engine::registry::instruments()
        .iter()
        .map(|k| {
            (
                k.category,
                MenuItem::new(Icon::Instrument, tr(k.name), Action::AddInstrument(k.id, replace))
                    .tip(tr(k.description)),
            )
        })
        .collect();
    by_category(rows, category_icon)
}

impl App {
    /// The main menu (☰, top right): files, Import and Export submenus, theme, help.
    pub(super) fn main_menu(&self) -> (String, Vec<MenuItem>) {
        let key = |s: S| self.key_label(s);
        let (done, total) = self.lessons.count();
        let import = import_formats().into_iter().map(|f| MenuItem::new(f.icon, f.name, f.action).tip(f.tip)).collect();
        let v = vec![
            MenuItem::new(Icon::Plus, tr("New"), Action::NewProject).detail(key(S::NewProject)),
            MenuItem::new(Icon::Folder, tr("Open"), Action::OpenMenu).detail(key(S::Open)),
            MenuItem::sub_of(Icon::History, tr("Open recent"), Popup::Recent).id("recent"),
            MenuItem::new(Icon::Save, tr("Save"), Action::Save).detail(key(S::Save)),
            MenuItem::new(Icon::Pencil, tr("Save as"), Action::SaveAs).detail(key(S::SaveAs)),
            MenuItem::sub(Icon::Import, tr("Import"), import).id("import").sep(),
            MenuItem::sub(Icon::Export, tr("Export"), self.export_items()).id("export"),
            MenuItem::sub_of(Icon::Theme, tr("Theme"), Popup::Themes)
                .detail(self.theme_label(self.themes.active))
                .id("theme")
                .sep(),
            MenuItem::new(Icon::Layout, tr("Settings"), Action::Popup(Popup::Settings { tab: 0 }))
                .detail(key(S::Settings))
                .tip(tr("Views, panels and shortcuts")),
            MenuItem::new(Icon::Learn, tr("Learn"), Action::Lessons(None))
                .detail(format!("{done}/{total}"))
                .tip(tr("Short hands-on lessons: the basics, then your first tracks")),
            MenuItem::new(Icon::Info, tr("What's new"), Action::Update(super::updates::UpdateOp::ShowNews))
                .tip(tr("The changes of this version")),
            MenuItem::new(Icon::Reset, tr("About and updates"), Action::Popup(Popup::Settings { tab: 5 }))
                .detail(self.updates_badge())
                .tip(tr("Version, build and update settings")),
        ];
        ("Ponzik".into(), v)
    }

    /// The Export submenu: audio files, then the project for another app.
    pub(super) fn export_items(&self) -> Vec<MenuItem> {
        use super::filedialog::Format;
        vec![
            MenuItem::header(tr("Audio")),
            MenuItem::new(Icon::Export, "WAV", Action::Export)
                .detail(self.key_label(S::Export))
                .id("export-wav")
                .tip(tr("The song, start to end")),
            MenuItem::new(Icon::Export, "FLAC", Action::ExportWith(ponzik_project::ExportOptions::flac()))
                .id("export-flac")
                .tip(tr("Lossless, 24 bits, about half the size of a WAV")),
            MenuItem::new(Icon::Columns, tr("Stems"), Action::ExportStems(Default::default()))
                .tip(tr("One WAV per mixer track")),
            MenuItem::new(Icon::Columns, tr("Sections"), Action::ExportSections(Default::default()))
                .tip(tr("One WAV per arrangement section: \"<project> - <section>.wav\"")),
            MenuItem::header(tr("Project for another app")).sep(),
            MenuItem::new(Icon::Rack, "FL Studio", Action::SaveCopy(Format::Flp))
                .detail(".flp".into())
                .tip(tr("An .flp copy: patterns, notes, playlist, mixer levels")),
            MenuItem::new(Icon::Session, "Ableton Live", Action::SaveCopy(Format::Als))
                .detail(".als".into())
                .tip(tr("An .als copy: MIDI tracks with their notes, tempo, markers")),
            MenuItem::new(Icon::Columns, "REAPER", Action::SaveCopy(Format::Rpp))
                .detail(".rpp".into())
                .tip(tr("An .rpp copy: MIDI and audio items, tempo changes, markers")),
            MenuItem::new(Icon::Piano, "MIDI", Action::ExportMidi).detail(".mid".into()).tip(tr("The current pattern")),
        ]
    }

    /// Open recent: the last projects, most recent first, and "Clear the list".
    pub(super) fn recent_items(&self) -> Vec<MenuItem> {
        if self.recent_files.is_empty() {
            return vec![MenuItem::new(Icon::History, tr("No recent projects"), Action::None).dim(true)];
        }
        let mut v: Vec<MenuItem> = self
            .recent_files
            .iter()
            .map(|path| {
                let name = path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
                let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                let kind = super::filedialog::Kind::of(path, false);
                MenuItem::new(kind.icon(), &name, Action::Open(path.clone()))
                    .detail(if modified.is_some() {
                        super::filedialog::age_label(modified)
                    } else {
                        tr("missing").into()
                    })
                    .dim(modified.is_none())
                    .tip(&path.display().to_string())
            })
            .collect();
        v.push(MenuItem::new(Icon::Close, tr("Clear the list"), Action::ClearRecent).sep());
        v
    }
}

/// What a menu panel saw under the pointer.
struct Hit {
    /// Hoverable row under the pointer.
    row: Option<usize>,
}

impl App {
    /// The row of the menus a lesson or the tour points at: the WAV export.
    fn menu_guide(&self) -> Option<&'static str> {
        (self.tour.step == Some(2) || self.lessons.wants_export()).then_some("export-wav")
    }

    /// The rows of a submenu.
    fn sub_items(&self, it: &MenuItem) -> Vec<MenuItem> {
        let mut v = match &it.sub {
            Some(Sub::Items(items)) => items.clone(),
            Some(Sub::Of(kind)) => self.menu_items(kind).1,
            None => Vec::new(),
        };
        for it in &mut v {
            it.height = self.theme.dense(it.height);
        }
        v
    }

    /// Width and height of a menu: wide enough for its longest name and value.
    fn menu_size(&self, items: &[MenuItem], top: f32, screen: Rect) -> (f32, f32, f32) {
        let widest = items
            .iter()
            .map(|it| {
                let chevron = if it.sub.is_some() { 18.0 } else { 0.0 };
                self.fonts.measure(self.f.sans_med, 13.0, &it.name)
                    + self.fonts.measure(self.f.mono, 11.0, &it.detail)
                    + chevron
            })
            .fold(0.0f32, f32::max);
        let w = (widest + 84.0).clamp(if top > SUB_PAD { 260.0 } else { 220.0 }, 360.0);
        let seps = items.iter().filter(|i| i.sep).count() as f32 * 9.0;
        let rows_h: f32 = items.iter().map(row_h).sum();
        let content = rows_h + seps;
        let h = (top + content + 6.0).min(screen.h - 80.0);
        (w, h, content)
    }

    /// Wheel scrolling of a menu longer than the screen.
    fn menu_scroll(&mut self, input: &Input, key: u64, r: Rect, top: f32, content: f32) -> f32 {
        let list_h = r.h - top - 2.0;
        let mut scroll = self.anim.get(key).unwrap_or(0.0);
        if input.inside && r.contains(input.x, input.y) && input.wheel != 0.0 {
            scroll -= input.wheel * 60.0;
        }
        scroll = scroll.clamp(0.0, (content - list_h).max(0.0));
        self.anim.set(key, scroll);
        scroll
    }

    /// The hoverable row of `rows` under the pointer, inside the list area.
    fn menu_hit(input: &Input, items: &[MenuItem], rows: &[Rect], list: Rect, dense38: f32) -> Hit {
        let p_in = input.inside && list.contains(input.x, input.y);
        let row = (0..items.len()).find(|&i| {
            let it = &items[i];
            let line = it.height < dense38 && matches!(it.action, Action::None) && it.sub.is_none();
            p_in && !line && !it.header && rows[i].contains(input.x, input.y)
        });
        Hit { row }
    }

    /// Draws a menu panel: card, title, rows. `hl`: the highlighted rows
    /// (selected, open submenu). Returns the tip of the row under the pointer.
    #[allow(clippy::too_many_arguments)]
    fn menu_panel(
        &mut self,
        c: &mut Canvas,
        level: usize,
        r: Rect,
        title: &str,
        items: &[MenuItem],
        rows: &[Rect],
        hl: [Option<usize>; 2],
        hit: Option<usize>,
        guide: Option<&'static str>,
        alpha: f32,
    ) -> Option<String> {
        let t = self.theme;
        let radius = 14.0;
        c.fill_rrect(r.translate(0.0, 6.0).inset(-2.0), radius + 2.0, t.shade(90.0 * alpha));
        c.fill_rrect(r, radius, t.panel.alpha(alpha));
        c.stroke_rrect(r, radius, 1.0, t.hairline.alpha(1.6 * alpha));
        let top = if title.is_empty() { SUB_PAD } else { 38.0 };
        if !title.is_empty() {
            self.text_fit(c, self.f.sans_med, 12.0, r.x + 16.0, r.y + 20.0, title, r.w - 32.0, t.muted.alpha(alpha));
        }
        let list = Rect::new(r.x, r.y + top - 2.0, r.w, r.h - top);
        let prev = c.push_clip(list);
        let has_icons = items.iter().any(|it| it.icon.is_some());
        let mut tip = None;
        let breath = 0.5 + 0.5 * (self.time * 3.0).sin();
        for (i, it) in items.iter().enumerate() {
            let row = rows[i];
            if it.sep {
                c.fill_rect(Rect::new(r.x + 12.0, row.y - 5.0, r.w - 24.0, 1.0), t.hairline.alpha(alpha));
            }
            if row.bottom() < list.y || row.y > list.bottom() {
                continue;
            }
            let cy = row.y + row.h * 0.5;
            if it.header {
                self.text_fit(
                    c,
                    self.f.sans_med,
                    11.0,
                    row.x + 12.0,
                    cy + 3.0,
                    &it.name,
                    row.w - 24.0,
                    t.muted.alpha(alpha),
                );
                continue;
            }
            let id = wid("popup-row", i, level);
            let lit = hl.contains(&Some(i));
            let hv = self.hover_anim(id, lit);
            let current = it.on;
            if hv > 0.01 || current {
                c.fill_rrect(
                    row,
                    9.0,
                    t.raised.mix(t.raised_hi, hv * 0.6).alpha(hv.max(current as u8 as f32 * 0.7) * alpha),
                );
            }
            let guided = guide.is_some_and(|g| {
                it.id == g || it.sub.as_ref().is_some_and(|s| matches!(s, Sub::Items(v) if v.iter().any(|x| x.id == g)))
            });
            if guided {
                c.stroke_rrect(row.inset(-1.0), 10.0, 2.0, t.accent.alpha((0.55 + 0.4 * breath) * alpha));
                self.anim.set(wid("menu-guide", 0, 0), self.time);
            }
            let danger = it.icon == Some(Icon::Trash);
            let ink = if danger {
                t.negative
            } else if it.dim {
                t.muted.mix(t.text2, hv)
            } else {
                t.text2.mix(t.text, hv.max(current as u8 as f32))
            };
            let mut tx = row.x + 12.0;
            if let Some(icon) = it.icon {
                draw_icon(
                    c,
                    icon,
                    row.x + 20.0,
                    cy,
                    15.0,
                    if danger { t.negative } else { t.muted.mix(t.accent, hv) }.alpha(alpha),
                );
                tx = row.x + 38.0;
            } else if let Some(col) = it.color
                && has_icons
            {
                // Parmi des lignes à icône : une pastille à la place de l'icône.
                c.fill_circle(row.x + 20.0, cy, 6.0, col.alpha(alpha));
                tx = row.x + 38.0;
            } else if let Some(col) = it.color {
                c.fill_rrect(Rect::new(row.x + 12.0, cy - 7.0, 4.0, 14.0), 2.0, col.alpha(alpha));
                tx = row.x + 26.0;
            }
            let mut right = row.right() - 12.0;
            if current && it.detail.is_empty() && it.sub.is_none() {
                // The current choice (a theme, an output): a check on the right.
                draw_icon(c, Icon::Check, right - 6.0, cy, 13.0, t.accent.alpha(alpha));
                right -= 22.0;
            }
            if it.sub.is_some() {
                draw_icon(c, Icon::ChevronRight, right - 4.0, cy, 12.0, t.muted.mix(t.text, hv).alpha(alpha));
                right -= 18.0;
            }
            if !it.detail.is_empty() {
                right = self.text_right(c, self.f.mono, 11.0, right, cy, &it.detail, t.muted.alpha(alpha)) - 10.0;
            }
            self.text_fit(c, self.f.sans_med, 13.0, tx, cy, &it.name, right - tx, ink.alpha(alpha));
            if hit == Some(i) {
                if !it.tip.is_empty() {
                    tip = Some(it.tip.clone());
                }
                self.set_cursor(ponzik_window::Cursor::Hand);
            }
        }
        c.set_clip(Some(prev));
        tip
    }

    /// A menu popup with its submenu. False when it closes.
    pub(super) fn menu_popup(&mut self, c: &mut Canvas, input: &Input, screen: Rect, state: &mut PopupState) -> bool {
        let t = self.theme;
        // Its fade, advanced by `popup_layer`.
        let open_a = self.anim.get(wid("popup", 0, 0)).unwrap_or(1.0);
        let (title, mut items) = self.menu_items(&state.kind);
        for it in &mut items {
            it.height = t.dense(it.height);
        }
        let dense38 = t.dense(38.0);
        let rows_k: Vec<RowKind> = items.iter().map(RowKind::of).collect();
        let nav = &mut state.nav;

        // A submenu asked for by id (a capture), or the one the lesson points at.
        let guide = self.menu_guide().filter(|_| matches!(state.kind, Popup::MainMenu));
        if let Some(g) = guide.filter(|_| !nav.guided && nav.open.is_none()) {
            nav.guided = true;
            nav.want = items
                .iter()
                .find(|it| matches!(&it.sub, Some(Sub::Items(v)) if v.iter().any(|x| x.id == g)))
                .map(|it| it.id);
        }
        if let Some(id) = nav.want.take()
            && let Some(i) = items.iter().position(|it| it.id == id && it.sub.is_some())
        {
            let focus = nav.in_sub;
            nav.open_sub(i, focus);
            self.anim.set(wid("popup-sub", 0, 0), 0.0);
        }

        // The menu.
        let top = 38.0;
        let (w, h, content) = self.menu_size(&items, top, screen);
        let base = menu_rect(state.anchor, w, h, screen);
        let r = base.translate(0.0, (1.0 - open_a) * 6.0);
        let scroll = self.menu_scroll(input, wid("popup-scroll", 0, 0), r, top, content);
        let rows = layout(r, top, &items, scroll);
        let list = Rect::new(r.x, r.y + top - 2.0, r.w, r.h - top);
        let hit = Self::menu_hit(input, &items, &rows, list, dense38);

        // Keyboard.
        let mut run: Option<(usize, bool)> = None;
        for &(key, _) in &input.keys {
            nav.mouse = false;
            let child = |i: usize| self.sub_items(&items[i]).iter().map(RowKind::of).collect::<Vec<_>>();
            let before = nav.open;
            if let NavOut::Run(i, sub) = nav_key(nav, key, &rows_k, &child) {
                run = Some((i, sub));
            }
            if nav.open != before && nav.open.is_some() {
                self.anim.set(wid("popup-sub", 0, 0), 0.0);
                self.anim.set(wid("popup-scroll", 1, 0), 0.0);
            }
        }

        // The submenu's place, from the row that opens it.
        let geom = |s: &Self, open: Option<usize>| -> Option<(Vec<MenuItem>, Rect, bool, f32)> {
            let i = open?;
            let it = items.get(i).filter(|it| it.sub.is_some())?;
            let sub = s.sub_items(it);
            let (sw, sh, sc) = s.menu_size(&sub, SUB_PAD, screen);
            let lead: f32 = sub.iter().take_while(|it| !selectable(it)).map(row_h).sum();
            let (sr, left) = sub_rect(base, rows[i], lead, sw, sh, screen);
            Some((sub, sr, left, sc))
        };
        let mut child = geom(self, nav.open);
        let child_for = nav.open;

        // Pointer: intent delay and safe triangle.
        let p = (input.x, input.y);
        let now = self.time;
        let moved = input.inside && p != nav.last;
        if moved {
            nav.moved_t = now;
        }
        nav.last = p;
        if moved || input.pressed {
            nav.mouse = true;
        }
        let in_child = child.as_ref().is_some_and(|(_, sr, _, _)| input.inside && sr.contains(p.0, p.1));
        let aim = !in_child
            && !input.pressed
            && now - nav.moved_t < AIM_STILL
            && child.as_ref().is_some_and(|(_, sr, left, _)| aiming(nav.apex, p, *sr, *left));
        if !aim {
            nav.apex = p;
        }
        let mut just_opened = false;
        if let Some(i) = hit.row.filter(|_| !aim && !in_child && nav.mouse) {
            if nav.hover != Some(i) {
                nav.hover = Some(i);
                nav.hover_t = now;
            }
            nav.sel = Some(i);
            nav.in_sub = false;
            let wants = items[i].sub.is_some();
            if nav.open != Some(i) && (wants || nav.open.is_some()) {
                if input.pressed || now - nav.hover_t >= INTENT_DELAY {
                    if wants {
                        nav.open_sub(i, false);
                        just_opened = true;
                    } else {
                        nav.close_sub();
                    }
                } else {
                    // Keeps frames coming until the delay is over.
                    self.anim.set(wid("menu-wait", 0, 0), now);
                }
            }
        } else if hit.row.is_none() {
            nav.hover = None;
        }
        if aim {
            self.anim.set(wid("menu-wait", 0, 0), now);
        }
        if nav.open != child_for {
            if nav.open.is_some() {
                self.anim.set(wid("popup-sub", 0, 0), 0.0);
                self.anim.set(wid("popup-scroll", 1, 0), 0.0);
            }
            child = geom(self, nav.open);
        }
        if child.is_none() && nav.open.is_some() {
            // A row without a submenu (a capture asked for the wrong one).
            nav.close_sub();
        }

        // The submenu's rows and the pointer over them.
        let sub_a = if child.is_some() { self.anim.to(wid("popup-sub", 0, 0), 1.0, T_HOVER) } else { 0.0 };
        let mut sub_rows = Vec::new();
        let mut sub_hit = None;
        let mut sub_r = None;
        if let Some((sub, sr, left, sc)) = &child {
            let slide = (1.0 - sub_a) * 8.0 * if *left { 1.0 } else { -1.0 };
            let sr = sr.translate(slide, 0.0);
            let scroll = self.menu_scroll(input, wid("popup-scroll", 1, 0), sr, SUB_PAD, *sc);
            sub_rows = layout(sr, SUB_PAD, sub, scroll);
            let list = Rect::new(sr.x, sr.y + SUB_PAD - 2.0, sr.w, sr.h - SUB_PAD);
            sub_hit = Self::menu_hit(input, sub, &sub_rows, list, dense38).row.filter(|_| !just_opened);
            if let Some(j) = sub_hit.filter(|_| nav.mouse) {
                nav.sub_sel = Some(j);
                nav.in_sub = true;
                nav.sel = nav.open;
            }
            if nav.in_sub && nav.sub_sel.is_none() {
                nav.sub_sel = (0..sub.len()).find(|&j| selectable(&sub[j]));
            }
            sub_r = Some(sr);
        }

        // Drawing: the menu, then its submenu above it.
        let hl = [nav.sel, nav.open];
        let mut tip = self.menu_panel(c, 0, r, &title, &items, &rows, hl, hit.row.filter(|_| !aim), guide, 1.0);
        if let (Some((sub, ..)), Some(sr)) = (&child, sub_r) {
            let hl = [if nav.in_sub { nav.sub_sel } else { None }, None];
            if let Some(tp) = self.menu_panel(c, 1, sr, "", sub, &sub_rows, hl, sub_hit, guide, sub_a) {
                tip = Some(tp);
            }
        }
        // Les menus gardent leurs infobulles : la description d'un instrument, un chemin.
        if let Some(tip) = tip {
            let side = sub_r.filter(|_| in_child).unwrap_or(r);
            let tw = self.fonts.measure(self.f.sans, 12.0, &tip).min(screen.w * 0.4);
            let mut tx = side.right() + 8.0;
            if tx + tw + 20.0 > screen.right() - 8.0 {
                // No room on the right: beside the left edge of the menus.
                let left = sub_r.map_or(side.x, |s| s.x.min(r.x));
                tx = (left - tw - 28.0).max(8.0);
            }
            let tip_r = Rect::new(tx, input.y - 14.0, tw + 20.0, 28.0);
            c.fill_rrect(tip_r, 8.0, t.raised_hi);
            c.stroke_rrect(tip_r, 8.0, 1.0, t.hairline.alpha(1.6));
            self.text_fit(c, self.f.sans, 12.0, tip_r.x + 10.0, tip_r.y + 14.0, &tip, tw, t.text2);
        }

        // Clicks.
        if input.pressed {
            if let Some(j) = sub_hit {
                run = Some((j, true));
            } else if let Some(i) = hit.row.filter(|&i| items[i].sub.is_none() && !aim) {
                run = Some((i, false));
            }
        }
        if let Some((i, in_sub)) = run {
            let picked = if in_sub {
                child.as_ref().and_then(|(sub, ..)| sub.get(i).map(|it| (it.action.clone(), sub_rows[i])))
            } else {
                items.get(i).map(|it| (it.action.clone(), rows[i]))
            };
            if let Some((action, row)) = picked {
                self.anim.set(wid("popup-scroll", 0, 0), 0.0);
                let at = sub_r.filter(|_| in_sub).unwrap_or(r);
                self.run_action(action, Rect::new(at.x, row.y, at.w, row.h));
                return false;
            }
        }
        nav.rows = rows;
        nav.sub_rows = sub_rows;
        let outside = |q: Rect| !q.contains(input.x, input.y);
        let clicked_outside = (input.pressed || input.right_pressed) && outside(r) && sub_r.is_none_or(outside);
        !clicked_outside
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect { x: 0.0, y: 0.0, w: 1600.0, h: 960.0 };

    fn rows(kinds: &[(bool, bool)]) -> Vec<RowKind> {
        kinds.iter().map(|&(selectable, has_sub)| RowKind { selectable, has_sub }).collect()
    }

    #[test]
    fn submenu_opens_on_the_right_when_there_is_room() {
        let parent = Rect::new(100.0, 60.0, 280.0, 400.0);
        let row = Rect::new(106.0, 200.0, 268.0, 34.0);
        let (r, left) = sub_rect(parent, row, 0.0, 240.0, 300.0, SCREEN);
        assert!(!left);
        assert_eq!(r.x, parent.right() + SUB_GAP);
        // Its first row level with the row that opened it, below a group title.
        assert_eq!(r.y, row.y - SUB_PAD);
        let (r, _) = sub_rect(parent, row, HEADER_H, 240.0, 300.0, SCREEN);
        assert_eq!(r.y, row.y - SUB_PAD - HEADER_H);
    }

    #[test]
    fn submenu_flips_left_at_the_right_edge() {
        // The main menu, top right.
        let parent = Rect::new(1600.0 - 12.0 - 280.0, 48.0, 280.0, 520.0);
        let row = Rect::new(parent.x + 6.0, 300.0, 268.0, 34.0);
        let (r, left) = sub_rect(parent, row, 0.0, 260.0, 320.0, SCREEN);
        assert!(left);
        assert_eq!(r.right(), parent.x - SUB_GAP);
        assert!(r.x >= EDGE);
    }

    #[test]
    fn submenu_is_clamped_to_the_screen() {
        // A tall submenu from a low row: pushed up to fit.
        let parent = Rect::new(200.0, 400.0, 280.0, 500.0);
        let row = Rect::new(206.0, 860.0, 268.0, 34.0);
        let (r, _) = sub_rect(parent, row, 0.0, 240.0, 400.0, SCREEN);
        assert!(r.bottom() <= SCREEN.bottom() - EDGE + 0.01);
        assert!(r.y >= EDGE);
        // Taller than the screen: as tall as it can be, from the top.
        let (r, _) = sub_rect(parent, row, 0.0, 240.0, 2000.0, SCREEN);
        assert_eq!(r.y, EDGE);
        assert!(r.bottom() <= SCREEN.bottom() - EDGE + 0.01);
        // No room on either side: on the wider side, inside the screen.
        let narrow = Rect { x: 0.0, y: 0.0, w: 500.0, h: 960.0 };
        let parent = Rect::new(150.0, 60.0, 280.0, 300.0);
        let (r, left) = sub_rect(parent, Rect::new(156.0, 100.0, 268.0, 34.0), 0.0, 240.0, 200.0, narrow);
        assert!(left, "more room on the left (150) than on the right (70)");
        assert!(r.x >= EDGE && r.right() <= narrow.right() - EDGE + 0.01);
    }

    #[test]
    fn menus_stay_under_their_anchor_and_on_screen() {
        let r = menu_rect(Rect::new(1556.0, 8.0, 32.0, 32.0), 280.0, 500.0, SCREEN);
        assert_eq!(r.right(), SCREEN.right() - EDGE);
        assert_eq!(r.y, 48.0);
        let r = menu_rect(Rect::new(400.0, 900.0, 40.0, 30.0), 280.0, 300.0, SCREEN);
        assert!(r.bottom() <= 900.0, "above the anchor when there is no room below");
    }

    #[test]
    fn safe_triangle_keeps_a_diagonal_move() {
        let sub = Rect::new(400.0, 100.0, 240.0, 300.0);
        // From a row on the left, going right and down towards the submenu.
        assert!(aiming((300.0, 120.0), (330.0, 150.0), sub, false));
        // Going away from it (left, or straight down past its edge).
        assert!(!aiming((300.0, 120.0), (280.0, 130.0), sub, false));
        assert!(!aiming((300.0, 120.0), (300.0, 500.0), sub, false));
        // A still pointer is not heading anywhere.
        assert!(!aiming((300.0, 120.0), (300.0, 120.0), sub, false));
        // Flipped to the left: heading left.
        let sub = Rect::new(100.0, 100.0, 240.0, 300.0);
        assert!(aiming((420.0, 130.0), (390.0, 170.0), sub, true));
        assert!(!aiming((420.0, 130.0), (450.0, 170.0), sub, true));
    }

    #[test]
    fn arrows_move_over_selectable_rows() {
        // A header, two rows, a dim line, a row.
        let r = rows(&[(false, false), (true, false), (true, true), (false, false), (true, false)]);
        let none = |_: usize| Vec::new();
        let mut nav = MenuNav::default();
        nav_key(&mut nav, Key::Down, &r, &none);
        assert_eq!(nav.sel, Some(1));
        nav_key(&mut nav, Key::Down, &r, &none);
        nav_key(&mut nav, Key::Down, &r, &none);
        assert_eq!(nav.sel, Some(4), "the line is skipped");
        nav_key(&mut nav, Key::Down, &r, &none);
        assert_eq!(nav.sel, Some(1), "wraps around");
        nav_key(&mut nav, Key::Up, &r, &none);
        assert_eq!(nav.sel, Some(4));
        assert_eq!(nav_key(&mut nav, Key::Enter, &r, &none), NavOut::Run(4, false));
    }

    #[test]
    fn right_and_enter_open_left_and_escape_close() {
        let r = rows(&[(true, false), (true, true), (true, false)]);
        let child = |_: usize| rows(&[(false, false), (true, false), (true, false)]);
        let mut nav = MenuNav { sel: Some(1), ..Default::default() };
        nav_key(&mut nav, Key::Right, &r, &child);
        assert_eq!((nav.open, nav.in_sub, nav.sub_sel), (Some(1), true, Some(1)), "first row after the header");
        nav_key(&mut nav, Key::Down, &r, &child);
        assert_eq!(nav.sub_sel, Some(2));
        assert_eq!(nav_key(&mut nav, Key::Enter, &r, &child), NavOut::Run(2, true));
        nav_key(&mut nav, Key::Left, &r, &child);
        assert_eq!((nav.open, nav.in_sub, nav.sel), (None, false, Some(1)), "back on the row that opened it");
        nav_key(&mut nav, Key::Enter, &r, &child);
        assert_eq!(nav.open, Some(1), "Enter opens too");
        nav_key(&mut nav, Key::Escape, &r, &child);
        assert_eq!(nav.open, None, "Escape closes the child only");
        assert_eq!(nav.sel, Some(1));
        // Enter on a row without a submenu runs it; Right does nothing there.
        nav.sel = Some(0);
        assert_eq!(nav_key(&mut nav, Key::Right, &r, &child), NavOut::None);
        assert_eq!(nav.open, None);
        assert_eq!(nav_key(&mut nav, Key::Enter, &r, &child), NavOut::Run(0, false));
    }

    #[test]
    fn categories_become_submenus_and_singletons_stay_inline() {
        let row = |n: &str| MenuItem::plain(n, Action::NewPattern);
        let v =
            by_category(vec![("A", row("a1")), ("B", row("b1")), ("A", row("a2")), ("C", row("c1"))], |_| Icon::Effect);
        let names: Vec<&str> = v.iter().map(|it| it.name.as_str()).collect();
        assert_eq!(names, ["A", "b1", "c1"]);
        assert!(matches!(&v[0].sub, Some(Sub::Items(items)) if items.len() == 2));
        assert_eq!(v[0].detail, "2");
        assert!(v[1].sep && !v[2].sep, "the lone rows after a rule");
    }

    #[test]
    fn effect_and_instrument_menus_are_short_and_complete() {
        let leaves = |v: &[MenuItem]| -> usize {
            v.iter()
                .map(|it| match &it.sub {
                    Some(Sub::Items(items)) => items.len(),
                    _ => 1,
                })
                .sum()
        };
        let fx = effect_items(|_| true, |_| Action::None);
        assert!(fx.len() <= 14, "{} rows at the first level", fx.len());
        assert_eq!(leaves(&fx), ponzik_engine::registry::effects().len());
        let inst = instrument_items(None);
        assert!(inst.len() <= 12, "{} rows at the first level", inst.len());
        assert_eq!(leaves(&inst), ponzik_engine::registry::instruments().len());
        // Two levels at most.
        for it in fx.iter().chain(&inst) {
            if let Some(Sub::Items(items)) = &it.sub {
                assert!(items.iter().all(|x| x.sub.is_none()));
            }
        }
    }

    // --- In the app: the main menu, top right -----------------------------------

    fn app() -> App {
        let project = ponzik_project::demo_project();
        let none = |_: &str| None;
        let (_engine, handle) = project.build_engine(48_000.0, &none);
        App::new(handle, project, 48_000)
    }

    /// The ☰ button of the top bar.
    const MENU_BUTTON: Rect = Rect { x: 1556.0, y: 8.0, w: 32.0, h: 32.0 };

    struct Ui {
        app: App,
        c: Canvas,
        i: Input,
    }

    impl Ui {
        fn new() -> Self {
            let mut ui = Self { app: app(), c: Canvas::new(1600, 960), i: Input::default() };
            ui.frame();
            ui.app.open_popup(Popup::MainMenu, MENU_BUTTON);
            ui.frame();
            ui
        }

        fn frame(&mut self) {
            self.app.frame(&mut self.c, &self.i, 1.0 / 60.0);
            self.i.keys.clear();
            self.i.pressed = false;
        }

        fn frames(&mut self, n: usize) {
            for _ in 0..n {
                self.frame();
            }
        }

        fn key(&mut self, k: Key) {
            self.i.keys.push((k, Default::default()));
            self.frame();
        }

        fn point(&mut self, x: f32, y: f32) {
            (self.i.x, self.i.y, self.i.inside) = (x, y, true);
            self.frame();
        }

        fn click(&mut self, x: f32, y: f32) {
            (self.i.x, self.i.y, self.i.inside, self.i.pressed) = (x, y, true, true);
            self.frame();
        }

        fn nav(&self) -> &MenuNav {
            &self.app.popup.as_ref().expect("the menu is open").nav
        }

        fn row(&self, id: &str) -> usize {
            self.app.main_menu().1.iter().position(|it| it.id == id).expect("row")
        }
    }

    #[test]
    fn keyboard_walks_into_the_export_submenu_and_back() {
        let mut ui = Ui::new();
        let export = ui.row("export");
        for _ in 0..=export {
            ui.key(Key::Down);
        }
        assert_eq!(ui.nav().sel, Some(export));
        ui.key(Key::Right);
        assert_eq!(ui.nav().open, Some(export));
        assert!(ui.nav().in_sub);
        assert_eq!(ui.nav().sub_sel, Some(1), "WAV, under the Audio title");
        // The submenu is drawn to the left of the menu (no room on the right).
        let (menu_row, wav) = (ui.nav().rows[export], ui.nav().sub_rows[1]);
        assert!(wav.right() < menu_row.x);
        assert!((wav.y - menu_row.y).abs() < 0.5, "WAV level with Export");
        ui.key(Key::Escape);
        assert!(ui.app.popup.is_some(), "Escape closes the submenu only");
        assert_eq!(ui.nav().open, None);
        assert_eq!(ui.nav().sel, Some(export));
        ui.key(Key::Enter);
        assert_eq!(ui.nav().open, Some(export), "Enter opens it too");
        ui.key(Key::Left);
        assert_eq!(ui.nav().open, None);
        ui.key(Key::Escape);
        assert!(ui.app.popup.is_none(), "then Escape closes the menu");
    }

    #[test]
    fn hover_opens_after_a_delay_and_the_triangle_keeps_it_open() {
        let mut ui = Ui::new();
        let export = ui.row("export");
        let (x, y) = ui.nav().rows[export].center();
        ui.point(x, y);
        assert_eq!(ui.nav().open, None, "not at once");
        ui.frames(12);
        assert_eq!(ui.nav().open, Some(export));
        // Diagonally down-left to the last row of the submenu, over other rows of the menu.
        let target = *ui.nav().sub_rows.last().unwrap();
        let (tx, ty) = target.center();
        // Slowly: each row of the menu crossed takes longer than the intent delay.
        let steps = 120;
        for k in 1..=steps {
            let f = k as f32 / steps as f32;
            ui.point(x + (tx - x) * f, y + (ty - y) * f);
            assert_eq!(ui.nav().open, Some(export), "closed at step {k}");
        }
        let last = ui.nav().sub_rows.len() - 1;
        assert_eq!(ui.nav().sub_sel, Some(last));
        // Resting on another row of the menu: its intent wins, the submenu closes.
        let settings = ui.row("theme") + 1;
        let (sx, sy) = ui.nav().rows[settings].center();
        ui.point(sx, sy);
        ui.frames(30);
        assert_eq!(ui.nav().open, None);
        assert_eq!(ui.nav().sel, Some(settings));
    }

    #[test]
    fn clicks_open_submenus_run_their_rows_and_close_outside() {
        let mut ui = Ui::new();
        // The click that opened the menu does not close it (`fresh`).
        ui.app.open_popup(Popup::MainMenu, MENU_BUTTON);
        ui.click(800.0, 500.0);
        assert!(ui.app.popup.is_some());
        ui.i.pressed = false;
        // A click on Theme opens its submenu at once; a click on a theme applies it.
        let theme = ui.row("theme");
        let (x, y) = ui.nav().rows[theme].center();
        ui.click(x, y);
        assert_eq!(ui.nav().open, Some(theme));
        ui.frame();
        let before = ui.app.themes.active;
        let pick = if before == 1 { 2 } else { 1 };
        let (px, py) = ui.nav().sub_rows[pick].center();
        ui.point(px, py);
        ui.click(px, py);
        assert!(ui.app.popup.is_none());
        assert_eq!(ui.app.themes.active, pick);
        // Outside both: everything closes.
        ui.app.open_popup(Popup::MainMenu, MENU_BUTTON);
        ui.frame();
        let (x, y) = ui.nav().rows[ui.row("export")].center();
        ui.click(x, y);
        assert!(ui.nav().open.is_some());
        ui.click(300.0, 600.0);
        assert!(ui.app.popup.is_none());
    }

    #[test]
    fn the_export_lesson_and_the_tour_open_the_export_submenu() {
        let mut ui = Ui::new();
        assert_eq!(ui.nav().open, None);
        ui.app.popup = None;
        ui.app.screenshot_lesson("export:2", None);
        ui.app.open_popup(Popup::MainMenu, MENU_BUTTON);
        ui.frame();
        assert_eq!(ui.nav().open, Some(ui.row("export")));
        assert_eq!(ui.app.menu_guide(), Some("export-wav"));
        assert!(ui.app.export_items().iter().any(|it| it.id == "export-wav"));

        let mut ui = Ui::new();
        ui.app.popup = None;
        ui.app.screenshot_tour(2);
        ui.app.open_popup(Popup::MainMenu, MENU_BUTTON);
        ui.frame();
        assert_eq!(ui.nav().open, Some(ui.row("export")));
    }

    #[test]
    fn moving_away_closes_a_hovered_submenu() {
        let r = rows(&[(true, false), (true, true), (true, false)]);
        let child = |_: usize| rows(&[(true, false)]);
        let mut nav = MenuNav::default();
        nav.open_sub(1, false);
        nav_key(&mut nav, Key::Down, &r, &child);
        assert_eq!((nav.sel, nav.open), (Some(2), None));
    }
}
